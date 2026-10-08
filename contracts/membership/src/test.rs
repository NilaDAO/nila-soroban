#![cfg(test)]

extern crate std;

use super::*;
use nila_roles::{RolesContract, RolesContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    vec, Address, BytesN, Env, IntoVal,
};

struct Fx {
    env: Env,
    roles: Address,
    union: Address,
    leader: Address,
    attester: Address,
    id: Address,
}

impl Fx {
    fn client(&self) -> MembershipContractClient<'_> {
        MembershipContractClient::new(&self.env, &self.id)
    }

    fn roles(&self) -> RolesContractClient<'_> {
        RolesContractClient::new(&self.env, &self.roles)
    }

    fn poly(&self) -> BytesN<20> {
        BytesN::from_array(&self.env, &[0x11; 20])
    }

    fn confirm(&self, member: &Address, plots: &Vec<u64>) {
        self.client().confirm_member(
            &self.union,
            member,
            &self.leader,
            &self.attester,
            &self.poly(),
            plots,
            &1,
        );
    }
}

fn setup() -> Fx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);
    let admin = Address::generate(&env);
    let attester = Address::generate(&env);
    let roles = env.register(RolesContract, (admin.clone(),));
    let roles_client = RolesContractClient::new(&env, &roles);
    let union = Address::generate(&env);
    let leader = Address::generate(&env);
    roles_client.set_leader(&union, &leader, &true);
    roles_client.set_role(&attester, &Role::Attester, &true);

    let id = env.register(MembershipContract, (admin.clone(), roles.clone()));
    Fx {
        env,
        roles,
        union,
        leader,
        attester,
        id,
    }
}

#[test]
fn confirm_and_gate() {
    let fx = setup();
    let c = fx.client();
    let member = Address::generate(&fx.env);
    let plots = vec![&fx.env, 42u64, 43u64];

    assert!(!c.is_member(&fx.union, &member));
    assert_eq!(c.union_of(&member), None);
    fx.confirm(&member, &plots);
    assert!(c.is_member(&fx.union, &member));
    assert_eq!(c.plots_of(&fx.union, &member), plots);
    assert_eq!(c.union_of(&member), Some(fx.union.clone()));
    let rec = c.get_member(&fx.union, &member).unwrap();
    assert_eq!(rec.cycle, 1);
    assert_eq!(rec.confirmed_ts, 1_700_000_000);
    assert_eq!(rec.attested_ts, 1_700_000_000);

    c.revoke_member(&fx.union, &member, &fx.leader);
    assert!(!c.is_member(&fx.union, &member));
    assert_eq!(c.plots_of(&fx.union, &member).len(), 0);
    assert_eq!(c.union_of(&member), None);
}

#[test]
fn reject_too_many_plots() {
    let fx = setup();
    let member = Address::generate(&fx.env);
    let mut plots = vec![&fx.env];
    for i in 0..33u64 {
        plots.push_back(i);
    }
    let err = fx
        .client()
        .try_confirm_member(
            &fx.union,
            &member,
            &fx.leader,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::TooManyPlots));
    let err = fx
        .client()
        .try_set_plots(&fx.union, &member, &fx.attester, &plots)
        .unwrap_err();
    assert_eq!(err, Ok(Error::TooManyPlots));
}

#[test]
fn confirm_needs_a_leader_and_an_attester_from_roles() {
    let fx = setup();
    let c = fx.client();
    let member = Address::generate(&fx.env);
    let stranger = Address::generate(&fx.env);
    let plots = vec![&fx.env, 1u64];

    let err = c
        .try_confirm_member(
            &fx.union,
            &member,
            &stranger,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));

    let err = c
        .try_confirm_member(
            &fx.union,
            &member,
            &fx.leader,
            &stranger,
            &fx.poly(),
            &plots,
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));

    // A leader of another union is not enough.
    let other_union = Address::generate(&fx.env);
    fx.roles().set_leader(&other_union, &stranger, &true);
    let err = c
        .try_confirm_member(
            &fx.union,
            &member,
            &stranger,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));

    // Revoking the attester role in roles takes effect at once.
    fx.roles().set_role(&fx.attester, &Role::Attester, &false);
    let err = c
        .try_confirm_member(
            &fx.union,
            &member,
            &fx.leader,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));
    assert!(!c.is_member(&fx.union, &member));
}

