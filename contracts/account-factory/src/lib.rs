#![no_std]
//! The account factory, one per union (executive decision 26 Oct): it deploys the union's
//! Nila accounts (members, leaders, machine owners, LPs; nila-account) and holds the settings
//! they read. No master key over all unions: its admin is the union's own account, a Stellar
//! multisig (3 of 5, D12), which also names the union's leaders in roles. The admin sets the
//! recovery key (the node's key that proposes a new signer after its SMS check), the recovery
//! wait, the session length and the contracts a leader session reaches; a new account code
//! and the factory's own code take effect only after the timelock (nila_common::upgrade).
//! The node creates accounts (the recovery key authorizes `create`), at registration and L14.

use nila_common::{bump_instance, upgrade};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env, Vec,
};

contractmeta!(key = "binver", val = "0.1.0");
#[cfg(feature = "demo-upgrade-delay")]
soroban_sdk::contractmeta!(
    key = "demo_upgrade_delay",
    val = "demo-upgrade-delay: 60 s, never mainnet"
);
contractmeta!(
    key = "desc",
    val = "Nila account factory: one per union, governed by the union's multisig"
);

#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = upgrade::DEFAULT_DELAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;
/// Bounds of the settings: the recovery wait (1 hour – 30 days) and a session (1–15 minutes).
pub const WAIT_MIN: u64 = 3_600;
pub const WAIT_MAX: u64 = 30 * 86_400;
pub const SESSION_MIN: u64 = 60;
pub const SESSION_MAX: u64 = 900;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    UpgradeNotReady = 1,
    BadDelay = 2,
    /// A wait or session length outside its bounds.
    BadSetting = 3,
    /// No account code proposed, or its timelock has not passed.
    NotReady = 4,
}

/// The account's signer, as nila-account takes it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Signer {
    P256(BytesN<65>),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingWasm {
    pub hash: BytesN<32>,
    pub at: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Union, // admin: the union's multisig account
    Roles,
    Recovery,
    Wait,
    SessionLen,
    SessionContracts,
    AccountWasm,
    PendingWasm,
}

#[contractevent]
pub struct AccountCreated {
    #[topic]
    pub account: Address,
    pub signer: Signer,
}

#[contractevent]
pub struct AccountWasmProposed {
    pub hash: BytesN<32>,
    pub at: u64,
}

fn get<V: soroban_sdk::TryFromVal<Env, soroban_sdk::Val>>(env: &Env, k: DataKey) -> V {
    env.storage().instance().get(&k).unwrap()
}

fn require_union(env: &Env) {
    get::<Address>(env, DataKey::Union).require_auth();
}

fn salt(env: &Env, signer: &Signer) -> BytesN<32> {
    let Signer::P256(pk) = signer;
    env.crypto().sha256(&pk.clone().into()).into()
}

#[contract]
pub struct AccountFactory;

