#![cfg(test)]

extern crate std;

use super::*;
use nila_roles::{RolesContract, RolesContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env,
};

struct Fx {
    env: Env,
    union: Address,
    attester: Address,
    id: Address,
}

impl Fx {
    fn client(&self) -> LandTitleContractClient<'_> {
        LandTitleContractClient::new(&self.env, &self.id)
    }

    fn hash(&self, b: u8) -> BytesN<32> {
        BytesN::from_array(&self.env, &[b; 32])
    }

    fn set(&self, title_id: u64, holder: &Address, union: &Address) {
        self.client().set_title(
            &title_id,
            &self.attester,
            holder,
            union,
            &BytesN::from_array(&self.env, &[0x11; 20]),
            &self.hash(1),
            &self.hash(2),
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
    RolesContractClient::new(&env, &roles).set_role(&attester, &Role::Attester, &true);
    let union = Address::generate(&env);
    let id = env.register(LandTitleContract, (admin, roles));
    Fx {
        env,
        union,
        attester,
        id,
    }
}

#[test]
fn holding_a_title_of_the_union_is_membership() {
    let fx = setup();
    let c = fx.client();
    let farmer = Address::generate(&fx.env);
    let other_union = Address::generate(&fx.env);

    assert!(!c.holds_title(&fx.union, &farmer));
    fx.set(7, &farmer, &fx.union);
    assert!(c.holds_title(&fx.union, &farmer));
    assert!(!c.holds_title(&other_union, &farmer));

    let t = c.title(&7).unwrap();
    assert_eq!(t.holder, farmer);
    assert_eq!(t.union, fx.union);
    assert_eq!(t.record_hash, fx.hash(1));
    assert_eq!(t.updated_ts, 1_700_000_000);
}

#[test]
fn last_title_burned_ends_membership() {
    let fx = setup();
    let c = fx.client();
    let farmer = Address::generate(&fx.env);
    fx.set(1, &farmer, &fx.union);
    fx.set(2, &farmer, &fx.union);
    assert_eq!(c.title_count(&fx.union, &farmer), 2);

    c.burn_title(&1, &fx.attester);
    assert!(c.holds_title(&fx.union, &farmer));
    c.burn_title(&2, &fx.attester);
    assert!(!c.holds_title(&fx.union, &farmer));
    assert_eq!(c.title(&2), None);
    assert_eq!(
        c.try_burn_title(&2, &fx.attester).unwrap_err(),
        Ok(Error::NotFound)
    );
}

#[test]
fn a_moved_title_moves_membership() {
    let fx = setup();
    let c = fx.client();
    let mother = Address::generate(&fx.env);
    let daughter = Address::generate(&fx.env);
    fx.set(5, &mother, &fx.union);
    // Inheritance on Polygon (union + attester, C7), copied by the attester.
    fx.set(5, &daughter, &fx.union);
    assert!(!c.holds_title(&fx.union, &mother));
    assert!(c.holds_title(&fx.union, &daughter));
    assert_eq!(c.title_count(&fx.union, &daughter), 1);
}

#[test]
fn writes_are_attester_only() {
    let fx = setup();
    let c = fx.client();
    let stranger = Address::generate(&fx.env);
    let farmer = Address::generate(&fx.env);
    let err = c
        .try_set_title(
            &1,
            &stranger,
            &farmer,
            &fx.union,
            &BytesN::from_array(&fx.env, &[0; 20]),
            &fx.hash(1),
            &fx.hash(2),
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotAuthorized));
    fx.set(1, &farmer, &fx.union);
    assert_eq!(
        c.try_set_record(&1, &stranger, &fx.hash(3), &fx.hash(4))
            .unwrap_err(),
        Ok(Error::NotAuthorized)
    );
    assert_eq!(
        c.try_burn_title(&1, &stranger).unwrap_err(),
        Ok(Error::NotAuthorized)
    );
}

#[test]
fn set_record_updates_hashes_of_a_known_title() {
    let fx = setup();
    let c = fx.client();
    let farmer = Address::generate(&fx.env);
    assert_eq!(
        c.try_set_record(&9, &fx.attester, &fx.hash(3), &fx.hash(4))
            .unwrap_err(),
        Ok(Error::NotFound)
    );
    fx.set(9, &farmer, &fx.union);
    c.set_record(&9, &fx.attester, &fx.hash(3), &fx.hash(4));
    let t = c.title(&9).unwrap();
    assert_eq!(t.record_hash, fx.hash(3));
    assert_eq!(t.metadata_hash, fx.hash(4));
    assert_eq!(t.holder, farmer);
}

#[test]
fn pause_stops_writes_not_reads() {
    let fx = setup();
    let c = fx.client();
    let farmer = Address::generate(&fx.env);
    fx.set(1, &farmer, &fx.union);
    c.pause();
    assert!(c.is_paused());
    assert!(c.holds_title(&fx.union, &farmer));
    assert_eq!(
        c.try_burn_title(&1, &fx.attester).unwrap_err(),
        Ok(Error::Paused)
    );
    c.unpause();
    c.burn_title(&1, &fx.attester);
}

#[test]
fn upgrade_waits_for_the_delay() {
    let fx = setup();
    let c = fx.client();
    c.set_upgrade_delay(&3600);
    c.upgrade(&fx.hash(9));
    assert_eq!(
        c.try_execute_upgrade().unwrap_err(),
        Ok(Error::UpgradeNotReady)
    );
    c.cancel_upgrade();
    assert_eq!(
        c.try_execute_upgrade().unwrap_err(),
        Ok(Error::UpgradeNotReady)
    );
}

#[test]
fn upgrade_delay_is_lowered_only_through_the_delay() {
    let fx = setup();
    let c = fx.client();
    c.set_upgrade_delay(&(48 * 3600));
    c.set_upgrade_delay(&0);
    assert_eq!(c.upgrade_delay(), 48 * 3600);
    fx.env.ledger().set_timestamp(1_700_000_000 + 48 * 3600);
    c.set_upgrade_delay(&0);
    assert_eq!(c.upgrade_delay(), 0);
}

#[test]
fn pause_and_upgrade_need_the_admin() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let roles = env.register(RolesContract, (admin.clone(),));
    let id = env.register(LandTitleContract, (admin, roles));
    let c = LandTitleContractClient::new(&env, &id);
    // No auth mocked: the admin's signature is missing.
    assert!(c.try_pause().is_err());
    assert!(c.try_upgrade(&BytesN::from_array(&env, &[1; 32])).is_err());
}

#[test]
fn membership_read_keeps_the_entry_alive() {
    use soroban_sdk::testutils::storage::Persistent as _;
    let fx = setup();
    let farmer = Address::generate(&fx.env);
    fx.set(1, &farmer, &fx.union);
    let key = DataKey::Count(fx.union.clone(), farmer.clone());
    let ttl_after_write = fx
        .env
        .as_contract(&fx.id, || fx.env.storage().persistent().get_ttl(&key));
    fx.env.ledger().with_mut(|l| l.sequence_number += 100 * DAY);
    fx.client().holds_title(&fx.union, &farmer);
    let ttl_after_read = fx
        .env
        .as_contract(&fx.id, || fx.env.storage().persistent().get_ttl(&key));
    assert!(ttl_after_read > ttl_after_write - 100 * DAY);
}

const DAY: u32 = nila_common::DAY_IN_LEDGERS;