#[test]
fn confirm_requires_both_signatures() {
    let fx = setup();
    let member = Address::generate(&fx.env);
    let plots = vec![&fx.env, 1u64];
    let args = (
        &fx.union,
        &member,
        &fx.leader,
        &fx.attester,
        fx.poly(),
        plots.clone(),
        1u32,
    )
        .into_val(&fx.env);
    let invoke = MockAuthInvoke {
        contract: &fx.id,
        fn_name: "confirm_member",
        args,
        sub_invokes: &[],
    };
    // Leader only: the attester's auth is missing.
    let res = fx
        .client()
        .mock_auths(&[MockAuth {
            address: &fx.leader,
            invoke: &invoke,
        }])
        .try_confirm_member(
            &fx.union,
            &member,
            &fx.leader,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        );
    assert!(res.is_err());
    // Both: accepted.
    fx.client()
        .mock_auths(&[
            MockAuth {
                address: &fx.leader,
                invoke: &invoke,
            },
            MockAuth {
                address: &fx.attester,
                invoke: &invoke,
            },
        ])
        .confirm_member(
            &fx.union,
            &member,
            &fx.leader,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        );
    assert!(fx.client().is_member(&fx.union, &member));
}

#[test]
fn one_active_union_per_member() {
    let fx = setup();
    let c = fx.client();
    let member = Address::generate(&fx.env);
    let plots = vec![&fx.env, 7u64];
    fx.confirm(&member, &plots);

    let err = c
        .try_confirm_member(
            &fx.union,
            &member,
            &fx.leader,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::AlreadyMember));

    let union_b = Address::generate(&fx.env);
    let leader_b = Address::generate(&fx.env);
    fx.roles().set_leader(&union_b, &leader_b, &true);
    let err = c
        .try_confirm_member(
            &union_b,
            &member,
            &leader_b,
            &fx.attester,
            &fx.poly(),
            &plots,
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::AlreadyMember));

    // After a revoke she can join another union.
    c.revoke_member(&fx.union, &member, &fx.leader);
    c.confirm_member(
        &union_b,
        &member,
        &leader_b,
        &fx.attester,
        &fx.poly(),
        &plots,
        &2,
    );
    assert_eq!(c.union_of(&member), Some(union_b.clone()));
    assert!(c.is_member(&union_b, &member));
    assert!(!c.is_member(&fx.union, &member));
}

#[test]
fn set_plots_and_reattest_by_attester() {
    let fx = setup();
    let c = fx.client();
    let member = Address::generate(&fx.env);
    fx.confirm(&member, &vec![&fx.env, 1u64, 2u64]);

    fx.env.ledger().set_timestamp(1_700_100_000);
    let after_burn = vec![&fx.env, 2u64];
    c.set_plots(&fx.union, &member, &fx.attester, &after_burn);
    assert_eq!(c.plots_of(&fx.union, &member), after_burn);
    assert_eq!(
        c.get_member(&fx.union, &member).unwrap().attested_ts,
        1_700_100_000
    );

    // A leader is not an attester.
    let err = c
        .try_set_plots(&fx.union, &member, &fx.leader, &after_burn)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));

    c.reattest(&fx.union, &member, &fx.attester, &2);
    let rec = c.get_member(&fx.union, &member).unwrap();
    assert_eq!(rec.cycle, 2);
    assert_eq!(rec.confirmed_ts, 1_700_000_000);

    let err = c
        .try_reattest(&fx.union, &member, &fx.attester, &2)
        .unwrap_err();
    assert_eq!(err, Ok(Error::StaleCycle));
    let err = c
        .try_reattest(&fx.union, &member, &fx.leader, &3)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));
}

