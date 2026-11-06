#![cfg(test)]

use super::*;
use p256::ecdsa::{signature::hazmat::PrehashSigner, Signature as EcSig, SigningKey};
use soroban_sdk::{
    auth::ContractContext, symbol_short, testutils::Address as _, testutils::Ledger, vec, Address,
    BytesN, Env, IntoVal,
};

/// fx_pool as the account sees it: only its address matters (it locks, unlocks and takes).
mod mocks {
    use soroban_sdk::{contract, contractimpl, Env};
    #[contract]
    pub struct Pool;
    #[contractimpl]
    impl Pool {
        pub fn ping(_env: Env) {}
    }
}

use nila_roles::{RolesContract, RolesContractClient, UnionSettings};

struct T {
    env: Env,
    acct: Address,
    c: AccountContractClient<'static>,
    key: SigningKey,
    leaders: [Address; 2],
    fund_core: Address,
    /// The union account (its 2-of-3 multisig): names the leaders, sets the settings.
    union_ms: Address,
    roles: RolesContractClient<'static>,
}

impl T {
    fn settings(&self) -> UnionSettings {
        self.roles.union_settings(&self.union_ms).unwrap()
    }
    fn set_settings(&self, s: &UnionSettings) {
        self.roles.set_union_settings(&self.union_ms, s);
    }
    /// Another union, admitted by the trust, with the same settings as the first.
    fn other_union(&self) -> Address {
        let u = Address::generate(&self.env);
        self.roles
            .admit_union(&u, &BytesN::from_array(&self.env, &[2; 32]));
        self.roles.set_union_settings(&u, &self.settings());
        u
    }
}

/// A waiting move (review 5 Nov): wait it out and apply it.
fn finish(t: &T) {
    let m = t.c.pending_move().unwrap();
    t.env.ledger().set_timestamp(m.at);
    t.c.finish_move();
}

fn signer_of(env: &Env, k: &SigningKey) -> Signer {
    let pt = k.verifying_key().to_encoded_point(false);
    Signer::P256(BytesN::from_array(env, pt.as_bytes().try_into().unwrap()))
}

/// The device key's signature of a payload: ECDSA over SHA-256 of it, low-S (as the app sends it).
fn sign(env: &Env, k: &SigningKey, payload: &[u8; 32]) -> Signature {
    let digest: [u8; 32] = env
        .crypto()
        .sha256(&soroban_sdk::Bytes::from_array(env, payload))
        .into();
    let sig: EcSig = k.sign_prehash(&digest).unwrap();
    let sig = sig.normalize_s().unwrap_or(sig);
    Signature::P256(BytesN::from_array(env, &sig.to_bytes().into()))
}

/// An account with no union yet, and a union (real roles) with two leaders and its settings:
/// 48 h wait, 5-minute sessions reaching fund_core.
fn bare() -> T {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_800_000_000);
    let union_ms = Address::generate(&env);
    let leaders = [Address::generate(&env), Address::generate(&env)];
    let fund_core = Address::generate(&env);
    let roles_id = env.register(RolesContract, (Address::generate(&env),));
    let roles = RolesContractClient::new(&env, &roles_id);
    roles.admit_union(&union_ms, &BytesN::from_array(&env, &[1; 32]));
    // The trust gives fund_core the core role: only such contracts may be session contracts.
    roles.set_role(&fund_core, &nila_roles::Role::Core, &true);
    roles.set_leader(&union_ms, &leaders[0], &true);
    roles.set_leader(&union_ms, &leaders[1], &true);
    roles.set_union_settings(
        &union_ms,
        &UnionSettings {
            wait: 172_800,
            session_len: 300,
            session_contracts: vec![&env, fund_core.clone()],
            sms_key: None,
            fx_pool: None,
            usdc: None,
        },
    );
    let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
    let acct = env.register(AccountContract, (roles_id, signer_of(&env, &key)));
    let c = AccountContractClient::new(&env, &acct);
    T {
        env,
        acct,
        c,
        key,
        leaders,
        fund_core,
        union_ms,
        roles,
    }
}

/// The member has named the union.
fn setup() -> T {
    let t = bare();
    t.c.set_union(&t.union_ms);
    t
}

/// The union names an SMS key (the node's), which turns recovery on; returns it.
fn sms_on(t: &T) -> Address {
    let sms = Address::generate(&t.env);
    let mut st = t.settings();
    st.sms_key = Some(sms.clone());
    t.set_settings(&st);
    sms
}

