#![no_std]

use nila_common::{bump_instance, MemberRecord, RolesClient};
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
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Attester,
    Roles,
    Member(Address, Address), // (union, account)
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

#[contract]
pub struct MembershipContract;

fn require_admin(env: &Env) -> Result<Address, Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(admin)
}

#[contractimpl]
impl MembershipContract {
    pub fn __constructor(env: Env, admin: Address, attester: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Attester, &attester);
        bump_instance(&env);
    }

    pub fn set_roles(env: Env, roles: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Roles, &roles);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_attester(env: Env, attester: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Attester, &attester);
        bump_instance(&env);
        Ok(())
    }

    /// Two-signature confirmation: union leader + Nila attester.
    /// The Polygon title enters as an attested fact, not a proof (TAD §4.2).
    pub fn confirm_member(
        env: Env,
        union: Address,
        member: Address,
        leader: Address,
        polygon_address: BytesN<20>,
        plot_ids: Vec<u64>,
        cycle: u32,
    ) -> Result<(), Error> {
        if plot_ids.len() > MAX_PLOTS {
            return Err(Error::TooManyPlots);
        }

        leader.require_auth();
        let attester: Address = env.storage().instance().get(&DataKey::Attester).unwrap();
        attester.require_auth();

        let roles_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Roles)
            .ok_or(Error::RolesNotSet)?;
        let roles = RolesClient::new(&env, &roles_addr);
        if !roles.is_leader(&union, &leader) {
            return Err(Error::NotAuthorized);
        }

        let rec = MemberRecord {
            union: union.clone(),
            member: member.clone(),
            polygon_address,
            plot_ids: plot_ids.clone(),
            cycle,
            confirmed_ts: env.ledger().timestamp(),
            active: true,
        };
        let key = DataKey::Member(union.clone(), member.clone());
        env.storage().persistent().set(&key, &rec);
        env.storage().persistent().extend_ttl(
            &key,
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
        );

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

    pub fn revoke_member(
        env: Env,
        union: Address,
        member: Address,
        leader: Address,
    ) -> Result<(), Error> {
        leader.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let roles_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Roles)
            .ok_or(Error::RolesNotSet)?;
        let roles = RolesClient::new(&env, &roles_addr);
        if leader != admin && !roles.is_leader(&union, &leader) {
            return Err(Error::NotAuthorized);
        }

        let key = DataKey::Member(union.clone(), member.clone());
        let mut rec: MemberRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::NotMember)?;
        rec.active = false;
        env.storage().persistent().set(&key, &rec);

        MemberRevoked { union, member }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn is_member(env: Env, union: Address, account: Address) -> bool {
        let rec: Option<MemberRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Member(union, account));
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

    pub fn attester(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Attester).unwrap()
    }
}

#[cfg(test)]
mod test;
