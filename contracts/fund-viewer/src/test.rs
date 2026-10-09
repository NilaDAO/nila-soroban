#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env, String};

#[test]
fn union_registry() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(FundViewerContract, (admin.clone(),));
    let client = FundViewerContractClient::new(&env, &id);
    let union = Address::generate(&env);
    client.create_union(&union, &String::from_str(&env, "Mother Theresa"));
    let rec = client.get_union(&union).unwrap();
    assert!(rec.active);

    client.activate_union(&union, &false);
    assert!(!client.get_union(&union).unwrap().active);
}
