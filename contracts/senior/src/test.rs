#![cfg(test)]
//! The guarantee pool (executive decision 23 Nov): deposits, the fee raising the index, a loss cover lowering
//! it, exits after 14 days within the daily cap. fx_pool, fund_core, roles and the LP's account are stand-ins
//! here; the fee runs end to end against the real contracts in `fund-core/src/test.rs`.

extern crate std;

use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env,
};

const ONE: i128 = 10_000_000;
const T0: u64 = 1_700_000_000;

#[contract]
struct MockFx;
#[contractimpl]
impl MockFx {
    pub fn rate(env: Env) -> Option<Fix> {
        Some(Fix {
            inr_per_usd: 90 * ONE,
            valid_until: env.ledger().timestamp() + 600,
            set_by: env.current_contract_address(),
        })
    }
    pub fn lp_member(env: Env, _union: Address, _lp: Address) -> Option<LpOf> {
        Some(LpOf {
            accepted: true,
            kyc_ref: BytesN::from_array(&env, &[1; 32]),
        })
    }
    pub fn id_checked(_env: Env, _account: Address) -> bool {
        true
    }
    pub fn lp_fee(_env: Env) -> u32 {
        100
    }
}

/// fund_core's guarantee fee: 1,800 rupees waiting.
#[contract]
struct MockCore;
#[contractimpl]
impl MockCore {
    pub fn take_guarantee_fee(_env: Env, _union: Address, amount: i128) -> i128 {
        assert!(amount <= 1_800 * ONE);
        amount
    }
}

#[contract]
struct MockRoles;
#[contractimpl]
impl MockRoles {
    pub fn is_leader(_env: Env, _union: Address, _account: Address) -> bool {
        true
    }
}

/// An LP's account that pays from its own USDC.
#[contract]
struct MockLp;
#[contractimpl]
impl MockLp {
    pub fn init(env: Env, usdc: Address) {
        env.storage().instance().set(&0u32, &usdc);
    }
    pub fn send_usdc(env: Env, to: Address, amount: i128) {
        let usdc: Address = env.storage().instance().get(&0u32).unwrap();
        TokenClient::new(&env, &usdc).transfer(&env.current_contract_address(), &to, &amount);
    }
}

struct S {
    env: Env,
    s: SeniorContractClient<'static>,
    usdc: Address,
    lp: Address,
}

fn setup() -> S {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(T0);
    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let fx = env.register(MockFx, ());
    let core = env.register(MockCore, ());
    let roles = env.register(MockRoles, ());
    let lp = env.register(MockLp, ());
    MockLpClient::new(&env, &lp).init(&usdc);
    let id = env.register(SeniorContract, (admin, usdc.clone(), core, fx, roles));
    S {
        env: env.clone(),
        s: SeniorContractClient::new(&env, &id),
        usdc,
        lp,
    }
}

impl S {
    fn investor(&self, usdc: i128) -> Address {
        let who = Address::generate(&self.env);
        self.s.set_investor(
            &who,
            &true,
            &BytesN::from_array(&self.env, &[2; 32]),
            &Tag::Offshore,
        );
        StellarAssetClient::new(&self.env, &self.usdc).mint(&who, &usdc);
        who
    }
    fn bal(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.usdc).balance(who)
    }
}

#[test]
fn deposits_wait_for_the_cap_and_residency_is_set_once() {
    let t = setup();
    let s = &t.s;
    let a = t.investor(1_000 * ONE);
    let kyc = BytesN::from_array(&t.env, &[2; 32]);
    assert_eq!(s.try_deposit(&a, &(10 * ONE)), Err(Ok(Error::AboveCap)));
    assert_eq!(
        s.try_set_investor(&a, &true, &kyc, &Tag::Domestic),
        Err(Ok(Error::TagFixed))
    );
    let b = Address::generate(&t.env);
    assert_eq!(s.try_deposit(&b, &ONE), Err(Ok(Error::NotAccepted)));
    s.set_cap(&(1_000 * ONE));
    assert_eq!(s.deposit(&a, &(400 * ONE)), 400 * ONE);
    assert_eq!(s.total(), 400 * ONE);
    assert_eq!(t.bal(&s.address), 400 * ONE);
    s.set_paused(&true);
    assert_eq!(s.try_deposit(&a, &ONE), Err(Ok(Error::Paused)));
}

