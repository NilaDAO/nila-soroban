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

#[test]
fn upgrade_is_timelocked_and_the_delay_cannot_be_cut_at_once() {
    use soroban_sdk::{testutils::Ledger, BytesN};
    let (env, _admin, r) = setup();
    env.ledger().set_timestamp(1_000);
    assert_eq!(r.upgrade_delay(), 48 * 3600);
    r.upgrade(&BytesN::from_array(&env, &[1; 32]));
    assert_eq!(r.try_execute_upgrade(), Err(Ok(Error::UpgradeNotReady)));
    r.cancel_upgrade();
    // A stolen admin key cannot drop the delay to zero and upgrade at once.
    r.set_upgrade_delay(&0);
    assert_eq!(r.upgrade_delay(), 48 * 3600);
    env.ledger().set_timestamp(1_000 + 48 * 3600);
    r.set_upgrade_delay(&0);
    assert_eq!(r.upgrade_delay(), 0);
}

/// sha256 over the XDR of one sample of every stored key and value. If this changes, data a
/// deployed roles holds no longer decodes after an upgrade: write a migration, or leave the
/// stored shape alone (docs/STORAGE_LAYOUT.md).
#[test]
fn storage_layout_is_frozen() {
    use soroban_sdk::{xdr::ToXdr, BytesN, IntoVal, Val};
    let env = Env::default();
    let a = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    #[allow(unused_variables)]
    let h = BytesN::from_array(&env, &[7; 32]);
    let mut v: soroban_sdk::Vec<Val> = soroban_sdk::Vec::new(&env);
    v.push_back(DataKey::Admin.into_val(&env));
    for r in [Role::Oracle, Role::Attester, Role::Core] {
        v.push_back(DataKey::Role(a.clone(), r).into_val(&env));
    }
    v.push_back(DataKey::Leader(a.clone(), a.clone()).into_val(&env));
    for r in [UnionRole::Treasurer, UnionRole::Secretary] {
        v.push_back(DataKey::UnionRole(a.clone(), a.clone(), r).into_val(&env));
    }
    for k in [
        nila_common::upgrade::UpgradeKey::UpgDelay,
        nila_common::upgrade::UpgradeKey::UpgPending,
        nila_common::upgrade::UpgradeKey::UpgPendingDelay,
    ] {
        v.push_back(k.into_val(&env));
    }
    v.push_back(
        nila_common::upgrade::PendingUpgrade {
            wasm_hash: h.clone(),
            eta: 1,
        }
        .into_val(&env),
    );
    v.push_back(nila_common::upgrade::PendingDelay { delay: 1, eta: 2 }.into_val(&env));
    let got: BytesN<32> = env.crypto().sha256(&v.to_xdr(&env)).into();
    let hex: std::string::String = got
        .to_array()
        .iter()
        .map(|b| std::format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex, "268e98646454ff294a044c0846fbf36be2d733e9ea860acd55d7c98e5a51dcc9",
        "roles storage layout changed"
    );
}

#[test]
fn a_filed_lowering_never_stays_armed_and_the_delay_is_bounded() {
    use soroban_sdk::{testutils::Ledger, BytesN};
    let (env, _admin, r) = setup();
    let t0 = 1_000;
    env.ledger().set_timestamp(t0);
    // File a lowering to 0, then raise to 30 days: the raise voids the filing.
    r.set_upgrade_delay(&0);
    assert!(r.pending_delay().is_some());
    r.set_upgrade_delay(&(30 * 24 * 3600));
    assert_eq!(r.pending_delay(), None);
    env.ledger().set_timestamp(t0 + 48 * 3600);
    r.set_upgrade_delay(&0);
    assert_eq!(r.upgrade_delay(), 30 * 24 * 3600);
    // A matured filing not used within 7 days is void: setting it again re-files.
    env.ledger()
        .set_timestamp(t0 + 48 * 3600 + 30 * 24 * 3600 + 8 * 24 * 3600);
    r.set_upgrade_delay(&0);
    assert_eq!(r.upgrade_delay(), 30 * 24 * 3600);
    // cancel_upgrade voids a filed lowering too.
    r.cancel_upgrade();
    assert_eq!(r.pending_delay(), None);
    // Never above 30 days, so the contract can always be upgraded again.
    assert_eq!(
        r.try_set_upgrade_delay(&(30 * 24 * 3600 + 1)),
        Err(Ok(Error::BadDelay))
    );
    // A new filing replaces the old one; nothing applies at once.
    let now = env.ledger().timestamp();
    r.upgrade(&BytesN::from_array(&env, &[1; 32]));
    r.upgrade(&BytesN::from_array(&env, &[2; 32]));
    let p = r.pending_upgrade().unwrap();
    assert_eq!(p.wasm_hash, BytesN::from_array(&env, &[2; 32]));
    assert_eq!(p.eta, now + 30 * 24 * 3600);
}
