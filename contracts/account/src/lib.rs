#![no_std]
//! A Nila account (executive decision 26 Oct): members, leaders, machine owners and LPs each
//! hold one. **It belongs to the member (executive decision 3 Nov):** it is deployed by the
//! immutable deployer with no union, the member names her union herself (`set_union`, her own
//! signature) and only she can change its code (`upgrade`, her own signature). The union's
//! settings (recovery wait, session length and contracts, SMS key, fx_pool and USDC) come from
//! roles (`union_settings`) for the union she named; with no union named, or a union that has
//! set no settings, there is no recovery and no leader session (`NoUnion` / `NoSettings`).
//! Its signer is the member's Privy Stellar wallet on a smartphone (Ed25519; Privy
//! keeps and restores it, so a new smartphone needs no recovery here) or a P-256 key on a
//! keypad phone (WebCrypto device key: ECDSA over SHA-256 of the authorization payload,
//! r | s with s in the lower half); no other copy of a key is kept. The signer signs each task
//! itself (self mode), or (assist and keypad mode) starts a short session in which any leader of
//! the account's union may sign for it: the member's phone shows one signed QR, the leader's app
//! submits `start_session` and then the tasks, each signed by the leader's own account. A session
//! reaches only the contracts the union lists (fund_core only, executive decision 4 Nov; roles
//! accepts only contracts the trust gave the core role), never this account itself (so never
//! `set_union` or `upgrade`) or the USDC token.
//! Recovery replaces the signer on the same address, so titles, shares, investments and
//! balances stay. **It has exactly one path (executive decision 4 Nov):** `recover_by_sms`, the
//! union's SMS key (roles union settings) together with one leader of the union, once the new
//! phone has opened the one-time SMS link; it takes effect after the union's wait
//! (`execute_recovery`, anyone). The new signer may be either type: a smartphone's new Privy key
//! (Ed25519) or a keypad's device key (P-256). A pending recovery freezes the account (executive
//! decision 29 Oct, a stolen phone): the current signer signs nothing, no task and no session,
//! and cannot change union or code; it is not cancelled or replaced, it executes after the wait.
//! Neither proposer can sign a task. The node's SMS key alone can do nothing.
//! LP positions (executive decision 31 Oct): fx_pool (the one the union names) locks USDC in
//! this account. A Collect locks the LP's own USDC at accept and takes it into fx_pool at the
//! scan (`pay`); a Bring puts the union's USDC here and locks it, until a Collect of the same LP
//! takes it back (`pay`) or a settle past the rate margin unlocks it. The account never signs an
//! `approve` on any contract (review 5 Nov: an allowance given while no USDC was guarded would
//! outlive a later lien, and `transfer_from` asks this account nothing). While a lien stands the
//! signer cannot authorize a USDC transfer or burn directly; free USDC goes out with
//! `send_usdc`, which keeps the lien covered. Only fx_pool locks, unlocks or takes locked USDC.
//! **The lien's fx_pool is fixed at the first lock (3 Nov):** the fx_pool and USDC the union
//! names when the lien goes from 0 to above 0 are recorded; while the lien stands only that
//! fx_pool may lock more, unlock or take, and the lien is counted in that USDC, even if the union
//! names another fx_pool meanwhile. The record is dropped when the lien is back at 0. The member
//! cannot change union or code while a lien stands.

