#![cfg(test)]
//! Unit tests for fx_pool's own rules. The cash scan, cash-out and trustline flip run
//! end to end against fund_core in `fund-core/src/test.rs`.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env,
};

const NIN: i128 = 10_000_000;

fn setup() -> (Env, FxPoolContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);
    let admin = Address::generate(&env);
    let a = || Address::generate(&env);
    let id = env.register(
        FxPoolContract,
        (admin.clone(), a(), a(), a(), a(), 2_500 * NIN),
    );
    let client = FxPoolContractClient::new(&env, &id);
    (env, client, admin)
}

#[test]
fn defaults_and_fund_core_set_once() {
    let (env, f, _) = setup();
    assert_eq!(f.max_scan(), 10_000 * NIN);
    assert_eq!(f.upgrade_delay(), 48 * 3_600);
    assert_eq!(f.daily_cap(), 2_500 * NIN);
    let core = Address::generate(&env);
    f.set_fund_core(&core);
    assert_eq!(f.fund_core(), Some(core));
    assert_eq!(
        f.try_set_fund_core(&Address::generate(&env)),
        Err(Ok(Error::CoreAlreadySet))
    );
}

#[test]
fn admin_only_setters() {
    let (env, f, _) = setup();
    env.set_auths(&[]);
    assert!(f.try_set_max_scan(&NIN).is_err());
    assert!(f.try_pause().is_err());
    assert!(f.try_set_fund_core(&Address::generate(&env)).is_err());
    env.mock_all_auths();
    assert_eq!(f.try_set_max_scan(&0), Err(Ok(Error::AmountZero)));
}

#[test]
fn only_fund_core_settles_or_authorises() {
    let (env, f, _) = setup();
    let union = Address::generate(&env);
    // No fund_core yet.
    assert_eq!(f.try_settle_cash(&union, &NIN), Err(Ok(Error::NotCore)));
    f.set_fund_core(&Address::generate(&env));
    env.set_auths(&[]);
    assert!(f.try_settle_cash(&union, &NIN).is_err());
    assert!(f
        .try_set_member_auth(&union, &Address::generate(&env), &false)
        .is_err());
    env.mock_all_auths();
    // Nothing at hand: nothing settled.
    assert_eq!(f.settle_cash(&union, &NIN), 0);
}

#[test]
fn pause_stops_the_desk() {
    let (env, f, _) = setup();
    f.pause();
    assert!(f.is_paused());
    let who = Address::generate(&env);
    let h = BytesN::from_array(&env, &[1; 32]);
    assert_eq!(
        f.try_cash_scan_mint(&who, &who, &Purpose::Invest, &who, &NIN, &h, &None),
        Err(Ok(Error::Paused))
    );
    assert_eq!(
        f.try_request_cash_out(&who, &who, &NIN, &h, &1_700_000_100),
        Err(Ok(Error::Paused))
    );
    assert_eq!(
        f.try_pay_out(&who, &Bytes::from_array(&env, &[1])),
        Err(Ok(Error::Paused))
    );
    f.unpause();
    assert!(!f.is_paused());
}

#[test]
fn upgrade_waits_out_the_timelock() {
    let (env, f, _) = setup();
    let hash = BytesN::from_array(&env, &[7; 32]);
    f.upgrade(&hash);
    assert_eq!(f.try_execute_upgrade(), Err(Ok(Error::UpgradeNotReady)));
    f.cancel_upgrade();
    assert_eq!(f.try_execute_upgrade(), Err(Ok(Error::UpgradeNotReady)));

    // Lowering the delay waits out the current one.
    f.set_upgrade_delay(&3_600);
    assert_eq!(f.upgrade_delay(), 48 * 3_600);
    env.ledger().with_mut(|l| l.timestamp += 48 * 3_600);
    f.set_upgrade_delay(&3_600);
    assert_eq!(f.upgrade_delay(), 3_600);
    // Raising applies at once.
    f.set_upgrade_delay(&(72 * 3_600));
    assert_eq!(f.upgrade_delay(), 72 * 3_600);
}

#[test]
fn unknown_escrow_and_code() {
    let (env, f, _) = setup();
    assert_eq!(f.try_burn_expired_escrow(&1), Err(Ok(Error::NotFound)));
    let h = BytesN::from_array(&env, &[3; 32]);
    assert_eq!(f.try_reclaim_cash_out(&h), Err(Ok(Error::NotFound)));
    assert_eq!(f.cash_out(&h), None);
    assert_eq!(f.cash_at_hand(&Address::generate(&env)), 0);
}