#[contractimpl]
impl AccountFactory {
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        env: Env,
        union: Address,
        roles: Address,
        recovery: Address,
        wait: u64,
        session_len: u64,
        session_contracts: Vec<Address>,
        account_wasm: BytesN<32>,
    ) {
        let s = env.storage().instance();
        s.set(&DataKey::Union, &union);
        s.set(&DataKey::Roles, &roles);
        s.set(&DataKey::Recovery, &recovery);
        s.set(&DataKey::Wait, &wait.clamp(WAIT_MIN, WAIT_MAX));
        s.set(
            &DataKey::SessionLen,
            &session_len.clamp(SESSION_MIN, SESSION_MAX),
        );
        s.set(&DataKey::SessionContracts, &session_contracts);
        s.set(&DataKey::AccountWasm, &account_wasm);
        upgrade::init(&env, UPGRADE_DELAY);
        bump_instance(&env);
    }

    /// The node (the recovery key) deploys an account for `signer`; its address follows from
    /// the key (`address_of`), so one key has one account.
    pub fn create(env: Env, signer: Signer) -> Address {
        get::<Address>(&env, DataKey::Recovery).require_auth();
        let wasm: BytesN<32> = get(&env, DataKey::AccountWasm);
        let union: Address = get(&env, DataKey::Union);
        let account = env
            .deployer()
            .with_current_contract(salt(&env, &signer))
            .deploy_v2(
                wasm,
                (env.current_contract_address(), union, signer.clone()),
            );
        AccountCreated {
            account: account.clone(),
            signer,
        }
        .publish(&env);
        bump_instance(&env);
        account
    }

    pub fn address_of(env: Env, signer: Signer) -> Address {
        env.deployer()
            .with_current_contract(salt(&env, &signer))
            .deployed_address()
    }

    // ---------- settings (the union's multisig) ----------

    pub fn set_recovery(env: Env, recovery: Address) {
        require_union(&env);
        env.storage().instance().set(&DataKey::Recovery, &recovery);
        bump_instance(&env);
    }

    pub fn set_wait(env: Env, wait: u64) -> Result<(), Error> {
        require_union(&env);
        if !(WAIT_MIN..=WAIT_MAX).contains(&wait) {
            return Err(Error::BadSetting);
        }
        env.storage().instance().set(&DataKey::Wait, &wait);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_session(env: Env, len: u64, contracts: Vec<Address>) -> Result<(), Error> {
        require_union(&env);
        if !(SESSION_MIN..=SESSION_MAX).contains(&len) {
            return Err(Error::BadSetting);
        }
        env.storage().instance().set(&DataKey::SessionLen, &len);
        env.storage()
            .instance()
            .set(&DataKey::SessionContracts, &contracts);
        bump_instance(&env);
        Ok(())
    }

    /// A new account code: each account moves to it with its own `upgrade` once the timelock
    /// has passed and it is applied here.
    pub fn propose_account_wasm(env: Env, hash: BytesN<32>) {
        require_union(&env);
        let at = env.ledger().timestamp() + upgrade::delay(&env);
        env.storage().instance().set(
            &DataKey::PendingWasm,
            &PendingWasm {
                hash: hash.clone(),
                at,
            },
        );
        AccountWasmProposed { hash, at }.publish(&env);
        bump_instance(&env);
    }

    pub fn apply_account_wasm(env: Env) -> Result<(), Error> {
        require_union(&env);
        let p: PendingWasm = env
            .storage()
            .instance()
            .get(&DataKey::PendingWasm)
            .ok_or(Error::NotReady)?;
        if env.ledger().timestamp() < p.at {
            return Err(Error::NotReady);
        }
        env.storage().instance().set(&DataKey::AccountWasm, &p.hash);
        env.storage().instance().remove(&DataKey::PendingWasm);
        bump_instance(&env);
        Ok(())
    }

    // ---------- what the accounts read ----------

    pub fn union(env: Env) -> Address {
        get(&env, DataKey::Union)
    }
    pub fn roles(env: Env) -> Address {
        get(&env, DataKey::Roles)
    }
    pub fn recovery(env: Env) -> Address {
        get(&env, DataKey::Recovery)
    }
    pub fn wait(env: Env) -> u64 {
        get(&env, DataKey::Wait)
    }
    pub fn session_len(env: Env) -> u64 {
        get(&env, DataKey::SessionLen)
    }
    pub fn session_contracts(env: Env) -> Vec<Address> {
        get(&env, DataKey::SessionContracts)
    }
    pub fn account_wasm(env: Env) -> BytesN<32> {
        get(&env, DataKey::AccountWasm)
    }
    pub fn pending_account_wasm(env: Env) -> Option<PendingWasm> {
        env.storage().instance().get(&DataKey::PendingWasm)
    }

    // ---------- the factory's own upgrade (the union's multisig, timelock) ----------

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        require_union(&env);
        upgrade::propose(&env, new_wasm_hash);
        bump_instance(&env);
    }

    pub fn execute_upgrade(env: Env) -> Result<(), Error> {
        require_union(&env);
        if !upgrade::execute(&env) {
            return Err(Error::UpgradeNotReady);
        }
        Ok(())
    }

    pub fn cancel_upgrade(env: Env) {
        require_union(&env);
        upgrade::cancel(&env);
        bump_instance(&env);
    }

    pub fn upgrade_delay(env: Env) -> u64 {
        upgrade::delay(&env)
    }
}

#[cfg(test)]
mod test;
