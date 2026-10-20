#![no_std]

use nila_common::{bump_instance, upgrade};
use nila_math::preview_rate_bp;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env, String,
};

contractmeta!(key = "binver", val = "0.1.0");

// Demo builds only (feature `demo-upgrade-delay`): the upgrade timelock is 60 seconds, so a demo deployment can be
// upgraded in place and keep its state. Never for mainnet: the wasm carries this marker, which deploys refuse.
#[cfg(feature = "demo-upgrade-delay")]
soroban_sdk::contractmeta!(
    key = "demo_upgrade_delay",
    val = "demo-upgrade-delay: 60 s, never mainnet"
);
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
    UpgradeNotReady = 5,
    /// Above the 30-day maximum.
    BadDelay = 6,
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

/// Upgrade timelock at deploy: 48 hours (C15).
#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = upgrade::DEFAULT_DELAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;
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
        upgrade::init(&env, UPGRADE_DELAY);
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

    // ---------- upgrade (admin, 48-hour timelock, nila_common::upgrade) ----------

    /// Raising applies at once and voids a filed lowering; lowering applies only when set again
    /// after the current delay, within 7 days; at most 30 days.
    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        require_admin(&env)?;
        if !upgrade::set_delay(&env, delay_seconds) {
            return Err(Error::BadDelay);
        }
        bump_instance(&env);
        Ok(())
    }

    pub fn admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    pub fn upgrade_delay(env: Env) -> u64 {
        upgrade::delay(&env)
    }

    pub fn pending_upgrade(env: Env) -> Option<upgrade::PendingUpgrade> {
        upgrade::pending(&env)
    }

    /// A filed lowering of the delay, if any.
    pub fn pending_delay(env: Env) -> Option<upgrade::PendingDelay> {
        upgrade::pending_delay(&env)
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        require_admin(&env)?;
        upgrade::propose(&env, new_wasm_hash);
        bump_instance(&env);
        Ok(())
    }

    pub fn execute_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        if !upgrade::execute(&env) {
            return Err(Error::UpgradeNotReady);
        }
        Ok(())
    }

    pub fn cancel_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        upgrade::cancel(&env);
        bump_instance(&env);
        Ok(())
    }
}

#[cfg(test)]
mod test;