use nila_common::bump_instance;
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
    val = "Nila account: member-owned, one signer, leader sessions, recovery on the same address"
);

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    /// The co-signer of a recovery is not a leader of the account's union.
    NotLeaders = 1,
    /// No recovery pending, or its wait has not passed.
    NotReady = 2,
    /// The account already runs this code.
    Current = 3,
    /// No session, it has ended, or the signer is not a leader of the union.
    NoSession = 4,
    /// A session signature for a call outside the session's contracts.
    OutsideSession = 5,
    /// The member's session QR has expired.
    Expired = 6,
    /// A signature of another key type than the signer's.
    WrongKind = 7,
    /// A recovery is pending: the account signs nothing until it executes.
    Frozen = 8,
    /// The union names no fx_pool or USDC (LP positions are not set up for it), or the caller is
    /// not the fx_pool that holds the lien.
    NotPool = 9,
    /// Locked USDC: a lock above the free USDC, a send above it, a direct USDC move while a lien
    /// stands, or a change of union or code while a lien stands.
    Locked = 10,
    /// fx_pool asked for more than the lien.
    NotClaimable = 11,
    /// A zero or negative amount.
    BadAmount = 12,
    /// The union names no SMS key (SMS recovery is off for this union).
    NoSmsKey = 13,
    /// The member has named no union (no recovery, no session, no LP positions).
    NoUnion = 14,
    /// The named union has set no settings in roles.
    NoSettings = 15,
    /// A leader session is running: no change of union or code.
    InSession = 16,
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
    Roles,
    /// The union the member named (`set_union`); absent until she names one.
    Union,
    Signer,
    Pending,
    /// Until when (unix seconds) a leader of the union may sign for the account.
    Session,
    /// USDC locked by fx_pool for the union's cash she holds (the LP pledge, 30 Oct).
    Lien,
    /// The fx_pool and USDC of the lien, recorded at the lock that opened it (3 Nov).
    LienPool,
    LienUsdc,
    /// A change of union or code waiting out the union's recovery wait (review 5 Nov).
    Move,
}

/// What a waiting move changes (review 5 Nov).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MoveTo {
    Union(Address),
    Code(BytesN<32>),
}

/// A move of an account that already names a union: it applies after the union's recovery wait,
/// and a recovery proposed meanwhile cancels it, so a stolen key can never outrun recovery.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Move {
    pub to: MoveTo,
    pub at: u64,
}

/// A union's settings as roles keeps them (`roles.union_settings`, 3 Nov); same layout as
/// nila-roles' `UnionSettings`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionSettings {
    pub wait: u64,
    pub session_len: u64,
    pub session_contracts: Vec<Address>,
    pub sms_key: Option<Address>,
    pub fx_pool: Option<Address>,
    pub usdc: Option<Address>,
}

/// What an account reads from roles.
#[contractclient(name = "RolesClient")]
pub trait RolesTrait {
    fn is_leader(union: Address, account: Address) -> bool;
    fn union_settings(union: Address) -> Option<UnionSettings>;
}

#[contractevent]
pub struct LienSet {
    pub lien: i128,
}

/// fx_pool took locked USDC into itself (a Collect closing or opening an LP position, 31 Oct).
#[contractevent]
pub struct PaidPool {
    pub usdc: i128,
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
pub struct SessionStarted {
    pub until: u64,
}

#[contractevent]
pub struct SessionUsed {
    pub leader: Address,
}

/// A change of union or code is waiting (review 5 Nov); `finish_move` applies it from `at`.
#[contractevent]
pub struct MoveWaiting {
    pub to: MoveTo,
    pub at: u64,
}

/// The member named her union (3 Nov).
#[contractevent]
pub struct UnionSet {
    pub union: Address,
}

fn get<V: soroban_sdk::TryFromVal<Env, soroban_sdk::Val>>(env: &Env, k: DataKey) -> V {
    env.storage().instance().get(&k).unwrap()
}

fn roles(env: &Env) -> RolesClient<'_> {
    RolesClient::new(env, &get(env, DataKey::Roles))
}

fn union_of(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Union)
        .ok_or(Error::NoUnion)
}

