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
//! balances stay: proposed by two leaders of the union (a lost phone, smartphone or keypad;
//! executive decision 29 Oct, no SMS key: Nila holds no key with any power here) or by the
//! union's multisig account;
//! it takes effect after the factory's wait. A pending recovery freezes the account
//! (executive decision 29 Oct, a stolen phone): the current signer signs nothing, no task and
//! no session, and cannot cancel; only the union's multisig account cancels, so a member
//! disputes a recovery through the union. Neither proposer can sign a task. (A smartphone gets its Privy key back at login, so it
//! needs none of this unless the member's number is lost.) The node holds no power here.
//! Settings (roles, wait, session length and contracts, the account code) come from the
//! factory that deployed the account, one per union.
//! LP pledge (executive decision 30 Oct): an LP's USDC stays in this account. fx_pool (the one
//! the factory names) puts a lien on part of it when she takes the union's cash and lifts it as
//! she brings the cash back. While a lien stands the signer cannot authorize a USDC transfer,
//! burn or approve directly; free USDC goes out with `send_usdc`, which keeps the lien covered.
//! Only the union's multisig takes locked USDC, with `claim`, after a called position's notice.

use nila_common::{bump_instance, RolesClient};
use soroban_sdk::{
    auth::{Context, CustomAccountInterface},
    contract, contractclient, contracterror, contractevent, contractimpl, contractmeta,
    contracttype,
    crypto::Hash,
    panic_with_error, symbol_short,
    token::TokenClient,
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
    /// A recovery is pending: the account signs nothing until it executes or the union cancels it.
    Frozen = 8,
    /// The factory names no fx_pool or USDC (the pledge is not set up for this union).
    NotPool = 9,
    /// Locked USDC: a lock above the free USDC, a send above it, or a direct USDC move while a lien stands.
    Locked = 10,
    /// The union's claim is above what fx_pool says it may claim, or above the lien.
    NotClaimable = 11,
    /// A zero or negative amount.
    BadAmount = 12,
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
    /// USDC locked by fx_pool for the union's cash she holds (the LP pledge, 30 Oct).
    Lien,
}

/// What an account reads from its factory.
#[contractclient(name = "FactoryClient")]
pub trait FactoryTrait {
    fn union(env: Env) -> Address;
    fn roles(env: Env) -> Address;
    fn wait(env: Env) -> u64;
    fn session_len(env: Env) -> u64;
    fn session_contracts(env: Env) -> Vec<Address>;
    fn account_wasm(env: Env) -> BytesN<32>;
    fn fx_pool(env: Env) -> Option<Address>;
    fn usdc(env: Env) -> Option<Address>;
}

/// What the account asks fx_pool about the union's claim (fx_pool never calls back into it).
#[contractclient(name = "PoolClient")]
pub trait PoolTrait {
    fn claimable(env: Env, union: Address, lp: Address) -> i128;
    fn book_claim(env: Env, union: Address, lp: Address, usdc: i128) -> i128;
}

#[contractevent]
pub struct LienSet {
    pub lien: i128,
}

#[contractevent]
pub struct Claimed {
    pub usdc: i128,
    pub unlocked: i128,
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

fn lien(env: &Env) -> i128 {
    env.storage().instance().get(&DataKey::Lien).unwrap_or(0)
}

fn set_lien(env: &Env, v: i128) {
    let v = core::cmp::max(v, 0);
    env.storage().instance().set(&DataKey::Lien, &v);
    LienSet { lien: v }.publish(env);
    bump_instance(env);
}

fn pool(env: &Env) -> Result<Address, Error> {
    factory(env).fx_pool().ok_or(Error::NotPool)
}

fn usdc(env: &Env) -> Result<TokenClient<'_>, Error> {
    Ok(TokenClient::new(
        env,
        &factory(env).usdc().ok_or(Error::NotPool)?,
    ))
}

/// USDC above the lien.
fn free_usdc(env: &Env) -> Result<i128, Error> {
    Ok(usdc(env)?.balance(&env.current_contract_address()) - lien(env))
}

/// The lien (30 Oct): no USDC approve ever (no flow needs one), and no direct USDC transfer or
/// burn while a lien stands (`send_usdc` checks the balance; the USDC contract cannot be read
/// here, as it is on the call stack when it asks for this account's authorization).
fn guard_usdc(env: &Env, contexts: &Vec<Context>) -> Result<(), Error> {
    let Some(token) = factory(env).usdc() else {
        return Ok(());
    };
    let liened = lien(env) > 0;
    for c in contexts.iter() {
        if let Context::Contract(cc) = c {
            if cc.contract != token {
                continue;
            }
            let f = cc.fn_name;
            if f == symbol_short!("approve")
                || (liened && (f == symbol_short!("transfer") || f == symbol_short!("burn")))
            {
                return Err(Error::Locked);
            }
        }
    }
    Ok(())
}

