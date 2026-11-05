#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal, Symbol,
};

fn h(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

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
    client.admit_union(&union, &h(&env, 1));

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
    client
        .mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &id,
                fn_name: "admit_union",
                args: (&union, h(&env, 1)).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .admit_union(&union, &h(&env, 1));

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
    client.admit_union(&union_a, &h(&env, 1));

    client.set_union_role(&union_a, &who, &UnionRole::Treasurer, &true);
    assert_eq!(env.auths()[0].0, union_a);
    assert_eq!(
        client.try_set_union_role(&union_b, &who, &UnionRole::Treasurer, &true),
        Err(Ok(Error::NotAdmitted))
    );
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
    client.admit_union(&union_a, &h(&env, 1));
    client.admit_union(&union_b, &h(&env, 2));
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
    client.admit_union(&union, &h(&env, 1));
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
    // 3 Nov: union settings and the trust's admission (decision 008), new keys at the end.
    v.push_back(DataKey::UnionSettings(a.clone()).into_val(&env));
    v.push_back(DataKey::Admitted(a.clone()).into_val(&env));
    v.push_back(
        UnionSettings {
            wait: 1,
            session_len: 2,
            session_contracts: soroban_sdk::vec![&env, a.clone()],
            sms_key: Some(a.clone()),
            fx_pool: Some(a.clone()),
            usdc: Some(a.clone()),
        }
        .into_val(&env),
    );
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
        hex, "b10c8ed1cbe20551d3522746593c24c4c15e21266eb34313ce2fa8bcf9f3825a",
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

/// A union's account settings (3 Nov): set by the union account only, refused out of bounds
/// (not clamped), None until set.
#[test]
fn union_settings_bounds_and_auth() {
    let (env, _admin, client) = setup();
    let union = Address::generate(&env);
    client.admit_union(&union, &h(&env, 1));
    assert_eq!(client.union_settings(&union), None);
    let fund_core = Address::generate(&env);
    let ok = UnionSettings {
        wait: WAIT_MIN,
        session_len: SESSION_MAX,
        session_contracts: soroban_sdk::vec![&env, fund_core.clone()],
        sms_key: Some(Address::generate(&env)),
        fx_pool: None,
        usdc: None,
    };
    // A session contract without the core role is refused (review 5 Nov); fund_core gets it.
    assert_eq!(
        client.try_set_union_settings(&union, &ok),
        Err(Ok(Error::BadSetting))
    );
    client.set_role(&fund_core, &Role::Core, &true);
    // fx_pool without USDC, or USDC without fx_pool: refused.
    for (p, u) in [
        (Some(Address::generate(&env)), None),
        (None, Some(Address::generate(&env))),
    ] {
        let mut bad = ok.clone();
        bad.fx_pool = p;
        bad.usdc = u;
        assert_eq!(
            client.try_set_union_settings(&union, &bad),
            Err(Ok(Error::BadSetting))
        );
    }
    for (w, s) in [
        (WAIT_MIN - 1, 300),
        (WAIT_MAX + 1, 300),
        (WAIT_MIN, SESSION_MIN - 1),
        (WAIT_MIN, SESSION_MAX + 1),
    ] {
        let mut bad = ok.clone();
        bad.wait = w;
        bad.session_len = s;
        assert_eq!(
            client.try_set_union_settings(&union, &bad),
            Err(Ok(Error::BadSetting))
        );
    }
    client.set_union_settings(&union, &ok);
    assert_eq!(
        env.auths(),
        std::vec![(
            union.clone(),
            AuthorizedInvocation {
                function: AuthorizedFunction::Contract((
                    client.address.clone(),
                    Symbol::new(&env, "set_union_settings"),
                    (union.clone(), ok.clone()).into_val(&env),
                )),
                sub_invocations: std::vec![],
            }
        )]
    );
    assert_eq!(client.union_settings(&union), Some(ok.clone()));
    // Without the union's signature: refused.
    env.set_auths(&[]);
    assert!(client.try_set_union_settings(&union, &ok).is_err());
    // Another union's settings stay unset.
    assert_eq!(client.union_settings(&Address::generate(&env)), None);
}

fn settings(env: &Env) -> UnionSettings {
    UnionSettings {
        wait: WAIT_MIN,
        session_len: SESSION_MIN,
        session_contracts: soroban_sdk::vec![env],
        sms_key: None,
        fx_pool: None,
        usdc: None,
    }
}

/// Decision 008: only the roles admin (the trust) admits or revokes a union; the union itself
/// cannot admit itself.
#[test]
fn admit_and_revoke_are_admin_only() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let id = env.register(RolesContract, (admin.clone(),));
    let client = RolesContractClient::new(&env, &id);
    let union = Address::generate(&env);
    let mock = |who: &Address, f: &'static str, args: soroban_sdk::Vec<soroban_sdk::Val>| {
        std::vec![MockAuthInvoke {
            contract: &id,
            fn_name: f,
            args,
            sub_invokes: &[],
        }]
        .into_iter()
        .map(move |invoke| (who.clone(), invoke))
        .collect::<std::vec::Vec<_>>()
    };
    // No signature, or the union's own: refused.
    assert!(client.try_admit_union(&union, &h(&env, 1)).is_err());
    let m = mock(&union, "admit_union", (&union, h(&env, 1)).into_val(&env));
    assert!(client
        .mock_auths(&[MockAuth {
            address: &m[0].0,
            invoke: &m[0].1,
        }])
        .try_admit_union(&union, &h(&env, 1))
        .is_err());
    assert!(!client.is_union(&union));
    // The admin: accepted, and the admin is the only authoriser.
    let m = mock(&admin, "admit_union", (&union, h(&env, 1)).into_val(&env));
    client
        .mock_auths(&[MockAuth {
            address: &m[0].0,
            invoke: &m[0].1,
        }])
        .admit_union(&union, &h(&env, 1));
    assert_eq!(env.auths()[0].0, admin);
    assert!(client.is_union(&union));
    // Revoke: the union cannot, the admin can.
    let m = mock(&union, "revoke_union", (&union,).into_val(&env));
    assert!(client
        .mock_auths(&[MockAuth {
            address: &m[0].0,
            invoke: &m[0].1,
        }])
        .try_revoke_union(&union)
        .is_err());
    assert!(client.is_union(&union));
    let m = mock(&admin, "revoke_union", (&union,).into_val(&env));
    client
        .mock_auths(&[MockAuth {
            address: &m[0].0,
            invoke: &m[0].1,
        }])
        .revoke_union(&union);
    assert!(!client.is_union(&union));
}