/// A smartphone's Privy key (Ed25519) as a signer, and its signature of a payload.
fn ed_signer(env: &Env, k: &ed25519_dalek::SigningKey) -> Signer {
    Signer::Ed25519(BytesN::from_array(env, &k.verifying_key().to_bytes()))
}

fn ed_sign(env: &Env, k: &ed25519_dalek::SigningKey, payload: &[u8; 32]) -> Signature {
    use ed25519_dalek::Signer as _;
    Signature::Ed25519(BytesN::from_array(env, &k.sign(payload).to_bytes()))
}

fn call_on(env: &Env, contract: &Address) -> Vec<Context> {
    vec![
        env,
        Context::Contract(ContractContext {
            contract: contract.clone(),
            fn_name: symbol_short!("draw_loan"),
            args: vec![env],
        }),
    ]
}

fn check(
    t: &T,
    payload: [u8; 32],
    sig: Signature,
    ctx: &Vec<Context>,
) -> Result<(), Result<Error, soroban_sdk::InvokeError>> {
    t.env.try_invoke_contract_check_auth::<Error>(
        &t.acct,
        &BytesN::from_array(&t.env, &payload),
        sig.into_val(&t.env),
        ctx,
    )
}

#[test]
fn the_signer_signs_and_no_other_key_does() {
    let t = setup();
    let ctx = call_on(&t.env, &t.fund_core);
    assert_eq!(
        check(&t, [1; 32], sign(&t.env, &t.key, &[1; 32]), &ctx),
        Ok(())
    );
    let other = SigningKey::from_slice(&[9u8; 32]).unwrap();
    assert!(check(&t, [1; 32], sign(&t.env, &other, &[1; 32]), &ctx).is_err());
    // A signature of another payload.
    assert!(check(&t, [2; 32], sign(&t.env, &t.key, &[1; 32]), &ctx).is_err());
}

/// A smartphone's Privy wallet (Ed25519) signs for its account; a P-256 signature does not.
#[test]
fn a_privy_wallet_signs_its_account() {
    use ed25519_dalek::{Signer as _, SigningKey as EdKey};
    let t = setup();
    let ed = EdKey::from_bytes(&[5u8; 32]);
    let pk = BytesN::from_array(&t.env, &ed.verifying_key().to_bytes());
    t.c.set_signer(&Signer::Ed25519(pk));
    let ctx = call_on(&t.env, &t.fund_core);
    let sig = Signature::Ed25519(BytesN::from_array(&t.env, &ed.sign(&[4; 32]).to_bytes()));
    assert_eq!(check(&t, [4; 32], sig, &ctx), Ok(()));
    assert_eq!(
        check(&t, [4; 32], sign(&t.env, &t.key, &[4; 32]), &ctx),
        Err(Ok(Error::WrongKind))
    );
}

#[test]
fn a_leader_signs_only_in_a_session_and_only_for_the_listed_contracts() {
    let t = setup();
    let ctx = call_on(&t.env, &t.fund_core);
    let lead = Signature::Leader(t.leaders[0].clone());
    // No session yet.
    assert_eq!(
        check(&t, [1; 32], lead.clone(), &ctx),
        Err(Ok(Error::NoSession))
    );
    // The member's QR, too late.
    assert_eq!(
        t.c.try_start_session(&1_799_999_999),
        Err(Ok(Error::Expired))
    );
    let until = t.c.start_session(&1_800_000_600);
    assert_eq!(until, 1_800_000_300);
    assert_eq!(check(&t, [1; 32], lead.clone(), &ctx), Ok(()));
    // Not a leader of the union.
    assert_eq!(
        check(
            &t,
            [1; 32],
            Signature::Leader(Address::generate(&t.env)),
            &ctx
        ),
        Err(Ok(Error::NoSession))
    );
    // A call outside the session's contracts (a token, this account itself).
    assert_eq!(
        check(
            &t,
            [1; 32],
            lead.clone(),
            &call_on(&t.env, &Address::generate(&t.env))
        ),
        Err(Ok(Error::OutsideSession))
    );
    assert_eq!(
        check(&t, [1; 32], lead.clone(), &call_on(&t.env, &t.acct)),
        Err(Ok(Error::OutsideSession))
    );
    // Five minutes later the session is over.
    t.env.ledger().set_timestamp(1_800_000_301);
    assert_eq!(check(&t, [1; 32], lead, &ctx), Err(Ok(Error::NoSession)));
    assert_eq!(t.c.session(), None);
}

