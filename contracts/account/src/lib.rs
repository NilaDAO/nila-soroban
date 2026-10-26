#![no_std]
//! A Nila account (executive decision 26 Oct): members, leaders, machine owners and LPs each
//! hold one. Its signer is the member's Privy Stellar wallet on a smartphone (Ed25519; Privy
//! keeps and restores it, so a new smartphone needs no recovery here) or a P-256 key on a
//! keypad phone (WebCrypto device key: ECDSA over SHA-256 of the authorization payload,
//! r | s with s in the lower half); no other copy of a key is kept. The signer signs each task itself (self mode), or (assist and keypad mode) starts a
//! short session in which any leader of the account's union may sign for it: the member's
//! phone shows one signed QR, the leader's app submits `start_session` and then the tasks,
//! each signed by the leader's own account. A session reaches only the contracts the factory
//! lists (fund_core, machinery), never this account itself or a token.
//! Recovery replaces the signer on the same address, so titles, shares, investments and
//! balances stay: proposed by the node's recovery key after its SMS check, or by two leaders
//! of the union when the number is lost; it takes effect after the factory's wait, and the
//! account's own signer can cancel it meanwhile. Neither can sign a task.
//! Settings (recovery key, roles, wait, session length and contracts, the account code) come
//! from the factory that deployed the account.

use nila_common::{bump_instance, RolesClient};
use soroban_sdk::{
    auth::{Context, CustomAccountInterface},
    contract, contractclient, contracterror, contractevent, contractimpl, contractmeta,
    contracttype,
    crypto::Hash,
    Address, BytesN, Env, Executable, IntoVal, Vec,
};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(
    key = "desc",
    val = "Nila account: one signer, leader sessions, recovery on the same address"
);

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    /// Not two different leaders of the account's union.
    NotLeaders = 1,
    /// No recovery pending, or its wait has not passed.
    NotReady = 2,
    /// The account code is already the factory's.
    Current = 3,
    /// No session, it has ended, or the signer is not a leader of the union.
    NoSession = 4,
    /// A session signature for a call outside the session's contracts.
    OutsideSession = 5,
    /// The member's session QR has expired.
    Expired = 6,
    /// A signature of another key type than the signer's.
    WrongKind = 7,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Signer {
    /// The Privy Stellar wallet (smartphones).
    Ed25519(BytesN<32>),
    /// The keypad's device key, SEC1 uncompressed public key (0x04 | x | y).
    P256(BytesN<65>),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Signature {
    /// The signer's own, Ed25519.
    Ed25519(BytesN<64>),
    /// The signer's own, P-256: r | s, s in the lower half.
    P256(BytesN<64>),
    /// In a session: the leader who signs for the account (the leader's account authorizes it).
    Leader(Address),
}

/// A proposed signer and when it may take effect (unix seconds).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pending {
    pub signer: Signer,
    pub at: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Factory,
    Union,
    Signer,
    Pending,
    /// Until when (unix seconds) a leader of the union may sign for the account.
    Session,
}

/// What an account reads from its factory.
#[contractclient(name = "FactoryClient")]
pub trait FactoryTrait {
    fn recovery(env: Env) -> Address;
    fn roles(env: Env) -> Address;
    fn wait(env: Env) -> u64;
    fn session_len(env: Env) -> u64;
    fn session_contracts(env: Env) -> Vec<Address>;
    fn account_wasm(env: Env) -> BytesN<32>;
}

#[contractevent]
pub struct RecoveryProposed {
    pub signer: Signer,
    pub at: u64,
}

#[contractevent]
pub struct SignerSet {
    pub signer: Signer,
}

#[contractevent]
pub struct RecoveryCancelled {}

#[contractevent]
pub struct SessionStarted {
    pub until: u64,
}

#[contractevent]
pub struct SessionUsed {
    pub leader: Address,
}

fn get<V: soroban_sdk::TryFromVal<Env, soroban_sdk::Val>>(env: &Env, k: DataKey) -> V {
    env.storage().instance().get(&k).unwrap()
}

fn factory(env: &Env) -> FactoryClient<'_> {
    FactoryClient::new(env, &get(env, DataKey::Factory))
}

fn set_signer(env: &Env, signer: Signer) {
    env.storage().instance().set(&DataKey::Signer, &signer);
    env.storage().instance().remove(&DataKey::Pending);
    env.storage().instance().remove(&DataKey::Session);
    SignerSet { signer }.publish(env);
    bump_instance(env);
}

fn propose(env: &Env, signer: Signer) {
    let at = env.ledger().timestamp() + factory(env).wait();
    env.storage().instance().set(
        &DataKey::Pending,
        &Pending {
            signer: signer.clone(),
            at,
        },
    );
    RecoveryProposed { signer, at }.publish(env);
    bump_instance(env);
}

fn is_leader(env: &Env, who: &Address) -> bool {
    let union: Address = get(env, DataKey::Union);
    RolesClient::new(env, &factory(env).roles()).is_leader(&union, who)
}

#[contract]
pub struct AccountContract;

#[contractimpl]
impl AccountContract {
    pub fn __constructor(env: Env, factory: Address, union: Address, signer: Signer) {
        let s = env.storage().instance();
        s.set(&DataKey::Factory, &factory);
        s.set(&DataKey::Union, &union);
        s.set(&DataKey::Signer, &signer);
        bump_instance(&env);
    }

