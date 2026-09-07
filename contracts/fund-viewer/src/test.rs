#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String};

#[test]
fn union_and_fund_types() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(FundViewerContract, (admin.clone(),));
    let client = FundViewerContractClient::new(&env, &id);
    let union = Address::generate(&env);
    client.create_union(&union, &String::from_str(&env, "Mother Theresa"));
    let rec = client.get_union(&union).unwrap();
    assert!(rec.active);

    let lt = BytesN::from_array(&env, &[1u8; 32]);
    client.add_fund_type(&union, &lt, &String::from_str(&env, "paddy"));
    assert_eq!(client.get_fund_types(&union).len(), 1);
    client.remove_fund_type(&union, &0);
    assert_eq!(client.get_fund_types(&union).len(), 0);
}

#[test]
fn borrower_index_from_core() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let core = Address::generate(&env);
    let id = env.register(FundViewerContract, (admin.clone(),));
    let client = FundViewerContractClient::new(&env, &id);
    client.set_core(&core);

    let union = Address::generate(&env);
    let borrower = Address::generate(&env);
    let loan = BytesN::from_array(&env, &[9u8; 32]);
    client.on_loan_created(&union, &loan, &borrower);
    assert_eq!(client.get_loans_by_borrower(&union, &borrower).len(), 1);
    client.on_loan_closed(&union, &loan, &borrower);
    assert_eq!(client.get_loans_by_borrower(&union, &borrower).len(), 0);
}
