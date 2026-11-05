#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn setup() -> (Env, Address, DeployerClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let roles = Address::generate(&env);
    let id = env.register(
        Deployer,
        (BytesN::from_array(&env, &[1; 32]), roles.clone()),
    );
    (env.clone(), roles, DeployerClient::new(&env, &id))
}

/// The account wasm built by `cargo build --release --target wasm32v1-none` (CI builds it before
/// the tests). Missing, the deploy tests fail (review 5 Nov: they passed without running); set
/// NILA_SKIP_WASM=1 to skip them on purpose.
fn account_wasm() -> Option<std::vec::Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wasm32v1-none/release/nila_account.wasm"
    );
    match std::fs::read(path) {
        Ok(b) => Some(b),
        Err(_) if std::env::var("NILA_SKIP_WASM").is_err() => {
            panic!("{path} missing: build the wasm first (or set NILA_SKIP_WASM=1)")
        }
        Err(_) => {
            std::eprintln!("skipped: {path} not built");
            None
        }
    }
}

fn p256(env: &Env, b: u8) -> Signer {
    Signer::P256(BytesN::from_array(env, &[b; 65]))
}

/// One key, one address, known before the account exists; the deployer keeps what it was given.
#[test]
fn the_address_follows_from_the_key() {
    let (env, roles, d) = setup();
    assert_eq!(d.address_of(&p256(&env, 4)), d.address_of(&p256(&env, 4)));
    assert_ne!(d.address_of(&p256(&env, 4)), d.address_of(&p256(&env, 5)));
    let ed = Signer::Ed25519(BytesN::from_array(&env, &[4; 32]));
    assert_ne!(d.address_of(&ed), d.address_of(&p256(&env, 4)));
    assert_eq!(d.roles(), roles);
    assert_eq!(d.account_wasm(), BytesN::from_array(&env, &[1; 32]));
}

/// `create` deploys at `address_of`, with the deployer's roles, the key as signer and no union;
/// the address does not depend on any union, and a key's account cannot be created twice.
#[test]
fn create_deploys_at_address_of_with_no_union() {
    let Some(bytes) = account_wasm() else { return };
    let env = Env::default();
    env.mock_all_auths();
    let hash = env
        .deployer()
        .upload_contract_wasm(soroban_sdk::Bytes::from_slice(&env, &bytes));
    let roles = env.register(nila_roles::RolesContract, (Address::generate(&env),));
    let id = env.register(Deployer, (hash.clone(), roles.clone()));
    let d = DeployerClient::new(&env, &id);
    let s = Signer::Ed25519(BytesN::from_array(&env, &[7; 32]));
    let expected = d.address_of(&s);
    let acct = d.create(&s);
    assert_eq!(acct, expected);
    let a = nila_account::AccountContractClient::new(&env, &acct);
    assert_eq!(a.roles(), roles);
    assert_eq!(a.union(), None);
    assert_eq!(
        a.signer(),
        nila_account::Signer::Ed25519(BytesN::from_array(&env, &[7; 32]))
    );
    // Naming a union (admitted, with settings: decision 008) changes nothing about the address.
    let union = Address::generate(&env);
    let r = nila_roles::RolesContractClient::new(&env, &roles);
    r.admit_union(&union, &BytesN::from_array(&env, &[2; 32]));
    r.set_union_settings(
        &union,
        &nila_roles::UnionSettings {
            wait: nila_roles::WAIT_MIN,
            session_len: nila_roles::SESSION_MIN,
            session_contracts: soroban_sdk::vec![&env],
            sms_key: None,
            fx_pool: None,
            usdc: None,
        },
    );
    a.set_union(&union);
    assert_eq!(a.union(), Some(union));
    assert_eq!(d.address_of(&s), acct);
    // One key, one account.
    assert!(d.try_create(&s).is_err());
}