/// sha256 over the XDR of one sample of every stored key and value. If this changes, data a
/// deployed fx_pool holds no longer decodes after an upgrade: write a migration, or leave the
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
    v.push_back(DataKey::Roles.into_val(&env));
    v.push_back(DataKey::LandTitle.into_val(&env));
    v.push_back(DataKey::Usdc.into_val(&env));
    v.push_back(DataKey::Nin.into_val(&env));
    v.push_back(DataKey::FundCore.into_val(&env));
    v.push_back(DataKey::DailyCap.into_val(&env));
    v.push_back(DataKey::MaxScan.into_val(&env));
    v.push_back(DataKey::Paused.into_val(&env));
    v.push_back(DataKey::NextEscrow.into_val(&env));
    v.push_back(DataKey::Escrow(1).into_val(&env));
    v.push_back(DataKey::EscrowsOf(a.clone()).into_val(&env));
    v.push_back(DataKey::CashAtHand(a.clone()).into_val(&env));
    v.push_back(DataKey::Scan(h.clone()).into_val(&env));
    v.push_back(DataKey::CashOut(h.clone()).into_val(&env));
    v.push_back(DataKey::CodeUsed(h.clone()).into_val(&env));
    v.push_back(DataKey::SeniorInvestor(a.clone()).into_val(&env));
    v.push_back(DataKey::Rate.into_val(&env));
    v.push_back(DataKey::VoucherFloor(a.clone()).into_val(&env));
    v.push_back(DataKey::Anchor.into_val(&env));
    v.push_back(DataKey::LpOf(a.clone(), a.clone()).into_val(&env));
    v.push_back(DataKey::LpUsdc(a.clone()).into_val(&env));
    v.push_back(DataKey::LpFeeBp.into_val(&env));
    v.push_back(DataKey::NextOffer.into_val(&env));
    v.push_back(DataKey::Offer(1).into_val(&env));
    v.push_back(DataKey::OffersOf(a.clone()).into_val(&env));
    v.push_back(DataKey::DayOut(1).into_val(&env));
    v.push_back(DataKey::RateBandBp.into_val(&env));
    v.push_back(
        LpMember {
            accepted: true,
            kyc_ref: h.clone(),
        }
        .into_val(&env),
    );
    for k in [OfferKind::Bring, OfferKind::Collect] {
        v.push_back(k.into_val(&env));
    }
    for st in [
        OfferState::Open,
        OfferState::Accepted,
        OfferState::Done,
        OfferState::Cancelled,
        OfferState::Expired,
    ] {
        v.push_back(st.into_val(&env));
    }
    v.push_back(
        Offer {
            union: a.clone(),
            kind: OfferKind::Bring,
            inr: 1,
            expires: 2,
            state: OfferState::Accepted,
            lp: Some(a.clone()),
            usdc: 3,
            inr_per_usd: 4,
            sep24_id: soroban_sdk::String::from_str(&env, "s"),
            quote_id: soroban_sdk::String::from_str(&env, "q"),
            from_lp: 5,
            from_domestic: 6,
            from_offshore: 7,
        }
        .into_val(&env),
    );
    for t in [Tag::Offshore, Tag::Domestic] {
        v.push_back(DataKey::SeniorUsdc(a.clone(), t).into_val(&env));
    }
    v.push_back(
        SeniorInvestor {
            accepted: true,
            kyc_ref: h.clone(),
        }
        .into_val(&env),
    );
    v.push_back(
        RateFix {
            inr_per_usd: 1,
            valid_until: 2,
            set_by: a.clone(),
        }
        .into_val(&env),
    );
    for pu in [
        Purpose::Invest,
        Purpose::Repay,
        Purpose::Recovery,
        Purpose::Treasury,
        Purpose::Bring,
    ] {
        v.push_back(pu.into_val(&env));
    }
    v.push_back(
        Escrow {
            union: a.clone(),
            member: a.clone(),
            purpose: Purpose::Repay,
            amount: 1,
            remaining: 2,
            expires: 3,
        }
        .into_val(&env),
    );
    v.push_back(
        CashOut {
            member: a.clone(),
            union: a.clone(),
            amount: 1,
            expires: 2,
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
        hex, "955f17b96f8c9206c5c4f04fd98aff23174263343b70c328d91e510a4be3cc5e",
        "fx_pool storage layout changed"
    );
}