/// The named union's settings; none named → `NoUnion`, none set → `NoSettings`.
fn settings(env: &Env) -> Result<UnionSettings, Error> {
    roles(env)
        .union_settings(&union_of(env)?)
        .ok_or(Error::NoSettings)
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

/// Back at 0 the lien's fx_pool and USDC are forgotten.
fn set_lien(env: &Env, v: i128) {
    let v = core::cmp::max(v, 0);
    let s = env.storage().instance();
    s.set(&DataKey::Lien, &v);
    if v == 0 {
        s.remove(&DataKey::LienPool);
        s.remove(&DataKey::LienUsdc);
    }
    LienSet { lien: v }.publish(env);
    bump_instance(env);
}

/// The fx_pool and USDC that count: the lien's while one stands, else the union's.
fn pool_usdc(env: &Env) -> Result<(Address, Address), Error> {
    let s = env.storage().instance();
    if let (Some(p), Some(u)) = (s.get(&DataKey::LienPool), s.get(&DataKey::LienUsdc)) {
        return Ok((p, u));
    }
    let st = settings(env)?;
    match (st.fx_pool, st.usdc) {
        (Some(p), Some(u)) => Ok((p, u)),
        _ => Err(Error::NotPool),
    }
}

/// USDC above the lien.
fn free_usdc(env: &Env, usdc: &Address) -> i128 {
    TokenClient::new(env, usdc).balance(&env.current_contract_address()) - lien(env)
}

/// The USDC that counts for the guard: the lien's, else the union's (`st`, read once by the
/// caller); with neither there is no USDC to guard.
fn usdc_guarded(env: &Env, st: &Option<UnionSettings>) -> Option<Address> {
    env.storage()
        .instance()
        .get(&DataKey::LienUsdc)
        .or_else(|| st.as_ref().and_then(|st| st.usdc.clone()))
}

/// The lien (30 Oct): no `approve` ever, on any contract (no flow needs one; an allowance would
/// survive a later lien, as `transfer_from` never asks this account), and no direct USDC transfer
/// or burn while a lien stands (`send_usdc` checks the balance; the USDC contract cannot be read
/// here, as it is on the call stack when it asks for this account's authorization).
fn guard_usdc(env: &Env, token: &Option<Address>, contexts: &Vec<Context>) -> Result<(), Error> {
    let liened = lien(env) > 0;
    for c in contexts.iter() {
        if let Context::Contract(cc) = c {
            let f = cc.fn_name;
            if f == symbol_short!("approve") {
                return Err(Error::Locked);
            }
            if liened
                && Some(&cc.contract) == token.as_ref()
                && (f == symbol_short!("transfer") || f == symbol_short!("burn"))
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

fn in_session(env: &Env) -> bool {
    let until: Option<u64> = env.storage().instance().get(&DataKey::Session);
    until.is_some_and(|u| u >= env.ledger().timestamp())
}

/// Before a change only the member makes (union, code): not frozen, no session, no lien.
fn member_only(env: &Env) -> Result<(), Error> {
    if frozen(env) {
        return Err(Error::Frozen);
    }
    if in_session(env) {
        return Err(Error::InSession);
    }
    if lien(env) > 0 {
        return Err(Error::Locked);
    }
    Ok(())
}

/// A proposal freezes the account, ends any running session and cancels a waiting move.
fn propose(env: &Env, signer: Signer) -> Result<(), Error> {
    let at = env.ledger().timestamp() + settings(env)?.wait;
    env.storage().instance().remove(&DataKey::Session);
    env.storage().instance().remove(&DataKey::Move);
    env.storage().instance().set(
        &DataKey::Pending,
        &Pending {
            signer: signer.clone(),
            at,
        },
    );
    RecoveryProposed { signer, at }.publish(env);
    bump_instance(env);
    Ok(())
}

/// Review 5 Nov: with no union named yet (a new account) a move applies at once; once a union is
/// named it waits the union's recovery wait (`finish_move`). Returns true when it waits.
fn wait_move(env: &Env, to: MoveTo) -> Result<bool, Error> {
    if !env.storage().instance().has(&DataKey::Union) {
        return Ok(false);
    }
    let at = env.ledger().timestamp() + settings(env)?.wait;
    env.storage()
        .instance()
        .set(&DataKey::Move, &Move { to: to.clone(), at });
    MoveWaiting { to, at }.publish(env);
    bump_instance(env);
    Ok(true)
}

fn apply_union(env: &Env, union: Address) {
    env.storage().instance().set(&DataKey::Union, &union);
    UnionSet { union }.publish(env);
    bump_instance(env);
}

fn is_leader(env: &Env, union: &Address, who: &Address) -> bool {
    roles(env).is_leader(union, who)
}

#[contract]
pub struct AccountContract;

#[contractimpl]
impl AccountContract {
    /// Deployed by the deployer (or anyone) with no union: the member names it with `set_union`.
    pub fn __constructor(env: Env, roles: Address, signer: Signer) {
        let s = env.storage().instance();
        s.set(&DataKey::Roles, &roles);
        s.set(&DataKey::Signer, &signer);
        bump_instance(&env);
    }

    /// The member names (or changes) her union, with her own signature: never while a recovery
    /// is pending, a leader session runs, or a lien stands. Only a union with settings in roles
    /// (`NoSettings` otherwise), so never one the trust has not admitted (decision 008). Her first
    /// union applies at once; a change waits the union's recovery wait (`finish_move`, review 5 Nov).
    pub fn set_union(env: Env, union: Address) -> Result<(), Error> {
        member_only(&env)?;
        env.current_contract_address().require_auth();
        roles(&env)
            .union_settings(&union)
            .ok_or(Error::NoSettings)?;
        if !wait_move(&env, MoveTo::Union(union.clone()))? {
            apply_union(&env, union);
        }
        Ok(())
    }

    /// Anyone, once a waiting move's time has come and no recovery cancelled it: the change of
    /// union or code applies (review 5 Nov). Same conditions as the move itself.
    pub fn finish_move(env: Env) -> Result<(), Error> {
        let m: Move = env
            .storage()
            .instance()
            .get(&DataKey::Move)
            .ok_or(Error::NotReady)?;
        if env.ledger().timestamp() < m.at {
            return Err(Error::NotReady);
        }
        member_only(&env)?;
        env.storage().instance().remove(&DataKey::Move);
        match m.to {
            MoveTo::Union(u) => {
                roles(&env).union_settings(&u).ok_or(Error::NoSettings)?;
                apply_union(&env, u);
            }
            MoveTo::Code(h) => env.deployer().update_current_contract_wasm(h),
        }
        Ok(())
    }

    pub fn pending_move(env: Env) -> Option<Move> {
        env.storage().instance().get(&DataKey::Move)
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
    /// for the account for the union's session length.
    pub fn start_session(env: Env, not_after: u64) -> Result<u64, Error> {
        if frozen(&env) {
            return Err(Error::Frozen);
        }
        let len = settings(&env)?.session_len;
        env.current_contract_address().require_auth();
        let now = env.ledger().timestamp();
        if now > not_after {
            return Err(Error::Expired);
        }
        let until = now + len;
        env.storage().instance().set(&DataKey::Session, &until);
        SessionStarted { until }.publish(&env);
        bump_instance(&env);
        Ok(until)
    }

    /// The account's own signer ends a session early (a session never reaches the account itself).
    pub fn end_session(env: Env) {
        env.current_contract_address().require_auth();
        env.storage().instance().remove(&DataKey::Session);
    }

    /// The one recovery path (executive decision 4 Nov): one leader of the union and the union's
    /// SMS key together propose a new signer, of either type (a smartphone's new Privy key,
    /// Ed25519, or a keypad's device key, P-256); the node co-signs only for the key the new phone
    /// registered through the SMS link. It takes effect after the union's wait.
    pub fn recover_by_sms(env: Env, signer: Signer, leader: Address) -> Result<(), Error> {
        let union = union_of(&env)?;
        leader.require_auth();
        let sms = settings(&env)?.sms_key.ok_or(Error::NoSmsKey)?;
        sms.require_auth();
        if !is_leader(&env, &union, &leader) {
            return Err(Error::NotLeaders);
        }
        // A pending recovery is never replaced (that would restart the wait and could swap the key).
        if frozen(&env) {
            return Err(Error::Frozen);
        }
        propose(&env, signer)
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

    /// The member alone moves her account to new code (her own signature; a leader session never
    /// reaches this account): never while a recovery is pending, a session runs, or a lien stands.
    /// Once a union is named it waits the union's recovery wait (`finish_move`, review 5 Nov).
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        member_only(&env)?;
        env.current_contract_address().require_auth();
        if env.current_contract_address().executable()
            == Some(Executable::Wasm(new_wasm_hash.clone()))
        {
            return Err(Error::Current);
        }
        if !wait_move(&env, MoveTo::Code(new_wasm_hash.clone()))? {
            env.deployer().update_current_contract_wasm(new_wasm_hash);
        }
        Ok(())
    }

    // ---------- LP positions (31 Oct) ----------

    /// fx_pool only (an LP accepts a Collect, or a Bring put the union's USDC here): locks `amount`
    /// of the free USDC. The lock that opens a lien records its fx_pool and USDC. Returns the lien.
    pub fn lock(env: Env, amount: i128) -> Result<i128, Error> {
        let (pool, usdc) = pool_usdc(&env)?;
        pool.require_auth();
        if amount <= 0 {
            return Err(Error::BadAmount);
        }
        if amount > free_usdc(&env, &usdc) {
            return Err(Error::Locked);
        }
        let l = lien(&env);
        if l == 0 {
            env.storage().instance().set(&DataKey::LienPool, &pool);
            env.storage().instance().set(&DataKey::LienUsdc, &usdc);
        }
        set_lien(&env, l + amount);
        Ok(l + amount)
    }

    /// The lien's fx_pool only (a settle past the margin, or an accepted Collect that expired or
    /// was cancelled). Allowed while frozen. Returns the lien.
    pub fn unlock(env: Env, amount: i128) -> Result<i128, Error> {
        pool_usdc(&env)?.0.require_auth();
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
        let (_, usdc) = pool_usdc(&env)?;
        if amount > free_usdc(&env, &usdc) {
            return Err(Error::Locked);
        }
        TokenClient::new(&env, &usdc).transfer(&env.current_contract_address(), &to, &amount);
        Ok(())
    }

    /// The lien's fx_pool only, at a Collect's scan (allowed while frozen): `amount` of the lien
    /// goes to that fx_pool and the lien drops by it. Returns the lien.
    pub fn pay(env: Env, amount: i128) -> Result<i128, Error> {
        let (pool, usdc) = pool_usdc(&env)?;
        pool.require_auth();
        let l = lien(&env);
        if amount <= 0 || amount > l {
            return Err(Error::NotClaimable);
        }
        TokenClient::new(&env, &usdc).transfer(&env.current_contract_address(), &pool, &amount);
        set_lien(&env, l - amount);
        PaidPool { usdc: amount }.publish(&env);
        Ok(l - amount)
    }

    pub fn lien(env: Env) -> i128 {
        lien(&env)
    }

    /// The fx_pool that holds the lien (none while the lien is 0).
    pub fn lien_pool(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::LienPool)
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

    /// The union the member named, if any.
    pub fn union(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Union)
    }

    pub fn roles(env: Env) -> Address {
        get(&env, DataKey::Roles)
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
        // The union's settings, read once for the guard and the session check.
        let union = union_of(&env).ok();
        let st = union.as_ref().and_then(|u| roles(&env).union_settings(u));
        let usdc = usdc_guarded(&env, &st);
        guard_usdc(&env, &usdc, &auth_contexts)?;
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
                let Some(union) = union else {
                    return Err(Error::NoSession);
                };
                if !in_session(&env) || !is_leader(&env, &union, &leader) {
                    return Err(Error::NoSession);
                }
                let allowed = st.ok_or(Error::NoSettings)?.session_contracts;
                let me = env.current_contract_address();
                for c in auth_contexts.iter() {
                    match c {
                        // Never this account (its union, code, signer) or the USDC, whatever
                        // the union lists.
                        Context::Contract(cc)
                            if allowed.contains(&cc.contract)
                                && cc.contract != me
                                && Some(&cc.contract) != usdc.as_ref() => {}
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
