#![cfg(test)]
//! Unit tests for fx_pool's own rules. The cash scan, cash-out and trustline flip run
//! end to end against fund_core in `fund-core/src/test.rs`.

extern crate std;

use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    Address, Env, Symbol, Vec,
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
fn unknown_escrow() {
    let (env, f, _) = setup();
    let u = Address::generate(&env);
    assert_eq!(f.try_burn_expired_escrow(&u, &1), Err(Ok(Error::NotFound)));
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
    v.push_back(DataKey::OpenEscrows(a.clone()).into_val(&env));
    v.push_back(DataKey::CashAtHand(a.clone()).into_val(&env));
    v.push_back(DataKey::Senior.into_val(&env));
    v.push_back(DataKey::AttesterFixAt.into_val(&env));
    v.push_back(DataKey::Rate.into_val(&env));
    v.push_back(DataKey::Anchor.into_val(&env));
    v.push_back(DataKey::LpOf(a.clone(), a.clone()).into_val(&env));
    v.push_back(DataKey::LpFeeBp.into_val(&env));
    v.push_back(DataKey::NextOffer.into_val(&env));
    v.push_back(DataKey::Offer(1).into_val(&env));
    v.push_back(DataKey::OffersOf(a.clone()).into_val(&env));
    v.push_back(DataKey::DayOut(1).into_val(&env));
    v.push_back(DataKey::RateBandBp.into_val(&env));
    v.push_back(DataKey::SwapKey.into_val(&env));
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
    // A union's open escrows in one entry (24 Oct).
    v.push_back(
        OpenEscrow {
            id: 1,
            member: a.clone(),
            purpose: Purpose::Invest,
            amount: 1,
            remaining: 1,
            expires: 1,
        }
        .into_val(&env),
    );
    let got: BytesN<32> = env.crypto().sha256(&v.to_xdr(&env)).into();
    let hex: std::string::String = got
        .to_array()
        .iter()
        .map(|b| std::format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex, "f825389a33a6862da2638e13bd7727809b234b08d39de8267e97b564b5ace2a5",
        "fx_pool storage layout changed"
    );
}

/// A stand-in for Reflector's forex oracle: its last INR prices (USD per INR, 14 decimals), newest first.
#[contract]
struct MockReflector;

#[contractimpl]
impl MockReflector {
    pub fn put(env: Env, prices: Vec<ReflectorPrice>) {
        env.storage().instance().set(&0u32, &prices);
    }
    pub fn prices(env: Env, asset: ReflectorAsset, records: u32) -> Option<Vec<ReflectorPrice>> {
        if asset != ReflectorAsset::Other(Symbol::new(&env, "INR")) {
            return None;
        }
        let all: Vec<ReflectorPrice> = env.storage().instance().get(&0u32)?;
        Some(all.slice(0..records.min(all.len())))
    }
    pub fn decimals(_env: Env) -> u32 {
        14
    }
}

/// An hour of prices five minutes apart, newest first: `latest` now, the 11 before it at `before`.
fn hour(env: &Env, now: u64, latest: i128, before: i128) -> Vec<ReflectorPrice> {
    let mut v = Vec::new(env);
    v.push_back(ReflectorPrice {
        price: latest,
        timestamp: now,
    });
    for i in 1..12u64 {
        v.push_back(ReflectorPrice {
            price: before,
            timestamp: now - 300 * i,
        });
    }
    v
}

/// The rate in force (T2 rows 8–9; 23 Nov): Reflector while at most 15 minutes old and within 3% of its last hour's
/// average, else the attester's fix while valid; a jump beyond 3% stops every rate (the circuit breaker); no trust
/// override on mainnet builds.
#[test]
fn reflector_prices_with_a_staleness_guard_and_a_circuit_breaker() {
    let (env, f, _) = setup();
    let t0 = env.ledger().timestamp();
    let oracle = env.register(MockReflector, ());
    let m = MockReflectorClient::new(&env, &oracle);
    // 92.80 INR per USD = 0.010775862068965 USD per INR at 14 decimals; 93.00 = 0.010752688172043.
    let (p928, p930) = (1_077_586_206_896i128, 1_075_268_817_204i128);
    m.put(&hour(&env, t0, p928, p930));
    // The attester's fix (written as the attester would): ₹93, valid a day.
    let attester = Address::generate(&env);
    let fix = RateFix {
        inr_per_usd: 93 * NIN,
        valid_until: t0 + 86_400,
        set_by: attester.clone(),
    };
    env.as_contract(&f.address, || {
        env.storage().instance().set(&DataKey::Rate, &fix)
    });
    assert_eq!(f.rate(), Some(fix.clone()));
    // Reflector set: its latest price is the rate, valid 15 minutes from its time.
    f.set_reflector(&Some(oracle.clone()));
    assert_eq!(f.reflector().unwrap().decimals, 14);
    let r = f.rate().unwrap();
    assert_eq!(
        (r.inr_per_usd, r.valid_until, r.set_by.clone()),
        (928_000_000, t0 + 900, oracle.clone())
    );
    assert_eq!(f.fix(), Some(fix.clone()));
    // A jump of 4% against the last hour: no rate at all, not the attester's fix either.
    m.put(&hour(&env, t0, p930 * 104 / 100, p930));
    assert_eq!(f.rate(), None);
    env.as_contract(&f.address, || {
        assert_eq!(current_rate(&env), Err(Error::SuspiciousRate))
    });
    // 2.9% is within the band.
    m.put(&hour(&env, t0, p930 * 1029 / 1000, p930));
    assert!(f.rate().is_some());
    // Stale (over 15 minutes): the attester's fix stands in.
    m.put(&hour(&env, t0, p928, p930));
    env.ledger().set_timestamp(t0 + 901);
    assert_eq!(f.rate(), Some(fix.clone()));
    // A zero or future latest price is not used.
    m.put(&hour(&env, t0 + 901, 0, p930));
    assert_eq!(f.rate(), Some(fix.clone()));
    m.put(&hour(&env, t0 + 2_000, p930, p930));
    assert_eq!(f.rate(), Some(fix.clone()));
    // Cleared: the attester's fix only.
    f.set_reflector(&None);
    assert_eq!(f.reflector(), None);
    assert_eq!(f.rate(), Some(fix));
    // Only the trust sets it.
    env.set_auths(&[]);
    assert!(f.try_set_reflector(&Some(oracle)).is_err());
}
