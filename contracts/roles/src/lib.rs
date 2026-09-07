#![no_std]

use nila_common::bump_instance;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env,
};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(key = "desc", val = "Nila oracle/leader registry");

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
    Oracle(Address),
    Leader(Address, Address), // (union, account)
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
pub struct OracleUpdated {
    #[topic]
    pub account: Address,
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

    pub fn set_oracle(env: Env, account: Address, enabled: bool) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage()
            .persistent()
            .set(&DataKey::Oracle(account.clone()), &enabled);
        env.storage().persistent().extend_ttl(
            &DataKey::Oracle(account.clone()),
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
        );
        OracleUpdated { account, enabled }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_leader(
        env: Env,
        union: Address,
        account: Address,
        enabled: bool,
    ) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::Leader(union.clone(), account.clone());
        env.storage().persistent().set(&key, &enabled);
        env.storage().persistent().extend_ttl(
            &key,
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
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

    pub fn is_oracle(env: Env, account: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Oracle(account))
            .unwrap_or(false)
    }

    pub fn is_leader(env: Env, union: Address, account: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Leader(union, account))
            .unwrap_or(false)
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