#[test]
fn recovery_replaces_the_signer_on_the_same_address_after_the_wait() {
    let t = setup();
    sms_on(&t);
    let new_key = SigningKey::from_slice(&[11u8; 32]).unwrap();
    let new = signer_of(&t.env, &new_key);
    t.c.recover_by_sms(&new, &t.leaders[0]);
    assert_eq!(t.c.try_execute_recovery(), Err(Ok(Error::NotReady)));
    t.env.ledger().set_timestamp(1_800_000_000 + 172_799);
    assert_eq!(t.c.try_execute_recovery(), Err(Ok(Error::NotReady)));
    t.env.ledger().set_timestamp(1_800_000_000 + 172_800);
    t.c.execute_recovery();
    assert_eq!(t.c.signer(), new);
    assert_eq!(t.c.pending(), None);
    // Nothing pending: nothing to execute.
    assert_eq!(t.c.try_execute_recovery(), Err(Ok(Error::NotReady)));
    let ctx = call_on(&t.env, &t.fund_core);
    assert_eq!(
        check(&t, [3; 32], sign(&t.env, &new_key, &[3; 32]), &ctx),
        Ok(())
    );
    assert!(check(&t, [3; 32], sign(&t.env, &t.key, &[3; 32]), &ctx).is_err());
}

fn call_named(env: &Env, contract: &Address, f: soroban_sdk::Symbol) -> Vec<Context> {
    vec![
        env,
        Context::Contract(ContractContext {
            contract: contract.clone(),
            fn_name: f,
            args: vec![env],
        }),
    ]
}

/// A stolen phone (executive decision 29 Oct): a proposal freezes the account until it executes;
/// the old signer can neither draw, withdraw, start a session nor change signer, and nobody
/// replaces or cancels the proposal (executive decision 4 Nov: one path, no cancel).
#[test]
fn a_pending_recovery_freezes_the_account_until_it_executes() {
    let t = setup();
    sms_on(&t);
    let lead = Signature::Leader(t.leaders[0].clone());
    let draw = call_on(&t.env, &t.fund_core);
    let withdraw = call_named(&t.env, &t.fund_core, symbol_short!("claim_jc"));
    // A session is running when the recovery is proposed: the proposal ends it.
    t.c.start_session(&1_800_000_600);
    let new_key = SigningKey::from_slice(&[11u8; 32]).unwrap();
    let new = signer_of(&t.env, &new_key);
    t.c.recover_by_sms(&new, &t.leaders[0]);
    assert_eq!(t.c.session(), None);

    // Frozen: the old signer draws nothing, withdraws nothing, starts no session.
    for ctx in [&draw, &withdraw] {
        assert_eq!(
            check(&t, [1; 32], sign(&t.env, &t.key, &[1; 32]), ctx),
            Err(Ok(Error::Frozen))
        );
        assert_eq!(
            check(&t, [1; 32], lead.clone(), ctx),
            Err(Ok(Error::Frozen))
        );
    }
    assert_eq!(
        t.c.try_start_session(&1_800_000_600),
        Err(Ok(Error::Frozen))
    );
    assert!(t.c.try_set_signer(&signer_of(&t.env, &t.key)).is_err());
    // The new key cannot sign yet either.
    assert_eq!(
        check(&t, [1; 32], sign(&t.env, &new_key, &[1; 32]), &draw),
        Err(Ok(Error::Frozen))
    );
    // A second proposal (another leader, another key) cannot replace it.
    let other = signer_of(&t.env, &SigningKey::from_slice(&[13u8; 32]).unwrap());
    assert_eq!(
        t.c.try_recover_by_sms(&other, &t.leaders[1]),
        Err(Ok(Error::Frozen))
    );
    assert_eq!(t.c.pending().unwrap().signer, new);

    // After the wait it executes; the new signer draws on the same address.
    t.env.ledger().set_timestamp(1_800_000_000 + 172_800);
    t.c.execute_recovery();
    assert_eq!(t.c.pending(), None);
    assert_eq!(
        check(&t, [3; 32], sign(&t.env, &new_key, &[3; 32]), &draw),
        Ok(())
    );
    assert!(check(&t, [3; 32], sign(&t.env, &t.key, &[3; 32]), &draw).is_err());
    assert_eq!(t.c.start_session(&1_800_172_900), 1_800_172_800 + 300);
}

// ---------- the LP pledge (30 Oct) ----------

struct P {
    t: T,
    pool: Address,
    usdc: Address,
}

/// The union names fx_pool (the mock) and a USDC asset; the account holds 50 USDC.
fn pledge() -> P {
    let t = setup();
    let pool = t.env.register(mocks::Pool, ());
    let usdc = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env))
        .address();
    let mut st = t.settings();
    st.fx_pool = Some(pool.clone());
    st.usdc = Some(usdc.clone());
    t.set_settings(&st);
    soroban_sdk::token::StellarAssetClient::new(&t.env, &usdc).mint(&t.acct, &50);
    P { t, pool, usdc }
}

