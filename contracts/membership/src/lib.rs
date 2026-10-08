#![no_std]
// `confirm_member` carries both signers plus the entry (R1). The contract macros
// generate client and spec items outside the impl, so the allow is crate-wide.
#![allow(clippy::too_many_arguments)]

use nila_common::{bump_instance, MemberRecord, Role, RolesClient};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, vec, Address,
    BytesN, Env, Vec,
};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(
    key = "desc",
    val = "Union membership + attested Polygon land-title facts"
);

const MAX_PLOTS: u32 = 32;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    NotAuthorized = 2,
    NotMember = 3,
    AlreadyMember = 4,
    TooManyPlots = 5,
    Inactive = 6,
    RolesNotSet = 7,
    StaleCycle = 8,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Roles,
    Member(Address, Address), // (union, account)
    MemberUnion(Address),     // account -> union of her active entry
    Record(u64),              // land title id -> record
}

/// Public record of one land title (X23): the record hash moves here from
/// `NilaLandTitle.setRecordHash` (R71); the metadata hash drives the member list sync (R70).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleRecord {
    pub record_hash: BytesN<32>,
    pub metadata_hash: BytesN<32>,
    pub updated_ts: u64,
}

#[contractevent]
pub struct MemberConfirmed {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub cycle: u32,
    pub plot_count: u32,
}

#[contractevent]
pub struct MemberRevoked {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
}

#[contractevent]
pub struct PlotsSet {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub plot_count: u32,
}

#[contractevent]
pub struct MemberReattested {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub cycle: u32,
}

#[contractevent]
pub struct RecordSet {
    #[topic]
    pub title_id: u64,
    pub record_hash: BytesN<32>,
    pub metadata_hash: BytesN<32>,
}

#[contract]
pub struct MembershipContract;

fn roles(env: &Env) -> Result<RolesClient<'_>, Error> {
    let addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Roles)
        .ok_or(Error::RolesNotSet)?;
    Ok(RolesClient::new(env, &addr))
}

fn require_leader(env: &Env, union: &Address, leader: &Address) -> Result<(), Error> {
    leader.require_auth();
    if !roles(env)?.is_leader(union, leader) {
        return Err(Error::NotAuthorized);
    }
    Ok(())
}

/// The attester comes from the one role list in `roles` (R4), not a local slot.
fn require_attester(env: &Env, attester: &Address) -> Result<(), Error> {
    attester.require_auth();
    if !roles(env)?.has_role(attester, &Role::Attester) {
        return Err(Error::NotAuthorized);
    }
    Ok(())
}

fn check_plots(plot_ids: &Vec<u64>) -> Result<(), Error> {
    if plot_ids.len() > MAX_PLOTS {
        return Err(Error::TooManyPlots);
    }
    Ok(())
}

fn active_record(env: &Env, union: &Address, member: &Address) -> Result<MemberRecord, Error> {
    let rec: MemberRecord = env
        .storage()
        .persistent()
        .get(&DataKey::Member(union.clone(), member.clone()))
        .ok_or(Error::NotMember)?;
    if !rec.active {
        return Err(Error::Inactive);
    }
    Ok(rec)
}