#[test]
fn no_updates_on_unknown_or_revoked_members() {
    let fx = setup();
    let c = fx.client();
    let member = Address::generate(&fx.env);
    let plots = vec![&fx.env, 1u64];

    let err = c
        .try_set_plots(&fx.union, &member, &fx.attester, &plots)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotMember));
    let err = c
        .try_revoke_member(&fx.union, &member, &fx.leader)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotMember));

    fx.confirm(&member, &plots);
    c.revoke_member(&fx.union, &member, &fx.leader);
    let err = c
        .try_reattest(&fx.union, &member, &fx.attester, &5)
        .unwrap_err();
    assert_eq!(err, Ok(Error::Inactive));
    let err = c
        .try_revoke_member(&fx.union, &member, &fx.leader)
        .unwrap_err();
    assert_eq!(err, Ok(Error::Inactive));
}

#[test]
fn revoke_is_leader_only() {
    let fx = setup();
    let c = fx.client();
    let member = Address::generate(&fx.env);
    fx.confirm(&member, &vec![&fx.env, 1u64]);

    let err = c
        .try_revoke_member(&fx.union, &member, &fx.attester)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));
    // The admin no longer overrides (R5).
    let admin = fx.roles().admin();
    let err = c.try_revoke_member(&fx.union, &member, &admin).unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));
    assert!(c.is_member(&fx.union, &member));
}

#[test]
fn set_record_is_attester_only_and_public() {
    let fx = setup();
    let c = fx.client();
    let record = BytesN::from_array(&fx.env, &[0xAA; 32]);
    let meta = BytesN::from_array(&fx.env, &[0xBB; 32]);

    assert_eq!(c.record_of(&9), None);
    let err = c
        .try_set_record(&9, &fx.leader, &record, &meta)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));

    c.set_record(&9, &fx.attester, &record, &meta);
    let got = c.record_of(&9).unwrap();
    assert_eq!(got.record_hash, record);
    assert_eq!(got.metadata_hash, meta);
    assert_eq!(got.updated_ts, 1_700_000_000);

    let meta2 = BytesN::from_array(&fx.env, &[0xCC; 32]);
    c.set_record(&9, &fx.attester, &record, &meta2);
    assert_eq!(c.record_of(&9).unwrap().metadata_hash, meta2);
}

#[test]
fn leader_and_attester_must_be_two_keys() {
    let fx = setup();
    let member = Address::generate(&fx.env);
    // The union appoints Nila's attester as a leader: still not two signatures.
    fx.roles().set_leader(&fx.union, &fx.attester, &true);
    let err = fx
        .client()
        .try_confirm_member(
            &fx.union,
            &member,
            &fx.attester,
            &fx.attester,
            &fx.poly(),
            &vec![&fx.env, 1u64],
            &1,
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));
}

#[test]
fn union_index_lives_as_long_as_the_entry() {
    use soroban_sdk::testutils::storage::Persistent as _;
    let fx = setup();
    let member = Address::generate(&fx.env);
    fx.confirm(&member, &vec![&fx.env, 1u64]);
    fx.env
        .ledger()
        .with_mut(|l| l.sequence_number += 100 * nila_common::DAY_IN_LEDGERS);
    fx.client()
        .set_plots(&fx.union, &member, &fx.attester, &vec![&fx.env, 2u64]);
    fx.env.as_contract(&fx.id, || {
        let s = fx.env.storage().persistent();
        let rec_ttl = s.get_ttl(&DataKey::Member(fx.union.clone(), member.clone()));
        let idx_ttl = s.get_ttl(&DataKey::MemberUnion(member.clone()));
        assert_eq!(rec_ttl, idx_ttl);
    });
}