fn frozen(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::Pending)
}

/// A proposal freezes the account and ends any running session.
fn propose(env: &Env, signer: Signer) {
    env.storage().instance().remove(&DataKey::Session);
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
        if frozen(&env) {
            panic_with_error!(&env, Error::Frozen);
        }
        env.current_contract_address().require_auth();
        set_signer(&env, signer);
    }

    /// Assist and keypad mode: the member's phone signs this (its QR, valid until `not_after`,
    /// unix seconds by the phone's clock); from its submission any leader of the union may sign
    /// for the account for the factory's session length.
    pub fn start_session(env: Env, not_after: u64) -> Result<u64, Error> {
        if frozen(&env) {
            return Err(Error::Frozen);
        }
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

    /// The union's multisig account proposes a new signer (e.g. a keypad member's new phone).
    pub fn recover(env: Env, signer: Signer) {
        factory(&env).union().require_auth();
        propose(&env, signer);
    }

    /// Two leaders of the account's union together propose a new signer (a lost phone).
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
        // Two leaders cannot replace a pending proposal (restarting the wait would keep the account frozen, and
        // could swap the key): only the union's multisig cancels it or overrides it with `recover`.
        if frozen(&env) {
            return Err(Error::Frozen);
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

    /// The union's multisig account cancels a proposed recovery (a member's dispute goes to the
    /// union); the account's own signer cannot, since a pending recovery freezes it.
    pub fn cancel_recovery(env: Env) {
        factory(&env).union().require_auth();
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

    // ---------- the LP pledge (30 Oct) ----------

    /// fx_pool only (an LP accepts a Collect): locks `amount` of her free USDC. Returns the lien.
    pub fn lock(env: Env, amount: i128) -> Result<i128, Error> {
        pool(&env)?.require_auth();
        if amount <= 0 {
            return Err(Error::BadAmount);
        }
        if amount > free_usdc(&env)? {
            return Err(Error::Locked);
        }
        let l = lien(&env) + amount;
        set_lien(&env, l);
        Ok(l)
    }

    /// fx_pool only (a Bring, or an accepted Collect that expired or was cancelled). Allowed while
    /// frozen. Returns the lien.
    pub fn unlock(env: Env, amount: i128) -> Result<i128, Error> {
        pool(&env)?.require_auth();
        if amount <= 0 {
            return Err(Error::BadAmount);
        }
        let l = core::cmp::max(lien(&env) - amount, 0);
        set_lien(&env, l);
        Ok(l)
    }

    /// The signer sends free USDC (the lien stays covered).
    pub fn send_usdc(env: Env, to: Address, amount: i128) -> Result<(), Error> {
        if frozen(&env) {
            return Err(Error::Frozen);
        }
        env.current_contract_address().require_auth();
        if amount <= 0 {
            return Err(Error::BadAmount);
        }
        if amount > free_usdc(&env)? {
            return Err(Error::Locked);
        }
        usdc(&env)?.transfer(&env.current_contract_address(), &to, &amount);
        Ok(())
    }

    /// The union's multisig claims locked USDC after a called position's notice (allowed while
    /// frozen): `amount` (at most fx_pool's `claimable`) goes to the union's account, fx_pool books
    /// it and says what unlocks. Returns the USDC unlocked.
    pub fn claim(env: Env, amount: i128) -> Result<i128, Error> {
        let f = factory(&env);
        let union = f.union();
        union.require_auth();
        let me = env.current_contract_address();
        let p = PoolClient::new(&env, &pool(&env)?);
        let l = lien(&env);
        if amount <= 0 || amount > l || amount > p.claimable(&union, &me) {
            return Err(Error::NotClaimable);
        }
        usdc(&env)?.transfer(&me, &union, &amount);
        let unlocked = p.book_claim(&union, &me, &amount);
        set_lien(&env, l - amount - unlocked);
        Claimed {
            usdc: amount,
            unlocked,
        }
        .publish(&env);
        Ok(unlocked)
    }

    pub fn lien(env: Env) -> i128 {
        lien(&env)
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
        if frozen(&env) {
            return Err(Error::Frozen);
        }
        guard_usdc(&env, &auth_contexts)?;
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