fn save<V: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(env: &Env, key: &DataKey, value: &V) {
    env.storage().persistent().set(key, value);
    env.storage()
        .persistent()
        .extend_ttl(key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
}

/// Writes the entry and, while it is active, keeps the `MemberUnion` index alive with it.
fn save_record(env: &Env, rec: &MemberRecord) {
    save(
        env,
        &DataKey::Member(rec.union.clone(), rec.member.clone()),
        rec,
    );
    if rec.active {
        save(env, &DataKey::MemberUnion(rec.member.clone()), &rec.union);
    }
}

/// Reads keep live entries alive: a member who is only read (gated by fund_core) is
/// never written again, and an archived entry would read as "not a member".
fn bump(env: &Env, key: &DataKey) {
    if env.storage().persistent().has(key) {
        env.storage().persistent().extend_ttl(
            key,
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
        );
    }
}

#[contractimpl]
impl MembershipContract {
    pub fn __constructor(env: Env, admin: Address, roles: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Roles, &roles);
        bump_instance(&env);
    }

    /// Two-signature confirmation: a leader of the union + an attester (R1).
    /// The Polygon title enters as an attested fact, not a proof (TAD §4.2).
    /// A member holds one active entry at a time; a revoked entry can be confirmed again.
    pub fn confirm_member(
        env: Env,
        union: Address,
        member: Address,
        leader: Address,
        attester: Address,
        polygon_address: BytesN<20>,
        plot_ids: Vec<u64>,
        cycle: u32,
    ) -> Result<(), Error> {
        check_plots(&plot_ids)?;
        // Two signatures means two keys: a leader who is also the attester is one.
        if leader == attester {
            return Err(Error::NotAuthorized);
        }
        require_leader(&env, &union, &leader)?;
        require_attester(&env, &attester)?;

        let union_key = DataKey::MemberUnion(member.clone());
        if env.storage().persistent().has(&union_key) {
            return Err(Error::AlreadyMember);
        }

        let now = env.ledger().timestamp();
        let rec = MemberRecord {
            union: union.clone(),
            member: member.clone(),
            polygon_address,
            plot_ids: plot_ids.clone(),
            cycle,
            confirmed_ts: now,
            attested_ts: now,
            active: true,
        };
        save_record(&env, &rec);

        MemberConfirmed {
            union,
            member,
            cycle,
            plot_count: plot_ids.len(),
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Plot list after a title burn (A5) or a union transfer. Attester only (R3).
    pub fn set_plots(
        env: Env,
        union: Address,
        member: Address,
        attester: Address,
        plot_ids: Vec<u64>,
    ) -> Result<(), Error> {
        check_plots(&plot_ids)?;
        require_attester(&env, &attester)?;
        let mut rec = active_record(&env, &union, &member)?;
        rec.plot_ids = plot_ids.clone();
        rec.attested_ts = env.ledger().timestamp();
        save_record(&env, &rec);

        PlotsSet {
            union,
            member,
            plot_count: plot_ids.len(),
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Per-cycle re-attestation (R1). The cycle only moves forward; gaps are allowed
    /// (a missed season), so a mistyped high cycle needs a revoke and re-confirm.
    pub fn reattest(
        env: Env,
        union: Address,
        member: Address,
        attester: Address,
        cycle: u32,
    ) -> Result<(), Error> {
        require_attester(&env, &attester)?;
        let mut rec = active_record(&env, &union, &member)?;
        if cycle <= rec.cycle {
            return Err(Error::StaleCycle);
        }
        rec.cycle = cycle;
        rec.attested_ts = env.ledger().timestamp();
        save_record(&env, &rec);

        MemberReattested {
            union,
            member,
            cycle,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Revoke by a leader of the union (R3). No admin override (R5).
    pub fn revoke_member(
        env: Env,
        union: Address,
        member: Address,
        leader: Address,
    ) -> Result<(), Error> {
        require_leader(&env, &union, &leader)?;
        let mut rec = active_record(&env, &union, &member)?;
        rec.active = false;
        save_record(&env, &rec);
        env.storage()
            .persistent()
            .remove(&DataKey::MemberUnion(member.clone()));

        MemberRevoked { union, member }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Record and metadata hash of a land title (R70, R71). Attester only; readable by all (X23).
    pub fn set_record(
        env: Env,
        title_id: u64,
        attester: Address,
        record_hash: BytesN<32>,
        metadata_hash: BytesN<32>,
    ) -> Result<(), Error> {
        require_attester(&env, &attester)?;
        let rec = TitleRecord {
            record_hash: record_hash.clone(),
            metadata_hash: metadata_hash.clone(),
            updated_ts: env.ledger().timestamp(),
        };
        save(&env, &DataKey::Record(title_id), &rec);

        RecordSet {
            title_id,
            record_hash,
            metadata_hash,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn is_member(env: Env, union: Address, account: Address) -> bool {
        let key = DataKey::Member(union, account);
        bump(&env, &key);
        let rec: Option<MemberRecord> = env.storage().persistent().get(&key);
        rec.map(|r| r.active).unwrap_or(false)
    }

    pub fn plots_of(env: Env, union: Address, account: Address) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::Member(union, account))
            .map(
                |r: MemberRecord| {
                    if r.active {
                        r.plot_ids
                    } else {
                        vec![&env]
                    }
                },
            )
            .unwrap_or_else(|| vec![&env])
    }

    pub fn get_member(env: Env, union: Address, account: Address) -> Option<MemberRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::Member(union, account))
    }

    /// Union of the account's active entry (R2).
    pub fn union_of(env: Env, account: Address) -> Option<Address> {
        let key = DataKey::MemberUnion(account);
        bump(&env, &key);
        env.storage().persistent().get(&key)
    }

    pub fn record_of(env: Env, title_id: u64) -> Option<TitleRecord> {
        env.storage().persistent().get(&DataKey::Record(title_id))
    }

    pub fn roles(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Roles).unwrap()
    }
}

#[cfg(test)]
mod test;
