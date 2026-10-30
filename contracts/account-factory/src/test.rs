#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, testutils::Ledger, vec, Address, BytesN, Env};

fn setup() -> (Env, AccountFactoryClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let a = |e: &Env| Address::generate(e);
    let id = env.register(
        AccountFactory,
        (
            a(&env),
            a(&env),
            1u64,
            99_999u64,
            vec![&env, a(&env)],
            BytesN::from_array(&env, &[1; 32]),
        ),
    );
    (env.clone(), AccountFactoryClient::new(&env, &id))
}

/// The settings stay inside their bounds, set by the union's multisig.
#[test]
fn settings_stay_in_bounds() {
    let (env, f) = setup();
    assert_eq!(f.wait(), WAIT_MIN);
    assert_eq!(f.session_len(), SESSION_MAX);
    assert_eq!(f.try_set_wait(&60), Err(Ok(Error::BadSetting)));
    f.set_wait(&172_800);
    assert_eq!(
        f.try_set_session(&1_000, &vec![&env]),
        Err(Ok(Error::BadSetting))
    );
    f.set_session(&300, &vec![&env]);
    assert_eq!(f.session_len(), 300);
}

/// A new account code applies only after the timelock.
#[test]
fn a_new_account_code_waits_for_the_timelock() {
    let (env, f) = setup();
    let h = BytesN::from_array(&env, &[2; 32]);
    f.propose_account_wasm(&h);
    assert_eq!(f.try_apply_account_wasm(), Err(Ok(Error::NotReady)));
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + UPGRADE_DELAY);
    f.apply_account_wasm();
    assert_eq!(f.account_wasm(), h);
}

/// One key, one address, known before the account exists.
#[test]
fn the_address_follows_from_the_key() {
    let (env, f) = setup();
    let s = |b: u8| Signer::P256(BytesN::from_array(&env, &[b; 65]));
    assert_eq!(f.address_of(&s(4)), f.address_of(&s(4)));
    assert_ne!(f.address_of(&s(4)), f.address_of(&s(5)));
}