/// The fee raises the index for every share; a later deposit buys fewer shares; a cover lowers it.
#[test]
fn the_fee_raises_the_index_and_a_cover_lowers_it() {
    let t = setup();
    let s = &t.s;
    s.set_cap(&(10_000 * ONE));
    let a = t.investor(1_000 * ONE);
    s.deposit(&a, &(1_000 * ONE));
    let union = Address::generate(&t.env);
    let leader = Address::generate(&t.env);
    // No fee into an empty pool.
    let empty = setup();
    let (eu, el) = (Address::generate(&empty.env), Address::generate(&empty.env));
    assert_eq!(
        empty
            .s
            .try_pay_fee(&eu, &el, &empty.lp, &(900 * ONE), &(10 * ONE)),
        Err(Ok(Error::NoShares))
    );
    // ₹900 at ₹90 = 10 USDC, less the LP's 1%: 9.9 USDC at least.
    StellarAssetClient::new(&t.env, &t.usdc).mint(&t.lp, &(100 * ONE));
    assert_eq!(
        s.try_pay_fee(&union, &leader, &t.lp, &(900 * ONE), &(98_000_000)),
        Err(Ok(Error::BelowFix))
    );
    s.pay_fee(&union, &leader, &t.lp, &(900 * ONE), &(10 * ONE));
    assert_eq!(s.total(), 1_010 * ONE);
    // Shares are valued rounded down, a stroop at most, in the pool's favour.
    assert!(1_010 * ONE - s.holding(&a).1 <= 1);
    assert_eq!(t.bal(&s.address), 1_010 * ONE);
    // A new investor buys at the new index: her 101 USDC is worth 101 USDC, not more.
    let b = t.investor(101 * ONE);
    s.deposit(&b, &(101 * ONE));
    assert!(101 * ONE - s.holding(&b).1 <= 2);
    // A cover: proposed, refused before 48 hours, then paid; every share falls pro rata.
    let to = Address::generate(&t.env);
    assert_eq!(
        s.try_propose_cover(&union, &to, &(2_000 * ONE)),
        Err(Ok(Error::NoCover))
    );
    s.propose_cover(&union, &to, &(111 * ONE));
    assert_eq!(s.try_execute_cover(), Err(Ok(Error::NotReady)));
    t.env.ledger().set_timestamp(T0 + COVER_DELAY);
    assert_eq!(s.execute_cover(), 111 * ONE);
    assert_eq!(t.bal(&to), 111 * ONE);
    assert_eq!(s.total(), 1_000 * ONE);
    // 111 of 1,111 USDC paid out: every share keeps 1,000 / 1,111 of its value.
    assert!((s.holding(&a).1 - 1_010 * ONE * 1_000 / 1_111).abs() <= 3);
    assert!((s.holding(&b).1 - 101 * ONE * 1_000 / 1_111).abs() <= 3);
    assert_eq!(s.pending_cover(), None);
    // Cancel works the same way, and nothing moves.
    s.propose_cover(&union, &to, &ONE);
    s.cancel_cover();
    assert_eq!(s.total(), 1_000 * ONE);
}

