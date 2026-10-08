#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal, Symbol,
};

fn setup() -> (Env, Address, RolesContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(RolesContract, (admin.clone(),));
    let client = RolesContractClient::new(&env, &id);
    (env, admin, client)
}

#[test]
fn role_and_leader_round_trip() {
    let (env, admin, client) = setup();
    assert_eq!(client.admin(), admin);

    let oracle = Address::generate(&env);
    let attester = Address::generate(&env);
    let union = Address::generate(&env);
    let leader = Address::generate(&env);

    assert!(!client.is_oracle(&oracle));
    client.set_role(&oracle, &Role::Oracle, &true);
    assert!(client.is_oracle(&oracle));
    assert!(client.has_role(&oracle, &Role::Oracle));
    assert!(!client.has_role(&oracle, &Role::Attester));
    client.set_role(&oracle, &Role::Oracle, &false);
    assert!(!client.is_oracle(&oracle));

    client.set_role(&attester, &Role::Attester, &true);
    assert!(client.has_role(&attester, &Role::Attester));

    assert!(!client.is_leader(&union, &leader));
    client.set_leader(&union, &leader, &true);
    assert!(client.is_leader(&union, &leader));
    client.set_leader(&union, &leader, &false);
    assert!(!client.is_leader(&union, &leader));
}

#[test]
fn set_role_requires_admin() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let id = env.register(RolesContract, (admin.clone(),));
    let client = RolesContractClient::new(&env, &id);
    let oracle = Address::generate(&env);

    assert!(client.try_set_role(&oracle, &Role::Oracle, &true).is_err());
}

/// D12: the union account authorises its own leaders; the admin key cannot.
#[test]
fn set_leader_is_signed_by_the_union_not_the_admin() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let id = env.register(RolesContract, (admin.clone(),));
    let client = RolesContractClient::new(&env, &id);
    let union = Address::generate(&env);
    let leader = Address::generate(&env);

    // Only the admin signs: rejected.
    let res = client
        .mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &id,
                fn_name: "set_leader",
                args: (&union, &leader, true).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_set_leader(&union, &leader, &true);
    assert!(res.is_err());
    assert!(!client.is_leader(&union, &leader));

    // The union signs: accepted, and the union is the only authoriser.
    client
        .mock_auths(&[MockAuth {
            address: &union,
            invoke: &MockAuthInvoke {
                contract: &id,
                fn_name: "set_leader",
                args: (&union, &leader, true).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .set_leader(&union, &leader, &true);
    assert_eq!(
        env.auths(),
        std::vec![(
            union.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    id.clone(),
                    Symbol::new(&env, "set_leader"),
                    (&union, &leader, true).into_val(&env),
                )),
                sub_invocations: std::vec![],
            }
        )]
    );
    assert!(client.is_leader(&union, &leader));
}

#[test]
fn union_roles_are_per_union_and_union_signed() {
    let (env, _admin, client) = setup();
    let union_a = Address::generate(&env);
    let union_b = Address::generate(&env);
    let who = Address::generate(&env);

    client.set_union_role(&union_a, &who, &UnionRole::Treasurer, &true);
    assert_eq!(env.auths()[0].0, union_a);
    assert!(client.has_union_role(&union_a, &who, &UnionRole::Treasurer));
    assert!(!client.has_union_role(&union_a, &who, &UnionRole::Secretary));
    assert!(!client.has_union_role(&union_b, &who, &UnionRole::Treasurer));
    // A union officer is not a leader and holds no trust-wide role.
    assert!(!client.is_leader(&union_a, &who));
    assert!(!client.has_role(&who, &Role::Core));

    client.set_union_role(&union_a, &who, &UnionRole::Treasurer, &false);
    assert!(!client.has_union_role(&union_a, &who, &UnionRole::Treasurer));
}

#[test]
fn leaders_of_one_union_are_not_leaders_of_another() {
    let (env, _admin, client) = setup();
    let union_a = Address::generate(&env);
    let union_b = Address::generate(&env);
    let leader = Address::generate(&env);
    client.set_leader(&union_a, &leader, &true);
    assert!(client.is_leader(&union_a, &leader));
    assert!(!client.is_leader(&union_b, &leader));
}

#[test]
fn reads_keep_a_leader_alive() {
    use soroban_sdk::testutils::{storage::Persistent as _, Ledger};
    let (env, _admin, client) = setup();
    let union = Address::generate(&env);
    let leader = Address::generate(&env);
    client.set_leader(&union, &leader, &true);
    // 100 days of reads only, then past the original 120-day TTL.
    env.ledger()
        .with_mut(|l| l.sequence_number += 100 * nila_common::DAY_IN_LEDGERS);
    assert!(client.is_leader(&union, &leader));
    env.ledger()
        .with_mut(|l| l.sequence_number += 50 * nila_common::DAY_IN_LEDGERS);
    assert!(client.is_leader(&union, &leader));
    env.as_contract(&client.address, || {
        let ttl = env
            .storage()
            .persistent()
            .get_ttl(&DataKey::Leader(union.clone(), leader.clone()));
        assert!(ttl > 30 * nila_common::DAY_IN_LEDGERS);
    });
}
