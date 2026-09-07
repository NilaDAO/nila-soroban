#![cfg(test)]

extern crate std;

use super::*;
use nila_roles::RolesContract;
use soroban_sdk::{testutils::Address as _, vec, Address, BytesN, Env};

fn setup() -> (
    Env,
    Address,
    Address,
    Address,
    Address,
    MembershipContractClient<'static>,
) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let attester = Address::generate(&env);
    let roles_id = env.register(RolesContract, (admin.clone(),));
    let roles = nila_roles::RolesContractClient::new(&env, &roles_id);
    let union = Address::generate(&env);
    let leader = Address::generate(&env);
    roles.set_leader(&union, &leader, &true);

    let id = env.register(MembershipContract, (admin.clone(), attester.clone()));
    let client = MembershipContractClient::new(&env, &id);
    client.set_roles(&roles_id);
    (env, admin, attester, union, leader, client)
}

#[test]
fn confirm_and_gate() {
    let (env, _admin, _attester, union, leader, client) = setup();
    let member = Address::generate(&env);
    let poly = BytesN::from_array(&env, &[0x11; 20]);
    let plots = vec![&env, 42u64, 43u64];

    assert!(!client.is_member(&union, &member));
    client.confirm_member(&union, &member, &leader, &poly, &plots, &1);
    assert!(client.is_member(&union, &member));
    assert_eq!(client.plots_of(&union, &member), plots);

    client.revoke_member(&union, &member, &leader);
    assert!(!client.is_member(&union, &member));
}

#[test]
fn reject_too_many_plots() {
    let (env, _admin, _attester, union, leader, client) = setup();
    let member = Address::generate(&env);
    let poly = BytesN::from_array(&env, &[0x11; 20]);
    let mut plots = vec![&env];
    for i in 0..33u64 {
        plots.push_back(i);
    }
    let err = client
        .try_confirm_member(&union, &member, &leader, &poly, &plots, &1)
        .unwrap_err();
    assert_eq!(err, Ok(Error::TooManyPlots));
}