fn usdc_call(env: &Env, usdc: &Address, f: &str) -> Vec<Context> {
    vec![
        env,
        Context::Contract(ContractContext {
            contract: usdc.clone(),
            fn_name: soroban_sdk::Symbol::new(env, f),
            args: vec![env],
        }),
    ]
}

/// Review 5 Nov: with no union named (so no USDC to guard) an approve is still refused, so no
/// allowance can wait for a later lien.
#[test]
fn no_approve_even_with_no_union() {
    let t = bare();
    let token = Address::generate(&t.env);
    assert_eq!(
        check(
            &t,
            [1; 32],
            sign(&t.env, &t.key, &[1; 32]),
            &usdc_call(&t.env, &token, "approve")
        ),
        Err(Ok(Error::Locked))
    );
}

#[test]
fn only_fx_pool_locks_and_never_above_her_free_usdc() {
    let p = pledge();
    let t = &p.t;
    assert_eq!(t.c.lock(&30), 30);
    let auths = t.env.auths();
    assert_eq!(auths[0].0, p.pool);
    assert_eq!(t.c.try_lock(&21), Err(Ok(Error::Locked)));
    assert_eq!(t.c.try_lock(&0), Err(Ok(Error::BadAmount)));
    assert_eq!(t.c.lock(&20), 50);
    assert_eq!(t.c.unlock(&45), 5);
    assert_eq!(t.c.unlock(&99), 0);
    // Nobody else.
    t.env.set_auths(&[]);
    assert!(t.c.try_lock(&1).is_err());
    assert!(t.c.try_unlock(&1).is_err());
}

#[test]
fn while_a_lien_stands_usdc_leaves_only_by_send_usdc_within_the_free_part() {
    let p = pledge();
    let t = &p.t;
    let k = |n: u8| sign(&t.env, &t.key, &[n; 32]);
    let transfer = usdc_call(&t.env, &p.usdc, "transfer");
    // No lien: her signer may move USDC directly; an approve never.
    assert_eq!(check(t, [1; 32], k(1), &transfer), Ok(()));
    assert_eq!(
        check(t, [1; 32], k(1), &usdc_call(&t.env, &p.usdc, "approve")),
        Err(Ok(Error::Locked))
    );
    t.c.lock(&30);
    for f in ["transfer", "burn", "approve"] {
        assert_eq!(
            check(t, [1; 32], k(1), &usdc_call(&t.env, &p.usdc, f)),
            Err(Ok(Error::Locked))
        );
    }
    // Other contracts are unaffected.
    assert_eq!(
        check(t, [1; 32], k(1), &call_on(&t.env, &t.fund_core)),
        Ok(())
    );
    // No approve on any contract, USDC or not (review 5 Nov).
    let other_token = Address::generate(&t.env);
    assert_eq!(
        check(
            t,
            [1; 32],
            k(1),
            &usdc_call(&t.env, &other_token, "approve")
        ),
        Err(Ok(Error::Locked))
    );
    // Her free 20 go out with send_usdc; more does not.
    let to = Address::generate(&t.env);
    assert_eq!(t.c.try_send_usdc(&to, &21), Err(Ok(Error::Locked)));
    t.c.send_usdc(&to, &20);
    let tok = soroban_sdk::token::TokenClient::new(&t.env, &p.usdc);
    assert_eq!((tok.balance(&t.acct), tok.balance(&to)), (30, 20));
    assert_eq!(t.c.try_send_usdc(&to, &1), Err(Ok(Error::Locked)));
}

#[test]
fn only_fx_pool_takes_locked_usdc_and_never_above_the_lien() {
    let p = pledge();
    let t = &p.t;
    t.c.lock(&33);
    assert_eq!(t.c.try_pay(&34), Err(Ok(Error::NotClaimable)));
    assert_eq!(t.c.try_pay(&0), Err(Ok(Error::NotClaimable)));
    // A frozen account (a recovery pending) still pays a Collect it accepted.
    sms_on(t);
    t.c.recover_by_sms(&signer_of(&t.env, &t.key), &t.leaders[0]);
    assert_eq!(t.c.pay(&30), 3);
    let auths = t.env.auths();
    assert_eq!(auths[0].0, p.pool);
    let tok = soroban_sdk::token::TokenClient::new(&t.env, &p.usdc);
    assert_eq!((tok.balance(&t.acct), tok.balance(&p.pool)), (20, 30));
    assert_eq!(t.c.lien(), 3);
    // Nobody else.
    t.env.set_auths(&[]);
    assert!(t.c.try_pay(&1).is_err());
}

