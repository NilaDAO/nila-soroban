#![cfg(test)]
//! machinery's own rules. Draw, claim, repay and default run end to end against fund_core in
//! `fund-core/src/test.rs`.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, IntoVal, Val,
};

const NIN: i128 = 10_000_000;

fn parts(driver: i128) -> RateParts {
    RateParts {
        driver,
        diesel: 500 * NIN,
        maintenance: 200 * NIN,
        insurance: 100 * NIN,
        depreciation: 250 * NIN,
        margin: 0,
    }
}

fn setup() -> (Env, MachineryContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);
    let a = || Address::generate(&env);
    let id = env.register(MachineryContract, (a(), a(), a()));
    let c = MachineryContractClient::new(&env, &id);
    (env, c)
}

#[test]
fn rate_table_is_the_unions_and_sets_the_prepay() {
    let (env, c) = setup();
    let union = Address::generate(&env);
    c.set_rate(&union, &1, &parts(400 * NIN));
    c.set_rate(&union, &2, &parts(600 * NIN));
    // Diesel plus half the driver part by default (X32): the larger of the two types.
    assert_eq!(c.max_prepay_per_hour(&union), 800 * NIN);
    c.set_prepay_share(&union, &10_000);
    assert_eq!(c.max_prepay_per_hour(&union), 1_100 * NIN);
    assert_eq!(
        c.try_set_prepay_share(&union, &10_001),
        Err(Ok(Error::BadArg))
    );
    let mut bad = parts(NIN);
    bad.diesel = -1;
    assert_eq!(c.try_set_rate(&union, &3, &bad), Err(Ok(Error::BadArg)));
    // Only the union account (its leaders, 3 of 5) sets its rates.
    env.set_auths(&[]);
    assert!(c.try_set_rate(&union, &1, &parts(NIN)).is_err());
}

#[test]
fn only_fund_core_grants_and_voids() {
    let (env, c) = setup();
    let union = Address::generate(&env);
    let id = BytesN::from_array(&env, &[1; 32]);
    assert_eq!(
        c.try_grant(&union, &Address::generate(&env), &id, &5),
        Err(Ok(Error::NotCore))
    );
    c.set_fund_core(&Address::generate(&env));
    assert_eq!(
        c.try_set_fund_core(&Address::generate(&env)),
        Err(Ok(Error::CoreAlreadySet))
    );
    env.set_auths(&[]);
    assert!(c
        .try_grant(&union, &Address::generate(&env), &id, &5)
        .is_err());
}

#[test]
fn upgrade_is_timelocked() {
    let (env, c) = setup();
    assert_eq!(c.upgrade_delay(), 48 * 3600);
    c.upgrade(&BytesN::from_array(&env, &[9; 32]));
    assert_eq!(c.try_execute_upgrade(), Err(Ok(Error::UpgradeNotReady)));
    c.cancel_upgrade();
    assert_eq!(c.pending_upgrade(), None);
}

/// sha256 over the XDR of one sample of every stored key and value. If this changes, data a
/// deployed machinery holds no longer decodes after an upgrade: write a migration, or leave the
/// stored shape alone (docs/STORAGE_LAYOUT.md).
#[test]
fn storage_layout_is_frozen() {
    use soroban_sdk::xdr::ToXdr;
    let env = Env::default();
    let a = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let h = BytesN::from_array(&env, &[7; 32]);
    let mut v: soroban_sdk::Vec<Val> = soroban_sdk::Vec::new(&env);
    for k in [
        DataKey::Admin,
        DataKey::Roles,
        DataKey::LandTitle,
        DataKey::FundCore,
        DataKey::Paused,
        DataKey::Rate(a.clone(), 1),
        DataKey::Types(a.clone()),
        DataKey::PrepayDriverBp(a.clone()),
        DataKey::NextMachine,
        DataKey::Machine(1),
        DataKey::Voucher(a.clone(), h.clone()),
        DataKey::WeekUse(1, 2),
    ] {
        v.push_back(k.into_val(&env));
    }
    v.push_back(parts(1).into_val(&env));
    v.push_back(
        Machine {
            union: a.clone(),
            owner: a.clone(),
            machine_type: 1,
            hours_per_week: 2,
            photos_hash: h.clone(),
            confirmed: true,
            on_hold: false,
            hold_reason: 0,
        }
        .into_val(&env),
    );
    v.push_back(
        Voucher {
            member: a.clone(),
            hours: 1,
            used: 2,
            void: false,
            code_hash: Some(h.clone()),
            code_max: 3,
            code_expires: 4,
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
    let got: BytesN<32> = env.crypto().sha256(&v.to_xdr(&env)).into();
    let hex: std::string::String = got
        .to_array()
        .iter()
        .map(|b| std::format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex, "5b52a85e199e7518d1ae4aac82299ae67706a6d11b5f76beba6fadf131d2b57e",
        "machinery storage layout changed"
    );
}

#[test]
fn weekly_hours_live_in_temporary_storage_and_lapse_after_the_week() {
    use soroban_sdk::testutils::storage::Temporary as _;
    let (env, c) = setup();
    let id = c.address.clone();
    let key = DataKey::WeekUse(7, env.ledger().timestamp() / WEEK);
    env.as_contract(&id, || {
        env.storage().temporary().set(&key, &5u32);
        env.storage()
            .temporary()
            .extend_ttl(&key, WEEK_USE_LEDGERS, WEEK_USE_LEDGERS);
        assert!(!env.storage().persistent().has(&key));
        assert!(env.storage().temporary().get_ttl(&key) >= WEEK_USE_LEDGERS - 1);
    });
    assert_eq!(c.hours_this_week(&7), 5);
}