/// An exit waits 14 days, is paid at the index of then (a cover during the wait counts) and within the daily cap.
#[test]
fn exits_wait_14_days_and_stay_within_the_daily_cap() {
    let t = setup();
    let s = &t.s;
    s.set_cap(&(10_000 * ONE));
    s.set_daily_cap(&(500 * ONE));
    let a = t.investor(1_000 * ONE);
    let shares = s.deposit(&a, &(1_000 * ONE));
    assert_eq!(
        s.try_request_redeem(&a, &(shares + 1)),
        Err(Ok(Error::AboveShares))
    );
    s.request_redeem(&a, &(shares / 2));
    assert_eq!(s.try_redeem(&a), Err(Ok(Error::NotReady)));
    // A cover in the meantime hits the pending shares too.
    s.propose_cover(&a, &Address::generate(&t.env), &(100 * ONE));
    t.env.ledger().set_timestamp(T0 + COVER_DELAY);
    s.execute_cover();
    t.env.ledger().set_timestamp(T0 + UNBOND);
    assert_eq!(s.redeem(&a), 450 * ONE);
    assert_eq!(t.bal(&a), 450 * ONE);
    assert_eq!(s.paid_today(), 450 * ONE);
    // The rest the same day passes the cap: it waits for the next day.
    s.request_redeem(&a, &s.holding(&a).0.shares);
    t.env.ledger().set_timestamp(T0 + 2 * UNBOND);
    assert_eq!(s.redeem(&a), 450 * ONE);
    assert_eq!(s.total(), 0);
    assert_eq!(s.total_shares(), 0);
    assert_eq!(s.holding(&a).0, Holding::default());
    assert_eq!(s.daily_cap(), 500 * ONE);
    assert_eq!(s.try_set_daily_cap(&-1), Err(Ok(Error::AmountZero)));
}

#[test]
fn the_daily_cap_counts_per_utc_day() {
    let t = setup();
    let s = &t.s;
    s.set_cap(&(10_000 * ONE));
    s.set_daily_cap(&(100 * ONE));
    let a = t.investor(300 * ONE);
    let sh = s.deposit(&a, &(300 * ONE));
    s.request_redeem(&a, &(sh / 3 * 2));
    t.env.ledger().set_timestamp(T0 + UNBOND);
    assert_eq!(s.try_redeem(&a), Err(Ok(Error::AboveDailyCap)));
    assert_eq!(s.daily_cap(), 100 * ONE);
}

#[test]
fn only_the_trust_sets_caps_investors_covers_and_upgrades() {
    let t = setup();
    let s = &t.s;
    let kyc = BytesN::from_array(&t.env, &[1; 32]);
    let x = Address::generate(&t.env);
    t.env.set_auths(&[]);
    assert!(s.try_set_cap(&1).is_err());
    assert!(s.try_set_daily_cap(&1).is_err());
    assert!(s.try_set_investor(&x, &true, &kyc, &Tag::Offshore).is_err());
    assert!(s.try_set_paused(&true).is_err());
    assert!(s.try_propose_cover(&x, &x, &1).is_err());
    assert!(s.try_cancel_cover().is_err());
    assert!(s.try_upgrade(&kyc).is_err());
    assert_eq!(s.upgrade_delay(), UPGRADE_DELAY);
    assert_eq!(s.daily_cap(), DEFAULT_DAILY_CAP);
    assert_eq!(s.index(), nila_math::RAY);
}

#[test]
fn storage_layout_is_frozen() {
    use soroban_sdk::{xdr::ToXdr, IntoVal, Val};
    let env = Env::default();
    let a = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let h = BytesN::from_array(&env, &[7; 32]);
    let mut v: soroban_sdk::Vec<Val> = soroban_sdk::Vec::new(&env);
    for k in [
        DataKey::Admin,
        DataKey::Usdc,
        DataKey::FundCore,
        DataKey::FxPool,
        DataKey::Roles,
        DataKey::Paused,
        DataKey::Cap,
        DataKey::Total,
        DataKey::Investor(a.clone()),
        DataKey::DailyCap,
        DataKey::DayOut(1),
        DataKey::Index,
        DataKey::TotalShares,
        DataKey::Holding(a.clone()),
        DataKey::PendingCover,
    ] {
        v.push_back(k.into_val(&env));
    }
    v.push_back(
        Investor {
            accepted: true,
            kyc_ref: h.clone(),
            tag: Tag::Domestic,
        }
        .into_val(&env),
    );
    v.push_back(
        Holding {
            shares: 1,
            pending: 2,
            request_ts: 3,
        }
        .into_val(&env),
    );
    v.push_back(
        Cover {
            union: a.clone(),
            to: a.clone(),
            usdc: 4,
            eta: 5,
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
        hex, "91deb223a1a9e9f72189efa0ca7686a39cb1be8b7097d524c9ee71b7de8c716e",
        "senior storage layout changed"
    );
}