/// A lost keypad phone (executive decision 2 Nov): one leader and the node's SMS key together
/// propose the new device key; both must sign, the leader must lead the union, and the wait and
/// the freeze apply.
#[test]
fn one_leader_and_the_sms_key_recover_a_keypad_phone() {
    let t = setup();
    let new_key = SigningKey::from_slice(&[12u8; 32]).unwrap();
    let new = signer_of(&t.env, &new_key);
    assert_eq!(
        t.c.try_recover_by_sms(&new, &t.leaders[0]),
        Err(Ok(Error::NoSmsKey))
    );
    let sms = sms_on(&t);
    assert_eq!(
        t.c.try_recover_by_sms(&new, &Address::generate(&t.env)),
        Err(Ok(Error::NotLeaders))
    );
    t.c.recover_by_sms(&new, &t.leaders[0]);
    // Both the leader and the SMS key authorized it.
    let auths = t.env.auths();
    assert!(auths.iter().any(|(a, _)| *a == t.leaders[0]));
    assert!(auths.iter().any(|(a, _)| *a == sms));
    // Frozen until the wait has passed; a second proposal is refused.
    assert_eq!(
        t.c.try_recover_by_sms(&new, &t.leaders[1]),
        Err(Ok(Error::Frozen))
    );
    assert_eq!(t.c.try_execute_recovery(), Err(Ok(Error::NotReady)));
    t.env.ledger().set_timestamp(1_800_000_000 + 172_800);
    t.c.execute_recovery();
    assert_eq!(t.c.signer(), new);
}

/// The one path takes either signer type and changes type (executive decision 4 Nov): a keypad
/// member (P-256) recovers onto a smartphone's new Privy key (Ed25519), and back onto a new keypad
/// device key; each new key signs after the wait, the old one does not.
#[test]
fn sms_recovery_takes_either_signer_type() {
    let t = setup();
    sms_on(&t);
    let ctx = call_on(&t.env, &t.fund_core);
    // P-256 → Ed25519 (a new smartphone).
    let ed = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
    t.c.recover_by_sms(&ed_signer(&t.env, &ed), &t.leaders[0]);
    assert_eq!(t.c.try_execute_recovery(), Err(Ok(Error::NotReady)));
    t.env.ledger().set_timestamp(1_800_000_000 + 172_800);
    t.c.execute_recovery();
    assert_eq!(t.c.signer(), ed_signer(&t.env, &ed));
    assert_eq!(
        check(&t, [4; 32], ed_sign(&t.env, &ed, &[4; 32]), &ctx),
        Ok(())
    );
    assert_eq!(
        check(&t, [4; 32], sign(&t.env, &t.key, &[4; 32]), &ctx),
        Err(Ok(Error::WrongKind))
    );
    // Ed25519 → P-256 (a new keypad device key), proposed by the other leader.
    let kp = SigningKey::from_slice(&[14u8; 32]).unwrap();
    t.c.recover_by_sms(&signer_of(&t.env, &kp), &t.leaders[1]);
    assert_eq!(
        check(&t, [5; 32], ed_sign(&t.env, &ed, &[5; 32]), &ctx),
        Err(Ok(Error::Frozen))
    );
    assert_eq!(t.c.try_execute_recovery(), Err(Ok(Error::NotReady)));
    t.env.ledger().set_timestamp(1_800_000_000 + 2 * 172_800);
    t.c.execute_recovery();
    assert_eq!(t.c.signer(), signer_of(&t.env, &kp));
    assert_eq!(
        check(&t, [5; 32], sign(&t.env, &kp, &[5; 32]), &ctx),
        Ok(())
    );
    assert_eq!(
        check(&t, [5; 32], ed_sign(&t.env, &ed, &[5; 32]), &ctx),
        Err(Ok(Error::WrongKind))
    );
}

/// "Move to a keypad phone": her own signature changes the signer's type (no recovery, no wait).
#[test]
fn her_own_signature_moves_a_smartphone_to_a_keypad() {
    let t = setup();
    let ed = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
    t.c.set_signer(&ed_signer(&t.env, &ed));
    let kp = SigningKey::from_slice(&[14u8; 32]).unwrap();
    t.c.set_signer(&signer_of(&t.env, &kp));
    let auths = t.env.auths();
    assert_eq!(auths[0].0, t.acct);
    let ctx = call_on(&t.env, &t.fund_core);
    assert_eq!(
        check(&t, [6; 32], sign(&t.env, &kp, &[6; 32]), &ctx),
        Ok(())
    );
    assert_eq!(
        check(&t, [6; 32], ed_sign(&t.env, &ed, &[6; 32]), &ctx),
        Err(Ok(Error::WrongKind))
    );
}