/// Before admission a union can name no leaders, officers or settings.
#[test]
fn a_union_not_admitted_is_refused() {
    let (env, _admin, client) = setup();
    let union = Address::generate(&env);
    let who = Address::generate(&env);
    assert!(!client.is_union(&union));
    assert_eq!(client.union_admission(&union), None);
    assert_eq!(
        client.try_set_leader(&union, &who, &true),
        Err(Ok(Error::NotAdmitted))
    );
    assert_eq!(
        client.try_set_union_role(&union, &who, &UnionRole::Secretary, &true),
        Err(Ok(Error::NotAdmitted))
    );
    assert_eq!(
        client.try_set_union_settings(&union, &settings(&env)),
        Err(Ok(Error::NotAdmitted))
    );
    // Revoking a union never admitted is refused too.
    assert_eq!(client.try_revoke_union(&union), Err(Ok(Error::NotAdmitted)));
}

/// Revoked: the entries stay, but leaders, officers and settings read as absent; admitting again
/// restores them as they were. A revoked union cannot change them either.
#[test]
fn revoke_hides_leaders_and_settings_and_readmission_restores_them() {
    let (env, _admin, client) = setup();
    let union = Address::generate(&env);
    let leader = Address::generate(&env);
    client.admit_union(&union, &h(&env, 1));
    client.set_leader(&union, &leader, &true);
    client.set_union_role(&union, &leader, &UnionRole::Treasurer, &true);
    client.set_union_settings(&union, &settings(&env));
    assert!(client.is_leader(&union, &leader));

    client.revoke_union(&union);
    assert!(!client.is_union(&union));
    assert!(!client.is_leader(&union, &leader));
    assert!(!client.has_union_role(&union, &leader, &UnionRole::Treasurer));
    assert_eq!(client.union_settings(&union), None);
    assert_eq!(
        client.try_set_leader(&union, &leader, &false),
        Err(Ok(Error::NotAdmitted))
    );
    env.as_contract(&client.address, || {
        assert!(env
            .storage()
            .persistent()
            .has(&DataKey::Leader(union.clone(), leader.clone())));
    });

    client.admit_union(&union, &h(&env, 2));
    assert!(client.is_leader(&union, &leader));
    assert!(client.has_union_role(&union, &leader, &UnionRole::Treasurer));
    assert_eq!(client.union_settings(&union), Some(settings(&env)));
}

/// Admitting again replaces the details hash; both calls publish their events.
#[test]
fn readmission_updates_the_hash_and_events_are_published() {
    use soroban_sdk::{testutils::Events as _, Event};
    let (env, _admin, client) = setup();
    let union = Address::generate(&env);
    client.admit_union(&union, &h(&env, 1));
    assert_eq!(
        env.events().all(),
        std::vec![UnionAdmitted {
            union: union.clone(),
            details_hash: h(&env, 1),
        }
        .to_xdr(&env, &client.address)]
    );
    assert_eq!(client.union_admission(&union), Some(h(&env, 1)));
    client.admit_union(&union, &h(&env, 2));
    assert_eq!(client.union_admission(&union), Some(h(&env, 2)));
    client.revoke_union(&union);
    assert_eq!(
        env.events().all(),
        std::vec![UnionRevoked {
            union: union.clone()
        }
        .to_xdr(&env, &client.address)]
    );
    assert_eq!(client.union_admission(&union), None);
}
