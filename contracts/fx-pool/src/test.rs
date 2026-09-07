#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

#[test]
fn escrow_resolve_by_core() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc = Address::generate(&env);
    let nin = Address::generate(&env);
    let core = Address::generate(&env);
    let id = env.register(
        FxPoolContract,
        (admin.clone(), usdc, nin, 2_500i128 * 10_000_000),
    );
    let client = FxPoolContractClient::new(&env, &id);
    client.set_fund_core(&core);

    let union = Address::generate(&env);
    client.open_escrow(&1, &union, &1_000i128, &86_400);
    client.resolve_escrow_cash(&1, &400);
    let e = client.get_escrow(&1).unwrap();
    assert_eq!(e.remaining, 600);
}
