#![no_std]

pub use nila_common::{Role, UnionRole};

use nila_common::bump_instance;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env,
};

contractmeta!(key = "binver", val = "0.1.0");
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
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Role(Address, Role),                    // (account, role) — trust-wide
    Leader(Address, Address),               // (union, account)
    UnionRole(Address, Address, UnionRole), // (union, account, role)
    PendingUpgrade,
    UpgradeDelay,
    UpgradeEta,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub wasm_hash: BytesN<32>,
    pub eta: u64,
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
        env.storage().instance().set(&DataKey::UpgradeDelay, &0u64);
        bump_instance(&env);
    }

    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
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

    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage()
            .instance()
            .set(&DataKey::UpgradeDelay, &delay_seconds);
        bump_instance(&env);
        Ok(())
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
            .ok_or(Error::NotAdmin)?;
        if env.ledger().timestamp() < pending.eta {
            return Err(Error::NotAdmin);
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