    /// The account's own signer moves to a new key (a new phone).
    pub fn set_signer(env: Env, signer: Signer) {
        env.current_contract_address().require_auth();
        set_signer(&env, signer);
    }

    /// Assist and keypad mode: the member's phone signs this (its QR, valid until `not_after`,
    /// unix seconds by the phone's clock); from its submission any leader of the union may sign
    /// for the account for the factory's session length.
    pub fn start_session(env: Env, not_after: u64) -> Result<u64, Error> {
        env.current_contract_address().require_auth();
        let now = env.ledger().timestamp();
        if now > not_after {
            return Err(Error::Expired);
        }
        let until = now + factory(&env).session_len();
        env.storage().instance().set(&DataKey::Session, &until);
        SessionStarted { until }.publish(&env);
        bump_instance(&env);
        Ok(until)
    }

    /// The account's own signer, or a leader in the session, ends it early.
    pub fn end_session(env: Env) {
        env.current_contract_address().require_auth();
        env.storage().instance().remove(&DataKey::Session);
    }

    /// The node's recovery key, after its SMS check, proposes a new signer.
    pub fn recover(env: Env, signer: Signer) {
        factory(&env).recovery().require_auth();
        propose(&env, signer);
    }

    /// Two leaders of the account's union together propose a new signer (the number is lost).
    pub fn recover_by_leaders(
        env: Env,
        signer: Signer,
        leader_a: Address,
        leader_b: Address,
    ) -> Result<(), Error> {
        if leader_a == leader_b {
            return Err(Error::NotLeaders);
        }
        leader_a.require_auth();
        leader_b.require_auth();
        if !is_leader(&env, &leader_a) || !is_leader(&env, &leader_b) {
            return Err(Error::NotLeaders);
        }
        propose(&env, signer);
        Ok(())
    }

    /// Anyone, once the wait has passed: the proposed signer takes over.
    pub fn execute_recovery(env: Env) -> Result<(), Error> {
        let p: Pending = env
            .storage()
            .instance()
            .get(&DataKey::Pending)
            .ok_or(Error::NotReady)?;
        if env.ledger().timestamp() < p.at {
            return Err(Error::NotReady);
        }
        set_signer(&env, p.signer);
        Ok(())
    }

    /// The account's own signer cancels a proposed recovery.
    pub fn cancel_recovery(env: Env) {
        env.current_contract_address().require_auth();
        env.storage().instance().remove(&DataKey::Pending);
        RecoveryCancelled {}.publish(&env);
        bump_instance(&env);
    }

    /// Anyone: the account moves to the factory's current account code (the factory changes it
    /// only after its timelock).
    pub fn upgrade(env: Env) -> Result<(), Error> {
        let wasm = factory(&env).account_wasm();
        if env.current_contract_address().executable() == Some(Executable::Wasm(wasm.clone())) {
            return Err(Error::Current);
        }
        env.deployer().update_current_contract_wasm(wasm);
        Ok(())
    }

    pub fn signer(env: Env) -> Signer {
        get(&env, DataKey::Signer)
    }

    pub fn pending(env: Env) -> Option<Pending> {
        env.storage().instance().get(&DataKey::Pending)
    }

    pub fn session(env: Env) -> Option<u64> {
        let until: Option<u64> = env.storage().instance().get(&DataKey::Session);
        until.filter(|u| *u >= env.ledger().timestamp())
    }

    pub fn union(env: Env) -> Address {
        get(&env, DataKey::Union)
    }

    pub fn factory(env: Env) -> Address {
        get(&env, DataKey::Factory)
    }
}

#[contractimpl]
impl CustomAccountInterface for AccountContract {
    type Signature = Signature;
    type Error = Error;

    #[allow(non_snake_case)]
    fn __check_auth(
        env: Env,
        signature_payload: Hash<32>,
        signature: Signature,
        auth_contexts: Vec<Context>,
    ) -> Result<(), Error> {
        match signature {
            Signature::Ed25519(sig) => match get(&env, DataKey::Signer) {
                Signer::Ed25519(pk) => {
                    env.crypto()
                        .ed25519_verify(&pk, &signature_payload.into(), &sig)
                }
                _ => return Err(Error::WrongKind),
            },
            Signature::P256(sig) => match get(&env, DataKey::Signer) {
                Signer::P256(pk) => {
                    let digest = env.crypto().sha256(&signature_payload.into());
                    env.crypto().secp256r1_verify(&pk, &digest, &sig);
                }
                _ => return Err(Error::WrongKind),
            },
            Signature::Leader(leader) => {
                let until: Option<u64> = env.storage().instance().get(&DataKey::Session);
                if until.is_none_or(|u| env.ledger().timestamp() > u) || !is_leader(&env, &leader) {
                    return Err(Error::NoSession);
                }
                let allowed = factory(&env).session_contracts();
                for c in auth_contexts.iter() {
                    match c {
                        Context::Contract(cc) if allowed.contains(&cc.contract) => {}
                        _ => return Err(Error::OutsideSession),
                    }
                }
                leader.require_auth_for_args((signature_payload,).into_val(&env));
                SessionUsed { leader }.publish(&env);
            }
        }
        bump_instance(&env);
        Ok(())
    }
}

#[cfg(test)]
mod test;
