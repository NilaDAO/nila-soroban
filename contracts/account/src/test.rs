#![cfg(test)]

use super::*;
use p256::ecdsa::{signature::hazmat::PrehashSigner, Signature as EcSig, SigningKey};
use soroban_sdk::{
    auth::ContractContext, symbol_short, testutils::Address as _, testutils::Ledger, vec, Address,
    BytesN, Env, IntoVal,
};

/// Stand-ins: a factory with fixed settings, and roles where `leaders` lead `union`.
mod mocks {
    use soroban_sdk::{contract, contractimpl, contracttype, Address, BytesN, Env, Vec};
    #[contracttype]
    pub enum K {
        Roles,
        Recovery,
        Allowed,
        Union,
        Leaders,
    }
    #[contract]
    pub struct Factory;
    #[contractimpl]
    impl Factory {
        pub fn __constructor(env: Env, roles: Address, recovery: Address, allowed: Vec<Address>) {
            env.storage().instance().set(&K::Roles, &roles);
            env.storage().instance().set(&K::Recovery, &recovery);
            env.storage().instance().set(&K::Allowed, &allowed);
        }
        pub fn union(env: Env) -> Address {
            env.storage().instance().get(&K::Recovery).unwrap()
        }
        pub fn roles(env: Env) -> Address {
            env.storage().instance().get(&K::Roles).unwrap()
        }
        pub fn wait(_env: Env) -> u64 {
            172_800
        }
        pub fn session_len(_env: Env) -> u64 {
            300
        }
        pub fn session_contracts(env: Env) -> Vec<Address> {
            env.storage().instance().get(&K::Allowed).unwrap()
        }
        pub fn account_wasm(env: Env) -> BytesN<32> {
            BytesN::from_array(&env, &[0; 32])
        }
    }
    #[contract]
    pub struct Roles;
    #[contractimpl]
    impl Roles {
        pub fn __constructor(env: Env, union: Address, leaders: Vec<Address>) {
            env.storage().instance().set(&K::Union, &union);
            env.storage().instance().set(&K::Leaders, &leaders);
        }
        pub fn is_leader(env: Env, union: Address, account: Address) -> bool {
            env.storage().instance().get::<_, Address>(&K::Union) == Some(union)
                && env
                    .storage()
                    .instance()
                    .get::<_, Vec<Address>>(&K::Leaders)
                    .unwrap()
                    .contains(&account)
        }
    }
}

struct T {
    env: Env,
    acct: Address,
    c: AccountContractClient<'static>,
    key: SigningKey,
    leaders: [Address; 2],
    fund_core: Address,
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

fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_800_000_000);
    let union = Address::generate(&env);
    let leaders = [Address::generate(&env), Address::generate(&env)];
    let fund_core = Address::generate(&env);
    let roles = env.register(
        mocks::Roles,
        (
            union.clone(),
            vec![&env, leaders[0].clone(), leaders[1].clone()],
        ),
    );
    let factory = env.register(
        mocks::Factory,
        (
            roles,
            Address::generate(&env),
            vec![&env, fund_core.clone()],
        ),
    );
    let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
    let acct = env.register(AccountContract, (factory, union, signer_of(&env, &key)));
    let c = AccountContractClient::new(&env, &acct);
    T {
        env,
        acct,
        c,
        key,
        leaders,
        fund_core,
    }
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
    let new_key = SigningKey::from_slice(&[11u8; 32]).unwrap();
    let new = signer_of(&t.env, &new_key);
    t.c.recover(&new);
    assert_eq!(t.c.try_execute_recovery(), Err(Ok(Error::NotReady)));
    // The member cancels from the old phone.
    t.c.cancel_recovery();
    assert_eq!(t.c.pending(), None);
    // Two leaders of the union, the number lost.
    assert_eq!(
        t.c.try_recover_by_leaders(&new, &t.leaders[0], &t.leaders[0]),
        Err(Ok(Error::NotLeaders))
    );
    assert_eq!(
        t.c.try_recover_by_leaders(&new, &t.leaders[0], &Address::generate(&t.env)),
        Err(Ok(Error::NotLeaders))
    );
    t.c.recover_by_leaders(&new, &t.leaders[0], &t.leaders[1]);
    t.env.ledger().set_timestamp(1_800_000_000 + 172_800);
    t.c.execute_recovery();
    assert_eq!(t.c.signer(), new);
    let ctx = call_on(&t.env, &t.fund_core);
    assert_eq!(
        check(&t, [3; 32], sign(&t.env, &new_key, &[3; 32]), &ctx),
        Ok(())
    );
    assert!(check(&t, [3; 32], sign(&t.env, &t.key, &[3; 32]), &ctx).is_err());
}
