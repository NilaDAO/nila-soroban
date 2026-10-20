#![no_std]

pub use nila_common::{Role, UnionRole};

use nila_common::{bump_instance, upgrade};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env,
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
    val = "Nila role registry: leaders, oracle, attester, core, union officers"
);

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotAdmin = 2,
    ZeroAddress = 3,
    UpgradeNotReady = 4,
    /// Above the 30-day maximum.
    BadDelay = 5,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Role(Address, Role),                    // (account, role) — trust-wide
    Leader(Address, Address),               // (union, account)
    UnionRole(Address, Address, UnionRole), // (union, account, role)
}

#[contractevent]
pub struct RoleUpdated {
    #[topic]
    pub account: Address,
    #[topic]
    pub role: Role,
    pub enabled: bool,
}

#[contractevent]
pub struct UnionRoleUpdated {
    #[topic]
    pub union: Address,
    #[topic]
    pub account: Address,
    pub role: UnionRole,
    pub enabled: bool,
}

#[contractevent]
pub struct LeaderUpdated {
    #[topic]
    pub union: Address,
    #[topic]
    pub account: Address,
    pub enabled: bool,
}

/// The trust's admin account changed (C19).
#[contractevent]
pub struct AdminSet {
    pub admin: Address,
}

/// Upgrade timelock at deploy: 48 hours (C15).
#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = upgrade::DEFAULT_DELAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;
#[contract]
pub struct RolesContract;

fn require_admin(env: &Env) -> Result<Address, Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(admin)
}

/// A revoked flag is removed rather than stored as false, so it costs no rent.
fn store_flag(env: &Env, key: &DataKey, enabled: bool) {
    let store = env.storage().persistent();
    if enabled {
        store.set(key, &true);
        store.extend_ttl(key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
    } else {
        store.remove(key);
    }
}

/// A set flag is extended on every read: a leader appointed once and only checked
/// afterwards must not be archived after 120 days.
fn read_flag(env: &Env, key: &DataKey) -> bool {
    let store = env.storage().persistent();
    if store.has(key) {
        store.extend_ttl(key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
        true
    } else {
        false
    }
}

#[contractimpl]
impl RolesContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        upgrade::init(&env, UPGRADE_DELAY);
        bump_instance(&env);
    }

    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        AdminSet { admin: new_admin }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Trust-wide role (oracle, attester, core). Admin only (R4, R75).
    pub fn set_role(env: Env, account: Address, role: Role, enabled: bool) -> Result<(), Error> {
        require_admin(&env)?;
        store_flag(&env, &DataKey::Role(account.clone(), role), enabled);
        RoleUpdated {
            account,
            role,
            enabled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Leaders of a union, all equal. Signed by the union account itself, whose
    /// 5 signers / threshold 3 make `require_auth` the 3 of 5 (D12, R4, R5).
    pub fn set_leader(
        env: Env,
        union: Address,
        account: Address,
        enabled: bool,
    ) -> Result<(), Error> {
        union.require_auth();
        store_flag(
            &env,
            &DataKey::Leader(union.clone(), account.clone()),
            enabled,
        );
        LeaderUpdated {
            union,
            account,
            enabled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Union officers (treasurer, secretary). Signed by the union account (D12, R4).
    pub fn set_union_role(
        env: Env,
        union: Address,
        account: Address,
        role: UnionRole,
        enabled: bool,
    ) -> Result<(), Error> {
        union.require_auth();
        store_flag(
            &env,
            &DataKey::UnionRole(union.clone(), account.clone(), role),
            enabled,
        );
        UnionRoleUpdated {
            union,
            account,
            role,
            enabled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn has_role(env: Env, account: Address, role: Role) -> bool {
        read_flag(&env, &DataKey::Role(account, role))
    }

    pub fn has_union_role(env: Env, union: Address, account: Address, role: UnionRole) -> bool {
        read_flag(&env, &DataKey::UnionRole(union, account, role))
    }

    /// Kept for fund_core's `RolesClient`; same as `has_role(account, Oracle)`.
    pub fn is_oracle(env: Env, account: Address) -> bool {
        read_flag(&env, &DataKey::Role(account, Role::Oracle))
    }

    pub fn is_leader(env: Env, union: Address, account: Address) -> bool {
        read_flag(&env, &DataKey::Leader(union, account))
    }

    pub fn admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
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

    /// Files the new wasm; `execute_upgrade` applies it once the delay has passed.
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