// ---------- member-owned accounts (executive decision 3 Nov) ----------

/// The member names her union with her own signature; nobody else can.
#[test]
fn set_union_needs_her_own_signature() {
    let t = bare();
    assert_eq!(t.c.union(), None);
    t.env.set_auths(&[]);
    assert!(t.c.try_set_union(&t.union_ms).is_err());
    t.env.mock_all_auths();
    t.c.set_union(&t.union_ms);
    let auths = t.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, t.acct);
    assert_eq!(t.c.union(), Some(t.union_ms.clone()));
    // Her own P-256 signature passes __check_auth for a call on the account itself.
    let ctx = call_named(
        &t.env,
        &t.acct,
        soroban_sdk::Symbol::new(&t.env, "set_union"),
    );
    assert_eq!(
        check(&t, [6; 32], sign(&t.env, &t.key, &[6; 32]), &ctx),
        Ok(())
    );
}

/// Decision 008: she can name only a union with settings, so never one the trust has not
/// admitted, nor an admitted one that has set no settings yet.
#[test]
fn set_union_refuses_a_union_without_settings() {
    let t = bare();
    let never = Address::generate(&t.env);
    assert_eq!(t.c.try_set_union(&never), Err(Ok(Error::NoSettings)));
    let admitted = Address::generate(&t.env);
    t.roles
        .admit_union(&admitted, &BytesN::from_array(&t.env, &[4; 32]));
    assert_eq!(t.c.try_set_union(&admitted), Err(Ok(Error::NoSettings)));
    assert_eq!(t.c.union(), None);
    t.roles.set_union_settings(&admitted, &t.settings());
    t.c.set_union(&admitted);
    assert_eq!(t.c.union(), Some(admitted.clone()));
    // Revoked: refused again, also as a change back.
    t.c.set_union(&t.union_ms);
    t.roles.revoke_union(&admitted);
    assert_eq!(t.c.try_set_union(&admitted), Err(Ok(Error::NoSettings)));
}

/// No change of union while frozen, in a session, or with a lien.
#[test]
fn set_union_is_refused_when_frozen_in_a_session_or_with_a_lien() {
    let p = pledge();
    let t = &p.t;
    let other = t.other_union();
    t.c.start_session(&1_800_000_600);
    assert_eq!(t.c.try_set_union(&other), Err(Ok(Error::InSession)));
    t.c.end_session();
    t.c.lock(&10);
    assert_eq!(t.c.try_set_union(&other), Err(Ok(Error::Locked)));
    t.c.unlock(&10);
    sms_on(t);
    t.c.recover_by_sms(&signer_of(&t.env, &t.key), &t.leaders[0]);
    assert_eq!(t.c.try_set_union(&other), Err(Ok(Error::Frozen)));
    t.env.ledger().set_timestamp(1_800_000_000 + 172_800);
    t.c.execute_recovery();
    t.c.set_union(&other);
    finish(t);
    assert_eq!(t.c.union(), Some(other));
}

/// With no union named, nobody recovers and no session starts.
#[test]
fn no_union_no_recovery_and_no_session() {
    let t = bare();
    let new = signer_of(&t.env, &SigningKey::from_slice(&[11u8; 32]).unwrap());
    assert_eq!(
        t.c.try_recover_by_sms(&new, &t.leaders[0]),
        Err(Ok(Error::NoUnion))
    );
    assert_eq!(
        t.c.try_start_session(&1_800_000_600),
        Err(Ok(Error::NoUnion))
    );
    assert_eq!(
        check(
            &t,
            [1; 32],
            Signature::Leader(t.leaders[0].clone()),
            &call_on(&t.env, &t.fund_core)
        ),
        Err(Ok(Error::NoSession))
    );
    // Her own signature still works: the account is hers with or without a union.
    assert_eq!(
        check(
            &t,
            [1; 32],
            sign(&t.env, &t.key, &[1; 32]),
            &call_on(&t.env, &t.fund_core)
        ),
        Ok(())
    );
    // A union the trust revoked after she named it: its settings read as None, so no
    // recovery, no session (never a zero wait).
    let bare_union = t.other_union();
    t.roles.set_leader(&bare_union, &t.leaders[0], &true);
    t.roles.set_leader(&bare_union, &t.leaders[1], &true);
    t.c.set_union(&bare_union);
    t.roles.revoke_union(&bare_union);
    assert_eq!(
        t.c.try_recover_by_sms(&new, &t.leaders[0]),
        Err(Ok(Error::NoSettings))
    );
    assert_eq!(
        t.c.try_start_session(&1_800_000_600),
        Err(Ok(Error::NoSettings))
    );
    assert_eq!(t.c.pending(), None);
}

