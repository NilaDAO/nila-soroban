#![no_std]
// `set_title` carries the attester plus the whole title. The contract macros
// generate client and spec items outside the impl, so the allow is crate-wide.
#![allow(clippy::too_many_arguments)]

//! Stellar mirror of the Polygon land titles (`NilaLandTitle`).
//!
//! There is no member list. As on Polygon (`GenericFundCore` checks
//! `landNFT.balanceOf(holder) > 0`), a member of a union is whoever holds a land
//! title that union minted. The attester (F1) copies each Polygon title here:
//! id, holder, minting union and its record hash (R70, R71).

use nila_common::{bump_instance, Role, RolesClient, TTL_BUMP, TTL_THRESHOLD};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env,
};

contractmeta!(key = "binver", val = "0.2.0");
contractmeta!(
    key = "desc",
    val = "Nila land titles mirrored from Polygon; holding one is membership"
);

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    NotAuthorized = 2,
    NotFound = 3,
    Paused = 4,
    RolesNotSet = 5,
    UpgradeNotReady = 6,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Roles,
    Paused,
    UpgradeDelay,
    PendingDelay,
    PendingUpgrade,
    Title(u64),              // Polygon token id -> title
    Count(Address, Address), // (union, holder) -> titles held from that union
}

/// One land title as minted on Polygon, public (X23).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Title {
    pub holder: Address,
    /// The union whose leader minted the title.
    pub union: Address,
    pub polygon_holder: BytesN<20>,
    pub record_hash: BytesN<32>,
    pub metadata_hash: BytesN<32>,
    pub updated_ts: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub wasm_hash: BytesN<32>,
    pub eta: u64,
}

#[contractevent]
pub struct TitleSet {
    #[topic]
    pub title_id: u64,
    #[topic]
    pub union: Address,
    pub holder: Address,
}

#[contractevent]
pub struct TitleBurned {
    #[topic]
    pub title_id: u64,
    #[topic]
    pub union: Address,
    pub holder: Address,
}

#[contractevent]
pub struct RecordSet {
    #[topic]
    pub title_id: u64,
    pub record_hash: BytesN<32>,
    pub metadata_hash: BytesN<32>,
}

#[contractevent]
pub struct Paused {
    pub paused: bool,
}

#[contractevent]
pub struct UpgradeProposed {
    pub wasm_hash: BytesN<32>,
    pub eta: u64,
}

#[contractevent]
pub struct UpgradeApplied {
    pub wasm_hash: BytesN<32>,
}

#[contract]
pub struct LandTitleContract;

fn require_admin(env: &Env) -> Result<(), Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(())
}

/// Writes are the attester's (one role list in `roles`, R4) and stop while paused.
fn require_attester(env: &Env, attester: &Address) -> Result<(), Error> {
    if env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        return Err(Error::Paused);
    }
    attester.require_auth();
    let roles: Address = env
        .storage()
        .instance()
        .get(&DataKey::Roles)
        .ok_or(Error::RolesNotSet)?;
    if !RolesClient::new(env, &roles).has_role(attester, &Role::Attester) {
        return Err(Error::NotAuthorized);
    }
    Ok(())
}

fn save<V: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(env: &Env, key: &DataKey, value: &V) {
    env.storage().persistent().set(key, value);
    env.storage()
        .persistent()
        .extend_ttl(key, TTL_THRESHOLD, TTL_BUMP);
}

fn count(env: &Env, union: &Address, holder: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::Count(union.clone(), holder.clone()))
        .unwrap_or(0)
}

fn add_count(env: &Env, union: &Address, holder: &Address, up: bool) {
    let key = DataKey::Count(union.clone(), holder.clone());
    let n = count(env, union, holder);
    let next = if up { n + 1 } else { n.saturating_sub(1) };
    if next == 0 {
        env.storage().persistent().remove(&key);
    } else {
        save(env, &key, &next);
    }
}

fn load_title(env: &Env, title_id: u64) -> Result<Title, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Title(title_id))
        .ok_or(Error::NotFound)
}

