#![no_std]

use nila_common::bump_instance;
use nila_math::preview_rate_bp;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address, Env,
    String,
};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(
    key = "desc",
    val = "Nila union registry; one pool per union"
);

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    NotFound = 3,
    AlreadyExists = 4,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Union(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionRecord {
    pub id: Address,
    pub display_name: String,
    pub active: bool,
    pub created_ts: u64,
}

#[contractevent]
pub struct UnionCreated {
    #[topic]
    pub union: Address,
}

#[contract]
pub struct FundViewerContract;

fn require_admin(env: &Env) -> Result<(), Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(())
}

#[contractimpl]
impl FundViewerContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        bump_instance(&env);
    }

    pub fn create_union(env: Env, union: Address, display_name: String) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::Union(union.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::AlreadyExists);
        }
        let rec = UnionRecord {
            id: union.clone(),
            display_name,
            active: true,
            created_ts: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&key, &rec);
        env.storage().persistent().extend_ttl(
            &key,
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
        );
        UnionCreated { union }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn activate_union(env: Env, union: Address, active: bool) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::Union(union);
        let mut rec: UnionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::NotFound)?;
        rec.active = active;
        env.storage().persistent().set(&key, &rec);
        bump_instance(&env);
        Ok(())
    }

    pub fn get_union(env: Env, union: Address) -> Option<UnionRecord> {
        env.storage().persistent().get(&DataKey::Union(union))
    }

    pub fn preview_rate_bp(env: Env, params: nila_math::RateParams, util_bp: u32) -> u32 {
        let _ = env;
        preview_rate_bp(&params, util_bp)
    }
}

#[cfg(test)]
mod test;