/// Recovery and sessions follow the settings and leaders of the union she named.
#[test]
fn recovery_and_sessions_use_the_named_unions_settings() {
    let t = setup();
    let b = Address::generate(&t.env);
    let b_leaders = [Address::generate(&t.env), Address::generate(&t.env)];
    t.roles
        .admit_union(&b, &BytesN::from_array(&t.env, &[3; 32]));
    for l in &b_leaders {
        t.roles.set_leader(&b, l, &true);
    }
    t.roles.set_union_settings(
        &b,
        &UnionSettings {
            wait: 3_600,
            session_len: 60,
            session_contracts: vec![&t.env, t.fund_core.clone()],
            sms_key: Some(Address::generate(&t.env)),
            fx_pool: None,
            usdc: None,
        },
    );
    t.c.set_union(&b);
    finish(&t);
    let now = t.env.ledger().timestamp();
    assert_eq!(t.c.start_session(&(now + 600)), now + 60);
    let ctx = call_on(&t.env, &t.fund_core);
    // Union A's leaders no longer sign for her; union B's do.
    assert_eq!(
        check(&t, [1; 32], Signature::Leader(t.leaders[0].clone()), &ctx),
        Err(Ok(Error::NoSession))
    );
    assert_eq!(
        check(&t, [1; 32], Signature::Leader(b_leaders[0].clone()), &ctx),
        Ok(())
    );
    t.c.end_session();
    // A's leaders cannot recover; B's can, with B's SMS key and B's one-hour wait.
    let new_key = SigningKey::from_slice(&[11u8; 32]).unwrap();
    let new = signer_of(&t.env, &new_key);
    assert_eq!(
        t.c.try_recover_by_sms(&new, &t.leaders[0]),
        Err(Ok(Error::NotLeaders))
    );
    let b_sms = t.roles.union_settings(&b).unwrap().sms_key.unwrap();
    t.c.recover_by_sms(&new, &b_leaders[0]);
    assert!(t.env.auths().iter().any(|(a, _)| *a == b_sms));
    assert_eq!(t.c.pending().unwrap().at, now + 3_600);
    t.env.ledger().set_timestamp(now + 3_600);
    t.c.execute_recovery();
    assert_eq!(t.c.signer(), new);
}

/// Only she moves her account to new code: no stranger, no leader session (even if the union
/// lists the account among its session contracts), not while frozen, in a session or liened.
#[test]
fn upgrade_needs_her_own_signature() {
    let p = pledge();
    let t = &p.t;
    let h = BytesN::from_array(&t.env, &[9; 32]);
    // A stranger: no auth.
    t.env.set_auths(&[]);
    assert!(t.c.try_upgrade(&h).is_err());
    t.env.mock_all_auths();
    // A leader session never reaches the account itself, even if the trust wrongly gave it the
    // core role and the union listed it (roles refuses the list otherwise).
    let mut st = t.settings();
    t.roles.set_role(&t.acct, &nila_roles::Role::Core, &true);
    st.session_contracts.push_back(t.acct.clone());
    t.set_settings(&st);
    t.c.start_session(&1_800_000_600);
    let up = call_named(&t.env, &t.acct, soroban_sdk::Symbol::new(&t.env, "upgrade"));
    assert_eq!(
        check(t, [1; 32], Signature::Leader(t.leaders[0].clone()), &up),
        Err(Ok(Error::OutsideSession))
    );
    let su = call_named(
        &t.env,
        &t.acct,
        soroban_sdk::Symbol::new(&t.env, "set_union"),
    );
    assert_eq!(
        check(t, [1; 32], Signature::Leader(t.leaders[0].clone()), &su),
        Err(Ok(Error::OutsideSession))
    );
    // Nor the USDC, even if listed (and given the core role).
    t.roles.set_role(&p.usdc, &nila_roles::Role::Core, &true);
    st.session_contracts.push_back(p.usdc.clone());
    t.set_settings(&st);
    assert_eq!(
        check(
            t,
            [1; 32],
            Signature::Leader(t.leaders[0].clone()),
            &usdc_call(&t.env, &p.usdc, "transfer")
        ),
        Err(Ok(Error::OutsideSession))
    );
    assert_eq!(t.c.try_upgrade(&h), Err(Ok(Error::InSession)));
    t.c.end_session();
    t.c.lock(&5);
    assert_eq!(t.c.try_upgrade(&h), Err(Ok(Error::Locked)));
    t.c.unlock(&5);
    // Her own signature passes __check_auth for it.
    assert_eq!(
        check(t, [2; 32], sign(&t.env, &t.key, &[2; 32]), &up),
        Ok(())
    );
    sms_on(t);
    t.c.recover_by_sms(&signer_of(&t.env, &t.key), &t.leaders[0]);
    assert_eq!(t.c.try_upgrade(&h), Err(Ok(Error::Frozen)));
}