#[contractimpl]
impl LandTitleContract {
    pub fn __constructor(env: Env, admin: Address, roles: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Roles, &roles);
        env.storage().instance().set(&DataKey::Paused, &false);
        bump_instance(&env);
    }

    /// Copy of a Polygon title (F1): a mint, or a move by union + attester (C7).
    pub fn set_title(
        env: Env,
        title_id: u64,
        attester: Address,
        holder: Address,
        union: Address,
        polygon_holder: BytesN<20>,
        record_hash: BytesN<32>,
        metadata_hash: BytesN<32>,
    ) -> Result<(), Error> {
        require_attester(&env, &attester)?;
        let key = DataKey::Title(title_id);
        if let Some(old) = env.storage().persistent().get::<_, Title>(&key) {
            add_count(&env, &old.union, &old.holder, false);
        }
        add_count(&env, &union, &holder, true);
        let t = Title {
            holder: holder.clone(),
            union: union.clone(),
            polygon_holder,
            record_hash,
            metadata_hash,
            updated_ts: env.ledger().timestamp(),
        };
        save(&env, &key, &t);
        TitleSet {
            title_id,
            union,
            holder,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Record and metadata hash of a title already copied (R70, R71).
    pub fn set_record(
        env: Env,
        title_id: u64,
        attester: Address,
        record_hash: BytesN<32>,
        metadata_hash: BytesN<32>,
    ) -> Result<(), Error> {
        require_attester(&env, &attester)?;
        let mut t = load_title(&env, title_id)?;
        t.record_hash = record_hash.clone();
        t.metadata_hash = metadata_hash.clone();
        t.updated_ts = env.ledger().timestamp();
        save(&env, &DataKey::Title(title_id), &t);
        RecordSet {
            title_id,
            record_hash,
            metadata_hash,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Copy of a Polygon burn (A5). Her last title of a union ends her membership there.
    pub fn burn_title(env: Env, title_id: u64, attester: Address) -> Result<(), Error> {
        require_attester(&env, &attester)?;
        let t = load_title(&env, title_id)?;
        add_count(&env, &t.union, &t.holder, false);
        env.storage().persistent().remove(&DataKey::Title(title_id));
        TitleBurned {
            title_id,
            union: t.union,
            holder: t.holder,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Membership: holds at least one title minted by `union`. Read by fund_core.
    /// A read keeps the entry alive: a holder who is only read is never written again.
    pub fn holds_title(env: Env, union: Address, account: Address) -> bool {
        let key = DataKey::Count(union, account);
        if !env.storage().persistent().has(&key) {
            return false;
        }
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_BUMP);
        true
    }

    pub fn title_count(env: Env, union: Address, account: Address) -> u32 {
        count(&env, &union, &account)
    }

    pub fn title(env: Env, title_id: u64) -> Option<Title> {
        env.storage().persistent().get(&DataKey::Title(title_id))
    }

    pub fn roles(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Roles).unwrap()
    }

    pub fn admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    // ---------- Pause and upgrade: the admin (trust multisig) ----------

    pub fn pause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        Paused { paused: true }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn unpause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        Paused { paused: false }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    /// Raising the delay applies at once. Lowering it waits out the current delay: the first
    /// call files the change, a second call after the delay applies it.
    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        require_admin(&env)?;
        let current: u64 = env
            .storage()
            .instance()
            .get(&DataKey::UpgradeDelay)
            .unwrap_or(0);
        if delay_seconds < current {
            let now = env.ledger().timestamp();
            match env
                .storage()
                .instance()
                .get::<_, (u64, u64)>(&DataKey::PendingDelay)
            {
                Some((d, eta)) if d == delay_seconds && now >= eta => {
                    env.storage().instance().remove(&DataKey::PendingDelay);
                }
                _ => {
                    env.storage().instance().set(
                        &DataKey::PendingDelay,
                        &(delay_seconds, now.saturating_add(current)),
                    );
                    bump_instance(&env);
                    return Ok(());
                }
            }
        }
        env.storage()
            .instance()
            .set(&DataKey::UpgradeDelay, &delay_seconds);
        bump_instance(&env);
        Ok(())
    }

    pub fn upgrade_delay(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::UpgradeDelay)
            .unwrap_or(0)
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        require_admin(&env)?;
        let delay: u64 = env
            .storage()
            .instance()
            .get(&DataKey::UpgradeDelay)
            .unwrap_or(0);
        if delay == 0 {
            env.deployer()
                .update_current_contract_wasm(new_wasm_hash.clone());
            UpgradeApplied {
                wasm_hash: new_wasm_hash,
            }
            .publish(&env);
        } else {
            let eta = env.ledger().timestamp().saturating_add(delay);
            env.storage().instance().set(
                &DataKey::PendingUpgrade,
                &PendingUpgrade {
                    wasm_hash: new_wasm_hash.clone(),
                    eta,
                },
            );
            UpgradeProposed {
                wasm_hash: new_wasm_hash,
                eta,
            }
            .publish(&env);
        }
        bump_instance(&env);
        Ok(())
    }

    pub fn execute_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        let pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .ok_or(Error::UpgradeNotReady)?;
        if env.ledger().timestamp() < pending.eta {
            return Err(Error::UpgradeNotReady);
        }
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        env.deployer()
            .update_current_contract_wasm(pending.wasm_hash.clone());
        UpgradeApplied {
            wasm_hash: pending.wasm_hash,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn cancel_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        bump_instance(&env);
        Ok(())
    }
}

#[cfg(test)]
mod test;
