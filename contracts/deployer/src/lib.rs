#![no_std]
//! The account deployer (executive decision 3 Nov: member-owned accounts; replaces the per-union
//! account factory). One for all unions and immutable: no admin, no settings, no upgrade. It
//! deploys a Nila account (nila-account) for a signer key at the address that follows from that
//! key alone (`address_of`), with no union: the member names her union on the account itself
//! (`set_union`), and the union's settings live in roles. The account code is fixed here at
//! deploy; a caller-chosen code would let anyone squat a member's address with code of their own.
//! Anyone may call `create` (the member's app, a leader's app at L14): it grants nothing
//! (membership is the land title, leadership is roles).

use nila_common::bump_instance;
use soroban_sdk::{
    contract, contractevent, contractimpl, contractmeta, contracttype, Address, BytesN, Env,
};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(
    key = "desc",
    val = "Nila account deployer: one for all unions, immutable, address from the key"
);

/// The account's signer, as nila-account takes it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Signer {
    Ed25519(BytesN<32>),
    P256(BytesN<65>),
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    AccountWasm,
    Roles,
}

#[contractevent]
pub struct AccountCreated {
    #[topic]
    pub account: Address,
    pub signer: Signer,
}

fn salt(env: &Env, signer: &Signer) -> BytesN<32> {
    let key: soroban_sdk::Bytes = match signer {
        Signer::Ed25519(pk) => pk.clone().into(),
        Signer::P256(pk) => pk.clone().into(),
    };
    env.crypto().sha256(&key).into()
}

#[contract]
pub struct Deployer;

#[contractimpl]
impl Deployer {
    pub fn __constructor(env: Env, account_wasm: BytesN<32>, roles: Address) {
        let s = env.storage().instance();
        s.set(&DataKey::AccountWasm, &account_wasm);
        s.set(&DataKey::Roles, &roles);
        bump_instance(&env);
    }

    /// Anyone deploys the account of `signer`; its address follows from the key
    /// (`address_of`), so one key has one account.
    pub fn create(env: Env, signer: Signer) -> Address {
        let wasm: BytesN<32> = env.storage().instance().get(&DataKey::AccountWasm).unwrap();
        let roles: Address = env.storage().instance().get(&DataKey::Roles).unwrap();
        let account = env
            .deployer()
            .with_current_contract(salt(&env, &signer))
            .deploy_v2(wasm, (roles, signer.clone()));
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

    pub fn account_wasm(env: Env) -> BytesN<32> {
        env.storage().instance().get(&DataKey::AccountWasm).unwrap()
    }

    pub fn roles(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Roles).unwrap()
    }
}

#[cfg(test)]
mod test;