/// With the built account wasm: her signed upgrade moves the account to it, on the same address.
#[test]
fn her_upgrade_moves_the_account_to_new_code() {
    extern crate std;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wasm32v1-none/release/nila_account.wasm"
    );
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) if std::env::var("NILA_SKIP_WASM").is_err() => {
            panic!("{path} missing: build the wasm first (or set NILA_SKIP_WASM=1)")
        }
        Err(_) => return,
    };
    let t = setup();
    let h = t
        .env
        .deployer()
        .upload_contract_wasm(soroban_sdk::Bytes::from_slice(&t.env, &bytes));
    t.c.upgrade(&h);
    let auths = t.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, t.acct);
    finish(&t);
    assert_eq!(t.c.try_upgrade(&h), Err(Ok(Error::Current)));
    assert_eq!(t.c.union(), Some(t.union_ms.clone()));
}

/// The lien's fx_pool is fixed at the lock that opened it: a union that names another fx_pool
/// meanwhile does not move who unlocks or takes it; at 0 the record goes.
#[test]
fn the_liens_fx_pool_is_fixed_at_the_first_lock() {
    let p = pledge();
    let t = &p.t;
    t.c.lock(&30);
    assert_eq!(t.c.lien_pool(), Some(p.pool.clone()));
    let pool2 = t.env.register(mocks::Pool, ());
    let mut st = t.settings();
    st.fx_pool = Some(pool2.clone());
    t.set_settings(&st);
    t.c.lock(&5);
    assert_eq!(t.env.auths()[0].0, p.pool);
    t.c.unlock(&10);
    assert_eq!(t.env.auths()[0].0, p.pool);
    assert_eq!(t.c.pay(&20), 5);
    assert_eq!(t.env.auths()[0].0, p.pool);
    let tok = soroban_sdk::token::TokenClient::new(&t.env, &p.usdc);
    assert_eq!(tok.balance(&p.pool), 20);
    t.c.unlock(&5);
    assert_eq!(t.c.lien_pool(), None);
    t.c.lock(&1);
    assert_eq!(t.env.auths()[0].0, pool2);
    assert_eq!(t.c.lien_pool(), Some(pool2));
}

/// Review 5 Nov (finding 6): once a union is named, a change of union or code waits the union's recovery
/// wait, and a recovery proposed meanwhile cancels it, so a stolen key cannot move the account out of
/// its union's reach. Her first union applies at once.
#[test]
fn a_move_waits_and_a_recovery_cancels_it() {
    let t = setup();
    let other = t.other_union();
    let wait = t.settings().wait;
    // The thief, with her key, asks to move her to another union: nothing changes yet.
    t.c.set_union(&other);
    assert_eq!(t.c.union(), Some(t.union_ms.clone()));
    let m = t.c.pending_move().unwrap();
    assert_eq!(m.at, t.env.ledger().timestamp() + wait);
    assert_eq!(t.c.try_finish_move(), Err(Ok(Error::NotReady)));
    // Her leader and the SMS key start a recovery: the move is cancelled.
    sms_on(&t);
    let new_key = SigningKey::from_slice(&[42u8; 32]).unwrap();
    t.c.recover_by_sms(&signer_of(&t.env, &new_key), &t.leaders[0]);
    assert_eq!(t.c.pending_move(), None);
    t.env.ledger().set_timestamp(m.at);
    assert_eq!(t.c.try_finish_move(), Err(Ok(Error::NotReady)));
    t.c.execute_recovery();
    assert_eq!(t.c.union(), Some(t.union_ms.clone()));
    assert_eq!(t.c.signer(), signer_of(&t.env, &new_key));
    // Unchallenged, the move applies after the wait (anyone may finish it).
    t.c.set_union(&other);
    finish(&t);
    assert_eq!(t.c.union(), Some(other));
    assert_eq!(t.c.pending_move(), None);
}
