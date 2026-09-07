#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

fn setup() -> (Env, Address, RolesContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(RolesContract, (admin.clone(),));
    let client = RolesContractClient::new(&env, &id);
    (env, admin, client)
}

#[test]
fn oracle_and_leader_round_trip() {
    let (env, admin, client) = setup();
    assert_eq!(client.admin(), admin);

    let oracle = Address::generate(&env);
    let union = Address::generate(&env);
    let leader = Address::generate(&env);

    assert!(!client.is_oracle(&oracle));
    client.set_oracle(&oracle, &true);
    assert!(client.is_oracle(&oracle));
    client.set_oracle(&oracle, &false);
    assert!(!client.is_oracle(&oracle));

    assert!(!client.is_leader(&union, &leader));
    client.set_leader(&union, &leader, &true);
    assert!(client.is_leader(&union, &leader));
}

#[test]
fn set_oracle_requires_admin() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let id = env.register(RolesContract, (admin.clone(),));
    let client = RolesContractClient::new(&env, &id);
    let oracle = Address::generate(&env);

    let err = client.try_set_oracle(&oracle, &true).unwrap_err();
    assert!(err.is_err());
}
