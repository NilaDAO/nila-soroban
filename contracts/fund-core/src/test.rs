#![cfg(test)]

extern crate std;

use super::*;
use nila_fx_pool::{FxPoolContract, FxPoolContractClient};
use nila_land_title::{LandTitleContract, LandTitleContractClient};
use nila_math::{NIN, YEAR};
use nila_roles::RolesContract;
use soroban_sdk::{
    testutils::{Address as _, IssuerFlags, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

const T0: u64 = 1_700_000_000;
/// A last draw date far ahead, for the re-attest refusals (not drawn on).
const FAR: u64 = u64::MAX;

struct Fx {
    env: Env,
    oracle: Address,
    leader: Address,
    union: Address,
    borrower: Address,
    neighbour: Address,
    core_id: Address,
    fx_id: Address,
    nin: Address,
    titles_id: Address,
    usdc: Address,
    attester: Address,
    roles_id: Address,
    cal_id: Address,
    next_title: core::cell::Cell<u64>,
}

/// A stand-in crop calendar (25 Oct): every token gets the window the test sets (`Fx::window`).
mod mock_calendar {
    use soroban_sdk::{contract, contractimpl, symbol_short, Env};

    #[contract]
    pub struct MockCalendar;

    #[contractimpl]
    impl MockCalendar {
        pub fn set(env: Env, w: Option<(u64, u64)>) {
            env.storage().instance().set(&symbol_short!("w"), &w);
        }

        pub fn window(env: Env, _crop_code: u32, _sos: u64) -> Option<(u64, u64)> {
            env.storage()
                .instance()
                .get(&symbol_short!("w"))
                .unwrap_or(None)
        }
    }
}

impl Fx {
    fn core(&self) -> FundCoreClient<'_> {
        FundCoreClient::new(&self.env, &self.core_id)
    }

    fn token(&self) -> TokenClient<'_> {
        TokenClient::new(&self.env, &self.nin)
    }

    fn fx(&self) -> FxPoolContractClient<'_> {
        FxPoolContractClient::new(&self.env, &self.fx_id)
    }

    /// `claim_junior` pays a leader's own investment to her wallet (members collect over the desk with
    /// `claim_junior_cash`, 22 Oct): tests of the wallet payout and the cash-out make the investor a leader.
    /// The window the stand-in calendar gives every food token; None: no sowing window.
    fn window(&self, w: Option<(u64, u64)>) {
        mock_calendar::MockCalendarClient::new(&self.env, &self.cal_id).set(&w);
    }

    fn make_leader(&self, who: &Address) {
        nila_roles::RolesContractClient::new(&self.env, &self.roles_id).set_leader(
            &self.union,
            who,
            &true,
        );
    }

    fn give_title(&self, who: &Address) {
        let id = self.next_title.get();
        self.next_title.set(id + 1);
        LandTitleContractClient::new(&self.env, &self.titles_id).set_title(
            &id,
            &self.attester,
            who,
            &self.union,
            &BytesN::from_array(&self.env, &[0xAB; 20]),
            &BytesN::from_array(&self.env, &[1; 32]),
            &BytesN::from_array(&self.env, &[2; 32]),
        );
    }

    /// Cash-scan INVEST: fx_pool mints the nIN for her rupees and deposits it.
    fn invest(&self, member: &Address, amount: i128) {
        StellarAssetClient::new(&self.env, &self.nin).mint(&self.fx_id, &amount);
        self.core().deposit_junior(&self.union, member, &amount);
    }

    /// Cash-scan REPAY: fx_pool mints the nIN for the rupees she brings and repays.
    fn repay(&self, id: &BytesN<32>, amount: i128) -> bool {
        StellarAssetClient::new(&self.env, &self.nin).mint(&self.fx_id, &amount);
        self.core().repay_loan(&self.union, id, &amount)
    }

    /// F2: the attester copies her food token, its window open now. Tests use the loan id as token id.
    fn attest(&self, member: &Address, token: &BytesN<32>, amount: i128) {
        self.core()
            .attest_food_token(&self.attester, &self.union, token, member, &amount, &0);
    }

    fn draw(&self, id: &BytesN<32>, cash: i128) {
        self.attest(&self.borrower, id, cash);
        self.core()
            .draw_loan(&self.union, &self.borrower, id, &cash, &0);
    }

    fn at(&self, ts: u64) {
        self.env.ledger().set_timestamp(ts);
    }

    /// fund_core's nIN equals the pools plus the union's treasury and rainy day.
    fn assert_conserved(&self) {
        let c = self.core();
        let books = c.get_junior_market(&self.union).cash
            + c.get_senior_market(&self.union).cash
            + c.union_treasury(&self.union)
            + c.union_rainy_day(&self.union);
        assert_eq!(self.token().balance(&self.core_id), books);
    }
}

fn loan_id(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

fn union_cfg() -> UnionConfig {
    UnionConfig {
        max_loan: 10_000 * NIN,
        gate: 600 * NIN,
        rate_bp: 1250,
        treasury_fee_bp: 100,
        rainy_fee_bp: 77,
        safety_bp: 0,
        safety_floor: 0,
        hard_stop: false,
        enforce_token_amount: false,
    }
}

fn setup() -> Fx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(T0);

    let admin = Address::generate(&env);
    let attester = Address::generate(&env);
    let oracle = Address::generate(&env);
    let leader = Address::generate(&env);
    let union = Address::generate(&env);
    let borrower = Address::generate(&env);
    let neighbour = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let nin = sac.address();
    // R8: authorisation required, revocable, clawback, as on testnet.
    sac.issuer().set_flag(IssuerFlags::RequiredFlag);
    sac.issuer().set_flag(IssuerFlags::RevocableFlag);
    sac.issuer().set_flag(IssuerFlags::ClawbackEnabledFlag);

    let roles_id = env.register(RolesContract, (admin.clone(),));
    let roles = nila_roles::RolesContractClient::new(&env, &roles_id);
    roles.set_role(&oracle, &nila_roles::Role::Oracle, &true);
    roles.set_role(&attester, &nila_roles::Role::Attester, &true);
    roles.set_leader(&union, &leader, &true);

    let titles_id = env.register(LandTitleContract, (admin.clone(), roles_id.clone()));
    let roles_keep = roles_id.clone();
    let core_id = env.register(
        FundCore,
        (
            admin.clone(),
            roles_id.clone(),
            titles_id.clone(),
            nin.clone(),
        ),
    );
    let usdc = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let fx_id = env.register(
        FxPoolContract,
        (
            admin.clone(),
            roles_id,
            titles_id.clone(),
            nin.clone(),
            usdc.clone(),
            25_000 * NIN,
        ),
    );
    FxPoolContractClient::new(&env, &fx_id).set_fund_core(&core_id);
    // R8: fx_pool is the nIN SAC admin and authorises the two contract balances.
    StellarAssetClient::new(&env, &nin).set_admin(&fx_id);
    FxPoolContractClient::new(&env, &fx_id).activate();
    // C9b: the anchor's swap key signs each LP accept.
    FxPoolContractClient::new(&env, &fx_id).set_swap_key(&BytesN::from_array(
        &env,
        &swap_key().verifying_key().to_bytes(),
    ));

    let cal_id = env.register(mock_calendar::MockCalendar, ());
    let fx = Fx {
        env,
        oracle,
        leader,
        union,
        borrower,
        neighbour,
        core_id,
        fx_id,
        nin,
        titles_id,
        usdc,
        attester,
        roles_id: roles_keep,
        cal_id,
        next_title: core::cell::Cell::new(1),
    };
    fx.core().set_fx_pool(&fx.fx_id);
    // The crop calendar: by default every share is usable now and has no last date in reach.
    fx.core().set_crop_calendar(&fx.cal_id);
    fx.window(Some((0, FAR)));
    fx.core().set_union_config(&fx.union, &union_cfg());
    fx.give_title(&fx.borrower);
    fx.give_title(&fx.neighbour);
    fx
}

#[test]
fn draw_then_repay_with_interest_and_fees() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.invest(&fx.neighbour, 1_000 * NIN);

    fx.draw(&id, 500 * NIN);
    // D9: the leader hands her rupees; the loan's nIN is burned, none reaches her wallet.
    assert_eq!(fx.token().balance(&fx.borrower), 0);
    assert_eq!(fx.token().balance(&fx.core_id), 1_500 * NIN);
    assert_eq!(c.get_junior_market(&fx.union).cash, 1_500 * NIN);
    assert_eq!(c.union_borrows(&fx.union), 500 * NIN);
    assert_eq!(c.loans_of(&fx.union, &fx.borrower).len(), 1);
    fx.assert_conserved();

    fx.at(T0 + YEAR / 2);
    let interest = 500 * NIN * 1250 / 10_000 / 2;
    // Overpaying takes only what is owed.
    assert!(fx.repay(&id, 600 * NIN));
    assert_eq!(
        fx.token().balance(&fx.fx_id),
        600 * NIN - 500 * NIN - interest
    );
    let ln = c.get_loan(&fx.union, &id).unwrap();
    assert!(ln.closed && !ln.defaulted);
    assert_eq!(ln.interest_paid, interest);
    assert_eq!(c.union_treasury(&fx.union), interest / 100);
    assert_eq!(c.union_rainy_day(&fx.union), interest * 77 / 10_000);
    assert_eq!(c.union_borrows(&fx.union), 0);
    fx.assert_conserved();

    // The net interest is shared by both investors through the share value.
    let p = c.position(&fx.union, &fx.neighbour);
    let net = interest - interest / 100 - interest * 77 / 10_000;
    assert!((p.earned - net / 2).abs() <= 1);
    assert_eq!(p.locked, 0);
}

#[test]
fn draw_needs_only_the_member_no_oracle() {
    let fx = setup();
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.draw(&loan_id(&fx.env, 1), 100 * NIN);
    let auths = fx.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, fx.borrower);
}

#[test]
fn draw_checks_title_gate_cap_and_cash() {
    let fx = setup();
    let c = fx.core();
    let stranger = Address::generate(&fx.env);
    fx.invest(&fx.borrower, 599 * NIN);
    let id = loan_id(&fx.env, 1);

    let err = c
        .try_draw_loan(&fx.union, &stranger, &id, &(10 * NIN), &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NftRequired));
    // R26: 600 nIN invested before any crop share cash.
    fx.attest(&fx.borrower, &id, 20_000 * NIN);
    let err = c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &(10 * NIN), &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::BelowGate));

    fx.invest(&fx.borrower, NIN);
    fx.invest(&fx.neighbour, 20_000 * NIN);
    // R34: ₹10,000 per loan.
    let err = c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &(10_000 * NIN + 1), &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::CapExceeded));
    c.draw_loan(&fx.union, &fx.borrower, &id, &(10_000 * NIN), &0);
    // One loan per token: the token id is the loan id.
    let err = c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &NIN, &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::FoodTokenUsed));

    // Cash comes from the junior pool only.
    let other = setup();
    other.invest(&other.borrower, 600 * NIN);
    let t = loan_id(&other.env, 1);
    other.attest(&other.borrower, &t, 1_000 * NIN);
    let err = other
        .core()
        .try_draw_loan(&other.union, &other.borrower, &t, &(601 * NIN), &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::InsufficientCash));
}

#[test]
fn money_comes_in_only_through_fx_pool() {
    let fx = setup();
    fx.invest(&fx.borrower, 1_000 * NIN);
    let auths = fx.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, fx.fx_id);

    // No fx_pool configured: nothing can come in.
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let roles = env.register(RolesContract, (admin.clone(),));
    let titles = env.register(LandTitleContract, (admin.clone(), roles.clone()));
    let nin = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let core = env.register(FundCore, (admin, roles, titles, nin));
    let c = FundCoreClient::new(&env, &core);
    let member = Address::generate(&env);
    let union = Address::generate(&env);
    assert_eq!(
        c.try_repay_loan(&union, &loan_id(&env, 1), &NIN)
            .unwrap_err(),
        Ok(Error::NotFxPool)
    );
    assert!(c.try_deposit_junior(&union, &member, &NIN).is_err());
}

#[test]
fn deposits_need_a_title_and_senior_rejects_members() {
    let fx = setup();
    let c = fx.core();
    let stranger = Address::generate(&fx.env);
    assert_eq!(
        c.try_deposit_junior(&fx.union, &stranger, &NIN)
            .unwrap_err(),
        Ok(Error::NftRequired)
    );
    assert_eq!(
        c.try_deposit_senior(&fx.union, &fx.borrower, &NIN)
            .unwrap_err(),
        Ok(Error::NftForbidden)
    );
}

#[test]
fn lock_holds_what_she_owes_and_frees_with_repayment() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.invest(&fx.neighbour, 1_000 * NIN);
    fx.draw(&id, 800 * NIN);

    let p = c.position(&fx.union, &fx.borrower);
    assert_eq!(p.invested, 1_000 * NIN);
    assert_eq!(p.locked, 800 * NIN);
    assert_eq!(p.free, 200 * NIN);
    // Shares are 1:1 with nIN before any interest.
    assert_eq!(
        c.try_request_unbond_junior(&fx.union, &fx.borrower, &(201 * NIN))
            .unwrap_err(),
        Ok(Error::BadShares)
    );
    c.request_unbond_junior(&fx.union, &fx.borrower, &(200 * NIN));

    // Half repaid at once: half the lock frees (no interest yet).
    fx.repay(&id, 400 * NIN);
    let p = c.position(&fx.union, &fx.borrower);
    assert_eq!(p.locked, 400 * NIN);
    assert_eq!(p.free, 400 * NIN);

    // Interest grows the lock.
    fx.at(T0 + YEAR);
    let p = c.position(&fx.union, &fx.borrower);
    assert_eq!(p.locked, 400 * NIN + 400 * NIN * 1250 / 10_000);
}

#[test]
fn unbond_claim_after_14_days_pays_yield_once() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.invest(&fx.neighbour, 1_000 * NIN);
    fx.draw(&id, 1_000 * NIN);
    fx.at(T0 + YEAR);
    fx.repay(&id, 1_125 * NIN);

    fx.make_leader(&fx.neighbour);
    fx.make_leader(&fx.borrower);
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(1_000 * NIN));
    assert_eq!(
        c.try_claim_junior(&fx.union, &fx.neighbour, &0)
            .unwrap_err(),
        Ok(Error::NotEligibleMaturity)
    );
    fx.at(T0 + YEAR + 14 * 86_400);
    c.claim_junior(&fx.union, &fx.neighbour, &0);
    c.request_unbond_junior(&fx.union, &fx.borrower, &(1_000 * NIN));
    fx.at(T0 + YEAR + 28 * 86_400);
    c.claim_junior(&fx.union, &fx.borrower, &0);

    // Everything paid out: only the fees (and rounding dust) stay behind.
    let interest = 125 * NIN;
    let fees = interest / 100 + interest * 77 / 10_000;
    let paid = fx.token().balance(&fx.neighbour) + fx.token().balance(&fx.borrower);
    assert!((paid - (2_000 * NIN + interest - fees)).abs() <= 2);
    assert!(fx.token().balance(&fx.core_id) - fees <= 2);
}

#[test]
fn no_end_of_season_means_no_overdue_and_no_default() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.draw(&id, 500 * NIN);
    fx.at(T0 + 5 * YEAR);
    assert_eq!(
        c.try_mark_default(&fx.union, &id).unwrap_err(),
        Ok(Error::NoMaturity)
    );
    assert_eq!(
        c.try_flag_overdue(&fx.union, &id).unwrap_err(),
        Ok(Error::NoMaturity)
    );
    assert_eq!(c.due_date(&fx.union, &id), None);
}

#[test]
fn maturity_is_the_oracles_fact_in_the_past() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.draw(&id, 500 * NIN);
    fx.at(T0 + 60 * DAY);
    assert_eq!(
        c.try_report_maturity(&fx.leader, &fx.union, &id, &(T0 + 50 * DAY))
            .unwrap_err(),
        Ok(Error::NotOracle)
    );
    assert_eq!(
        c.try_report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 61 * DAY))
            .unwrap_err(),
        Ok(Error::BadMaturity)
    );
    assert_eq!(
        c.try_report_maturity(&fx.oracle, &fx.union, &id, &(T0 - 1))
            .unwrap_err(),
        Ok(Error::BadMaturity)
    );
    c.report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 50 * DAY));
    assert_eq!(
        c.try_report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 50 * DAY))
            .unwrap_err(),
        Ok(Error::MaturityAlreadySet)
    );
    // Harvest before drawdown + 90 days: due = drawdown + 90 + 21 days.
    assert_eq!(c.due_date(&fx.union, &id), Some(T0 + 111 * DAY));
}

#[test]
fn overdue_puts_the_union_on_hold_until_repaid() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.invest(&fx.neighbour, 1_000 * NIN);
    fx.draw(&id, 500 * NIN);
    fx.at(T0 + 120 * DAY);
    c.report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 120 * DAY));
    // due = max(drawdown + 90, eos) + 21 = eos + 21.
    fx.at(T0 + 141 * DAY);
    assert_eq!(
        c.try_flag_overdue(&fx.union, &id).unwrap_err(),
        Ok(Error::NotDue)
    );
    fx.at(T0 + 141 * DAY + 1);
    c.flag_overdue(&fx.union, &id);
    assert_eq!(c.on_hold(&fx.union), 1);
    assert_eq!(
        c.try_flag_overdue(&fx.union, &id).unwrap_err(),
        Ok(Error::BadState)
    );

    let id2 = loan_id(&fx.env, 2);
    fx.attest(&fx.neighbour, &id2, NIN);
    let err = c
        .try_draw_loan(&fx.union, &fx.neighbour, &id2, &NIN, &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::LendingOnHold));

    // A part payment keeps the hold; the full payment lifts it.
    fx.repay(&id, 100 * NIN);
    assert_eq!(c.on_hold(&fx.union), 1);
    assert!(fx.repay(&id, 1_000 * NIN));
    assert_eq!(c.on_hold(&fx.union), 0);
    c.draw_loan(&fx.union, &fx.neighbour, &id2, &NIN, &0);
}

#[test]
fn default_waits_for_the_payback_date_and_anyone_may_call() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.draw(&id, 500 * NIN);
    fx.at(T0 + 100 * DAY);
    c.report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 100 * DAY));
    // maturityTs + 21 days (D-d) is also the payback date here.
    fx.at(T0 + 121 * DAY);
    assert_eq!(
        c.try_mark_default(&fx.union, &id).unwrap_err(),
        Ok(Error::NotDue)
    );
    fx.at(T0 + 121 * DAY + 1);
    let before = fx.env.auths().len();
    c.mark_default(&fx.union, &id);
    assert_eq!(fx.env.auths().len(), 0, "no signature needed ({before})");
    assert_eq!(
        c.try_mark_default(&fx.union, &id).unwrap_err(),
        Ok(Error::LoanClosed)
    );
}

#[test]
fn default_takes_her_investment_first_then_the_junior_pool() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 600 * NIN);
    fx.invest(&fx.neighbour, 1_400 * NIN);
    fx.draw(&id, 1_000 * NIN);
    fx.at(T0 + 100 * DAY);
    c.report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 100 * DAY));
    fx.at(T0 + 200 * DAY);
    c.mark_default(&fx.union, &id);

    // Her 600 first (interest is owed too, so all of it), then 400 off the pool:
    // the neighbour's 1,400 is now 1,000.
    let her = c.position(&fx.union, &fx.borrower);
    assert_eq!(her.invested, 0);
    let p = c.position(&fx.union, &fx.neighbour);
    assert!((p.invested - 1_000 * NIN).abs() <= 1);
    assert_eq!(c.union_borrows(&fx.union), 0);
    assert_eq!(c.get_junior_market(&fx.union).total_borrows, 0);
    let ln = c.get_loan(&fx.union, &id).unwrap();
    assert!(ln.defaulted && ln.closed);
    fx.assert_conserved();
}

#[test]
fn default_covered_by_her_own_investment_spares_the_pool() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.invest(&fx.neighbour, 1_000 * NIN);
    fx.draw(&id, 300 * NIN);
    fx.at(T0 + 100 * DAY);
    c.report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 100 * DAY));
    fx.at(T0 + 200 * DAY);
    c.mark_default(&fx.union, &id);
    // She owes 300 plus 200 days of 12.5% on it; her shares pay both. The interest part is
    // pool income, shared pro rata by the remaining shares, as at a repayment.
    let interest = 300 * NIN * 1250 / 10_000 * 200 / 365;
    let her = c.position(&fx.union, &fx.borrower).invested;
    let his = c.position(&fx.union, &fx.neighbour).invested;
    assert!((her + his - 1_700 * NIN).abs() <= 2);
    let his_share = interest * 1_000 * NIN / (1_700 * NIN - interest);
    assert!((his - 1_000 * NIN - his_share).abs() <= NIN / 100);
    assert!(her < 700 * NIN);
    fx.assert_conserved();
}

#[test]
fn recovery_after_default_goes_back_to_the_pool() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 600 * NIN);
    fx.invest(&fx.neighbour, 1_400 * NIN);
    fx.draw(&id, 1_000 * NIN);
    assert_eq!(
        c.try_credit_recovery(&fx.union, &id, &NIN).unwrap_err(),
        Ok(Error::NotDefaulted)
    );
    fx.at(T0 + 100 * DAY);
    c.report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 100 * DAY));
    fx.at(T0 + 200 * DAY);
    c.mark_default(&fx.union, &id);
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(400 * NIN));
    c.credit_recovery(&fx.union, &id, &(400 * NIN));
    assert!((c.position(&fx.union, &fx.neighbour).invested - 1_400 * NIN).abs() <= 1);
    fx.assert_conserved();
}

#[test]
fn no_admin_or_oracle_as_leader() {
    let fx = setup();
    let c = fx.core();
    let admin = c.get_config().admin;
    let err = c
        .try_withdraw_union_treasury(&admin, &fx.union, &NIN, &false)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotLeader));
    let err = c
        .try_withdraw_union_treasury(&fx.oracle, &fx.union, &NIN, &false)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NotLeader));
    let err = c
        .try_withdraw_union_treasury(&fx.leader, &fx.union, &NIN, &false)
        .unwrap_err();
    assert_eq!(err, Ok(Error::InsufficientCash));
}

#[test]
fn pause_stops_draws_and_money_in() {
    let fx = setup();
    let c = fx.core();
    fx.invest(&fx.borrower, 1_000 * NIN);
    c.pause();
    let err = c
        .try_draw_loan(&fx.union, &fx.borrower, &loan_id(&fx.env, 1), &NIN, &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::Paused));
    assert_eq!(
        c.try_deposit_junior(&fx.union, &fx.borrower, &NIN)
            .unwrap_err(),
        Ok(Error::Paused)
    );
    c.unpause();
    fx.draw(&loan_id(&fx.env, 1), NIN);
}

#[test]
fn union_config_is_admin_only_and_checked() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let roles = env.register(RolesContract, (admin.clone(),));
    let titles = env.register(LandTitleContract, (admin.clone(), roles.clone()));
    let nin = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let core = env.register(FundCore, (admin, roles, titles, nin));
    let c = FundCoreClient::new(&env, &core);
    let union = Address::generate(&env);
    // No auth mocked: the admin's signature is missing.
    assert!(c.try_set_union_config(&union, &union_cfg()).is_err());
    env.mock_all_auths();
    let mut bad = union_cfg();
    bad.treasury_fee_bp = 9_000;
    bad.rainy_fee_bp = 2_000;
    assert_eq!(
        c.try_set_union_config(&union, &bad).unwrap_err(),
        Ok(Error::BadArg)
    );
    c.set_union_config(&union, &union_cfg());
    assert_eq!(c.get_union_config(&union), Some(union_cfg()));
}

#[test]
fn a_senior_unbond_does_not_hold_back_junior_lending() {
    let fx = setup();
    let c = fx.core();
    let lp = Address::generate(&fx.env);
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(1_000 * NIN));
    c.deposit_senior(&fx.union, &lp, &(1_000 * NIN));
    c.request_unbond_senior(&fx.union, &lp, &(1_000 * NIN));
    fx.invest(&fx.borrower, 1_000 * NIN);
    // The senior queue's reserve is senior's own (I3): all junior cash can still lend.
    fx.draw(&loan_id(&fx.env, 1), 1_000 * NIN);
}

#[test]
fn pause_also_stops_overdue_default_and_treasury() {
    let fx = setup();
    let c = fx.core();
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.draw(&id, 500 * NIN);
    fx.at(T0 + 100 * DAY);
    c.report_maturity(&fx.oracle, &fx.union, &id, &(T0 + 100 * DAY));
    fx.at(T0 + 200 * DAY);
    c.pause();
    assert_eq!(
        c.try_flag_overdue(&fx.union, &id).unwrap_err(),
        Ok(Error::Paused)
    );
    assert_eq!(
        c.try_mark_default(&fx.union, &id).unwrap_err(),
        Ok(Error::Paused)
    );
    assert_eq!(
        c.try_withdraw_union_treasury(&fx.leader, &fx.union, &NIN, &false)
            .unwrap_err(),
        Ok(Error::Paused)
    );
    c.unpause();
    c.mark_default(&fx.union, &id);
}

#[test]
fn closed_loans_leave_the_open_list_and_recovery_is_capped() {
    let fx = setup();
    let c = fx.core();
    let (a, b) = (loan_id(&fx.env, 1), loan_id(&fx.env, 2));
    fx.invest(&fx.borrower, 600 * NIN);
    fx.invest(&fx.neighbour, 1_400 * NIN);
    fx.draw(&a, 100 * NIN);
    fx.draw(&b, 900 * NIN);
    assert_eq!(c.loans_of(&fx.union, &fx.borrower).len(), 2);
    fx.repay(&a, 100 * NIN);
    assert_eq!(c.loans_of(&fx.union, &fx.borrower).len(), 1);
    fx.at(T0 + 100 * DAY);
    c.report_maturity(&fx.oracle, &fx.union, &b, &(T0 + 100 * DAY));
    fx.at(T0 + 200 * DAY);
    c.mark_default(&fx.union, &b);
    assert_eq!(c.loans_of(&fx.union, &fx.borrower).len(), 0);
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(1_000 * NIN));
    assert_eq!(
        c.try_credit_recovery(&fx.union, &b, &(900 * NIN + 1))
            .unwrap_err(),
        Ok(Error::CapExceeded)
    );
    c.credit_recovery(&fx.union, &b, &(900 * NIN));
    assert_eq!(
        c.try_credit_recovery(&fx.union, &b, &1).unwrap_err(),
        Ok(Error::CapExceeded)
    );
    assert_eq!(
        c.try_report_milestone(&fx.oracle, &fx.union, &b, &1, &loan_id(&fx.env, 9))
            .unwrap_err(),
        Ok(Error::LoanClosed)
    );
    fx.assert_conserved();
}

#[test]
fn upgrade_waits_48_hours_and_the_delay_is_lowered_only_through_it() {
    let fx = setup();
    let c = fx.core();
    assert_eq!(c.upgrade_delay(), 48 * 3600);
    let h = loan_id(&fx.env, 0xAA);
    c.upgrade(&h);
    assert_eq!(c.pending_upgrade().unwrap().eta, T0 + 48 * 3600);
    assert_eq!(c.try_execute_upgrade(), Err(Ok(Error::UpgradeNotReady)));
    c.cancel_upgrade();
    assert_eq!(c.pending_upgrade(), None);
    // Only the admin.
    fx.env.set_auths(&[]);
    assert!(c.try_upgrade(&h).is_err());
    fx.env.mock_all_auths();

    c.set_upgrade_delay(&0);
    assert_eq!(c.upgrade_delay(), 48 * 3600);
    fx.at(T0 + 48 * 3600);
    c.set_upgrade_delay(&0);
    assert_eq!(c.upgrade_delay(), 0);
    c.set_upgrade_delay(&(72 * 3600));
    assert_eq!(c.upgrade_delay(), 72 * 3600);
}

#[test]
fn a_loan_needs_her_attested_food_token_from_its_date_up_to_its_amount_once() {
    let fx = setup();
    let c = fx.core();
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.invest(&fx.neighbour, 1_000 * NIN);
    let (tok, l1, l2) = (
        loan_id(&fx.env, 7),
        loan_id(&fx.env, 1),
        loan_id(&fx.env, 2),
    );
    let draw = |token: &BytesN<32>, _id: &BytesN<32>, cash: i128| {
        c.try_draw_loan(&fx.union, &fx.borrower, token, &cash, &0)
            .unwrap_err()
    };

    assert_eq!(draw(&tok, &l1, NIN), Ok(Error::NoFoodToken));
    // Someone else's token is not hers.
    fx.attest(&fx.neighbour, &tok, 500 * NIN);
    assert_eq!(draw(&tok, &l1, NIN), Ok(Error::NoFoodToken));
    // Attester only.
    assert_eq!(
        c.try_attest_food_token(&fx.leader, &fx.union, &tok, &fx.borrower, &(500 * NIN), &0,)
            .unwrap_err(),
        Ok(Error::NotAttester)
    );
    // Corrected before use; no floor (25 Oct): it can be drawn at once.
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &tok,
        &fx.borrower,
        &(500 * NIN),
        &0,
    );
    fx.at(T0 + 30 * DAY);
    // With the amount gate on, at most the attested amount.
    let mut uc = union_cfg();
    uc.enforce_token_amount = true;
    c.set_union_config(&fx.union, &uc);
    assert_eq!(draw(&tok, &l1, 500 * NIN + 1), Ok(Error::AboveFoodToken));
    // Testing (gate off): the union may give more than attested, within the ₹10,000 cap.
    c.set_union_config(&fx.union, &union_cfg());
    assert_eq!(draw(&tok, &l1, 10_000 * NIN + 1), Ok(Error::CapExceeded));
    c.draw_loan(&fx.union, &fx.borrower, &tok, &(800 * NIN), &0);
    // The loan lives inside the token's entry; the token id is the loan id (C6).
    assert!(c.food_token(&fx.union, &tok).unwrap().loan.drawn());
    assert_eq!(c.get_loan(&fx.union, &tok).unwrap().principal, 800 * NIN);

    // One loan per token, and no re-attesting a used one.
    assert_eq!(draw(&tok, &l2, NIN), Ok(Error::FoodTokenUsed));
    assert_eq!(
        c.try_attest_food_token(
            &fx.attester,
            &fx.union,
            &tok,
            &fx.borrower,
            &(500 * NIN),
            &0,
        )
        .unwrap_err(),
        Ok(Error::FoodTokenUsed)
    );
}

// ---------- C3: the cash desk through the real fx_pool, strict auth ----------

use nila_fx_pool::{Error as FxError, Purpose};
use soroban_sdk::{
    testutils::{AuthorizedFunction, MockAuth, MockAuthInvoke},
    Bytes, IntoVal, Symbol,
};

fn hash(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

impl Fx {
    /// The leader swaps USDC into the union treasury: `nin` at 100 nIN a USDC; the rate in force
    /// before is put back afterwards.
    fn treasury_usdc(&self, nin: i128) -> Result<i128, FxError> {
        let f = self.fx();
        self.env.mock_all_auths();
        let prev = f.rate();
        let now = self.env.ledger().timestamp();
        f.override_rate(&(100 * NIN), &(now + 86_400));
        let usdc = nin / 100;
        StellarAssetClient::new(&self.env, &self.usdc).mint(&self.leader, &usdc);
        let r = f.try_deposit_usdc_treasury(&self.leader, &self.union, &usdc);
        if let Some(p) = prev {
            if p.valid_until > now {
                f.override_rate(&p.inr_per_usd, &p.valid_until);
            }
        }
        match r {
            Ok(Ok(v)) => Ok(v),
            Err(Ok(e)) => Err(e),
            other => panic!("treasury usdc: {other:?}"),
        }
    }

    /// The leader scans rupees in; only the leader signs (fx_pool signs its own pull).
    fn scan(
        &self,
        purpose: Purpose,
        member: &Address,
        amount: i128,
        receipt: u8,
        loan: Option<BytesN<32>>,
    ) -> Result<u64, FxError> {
        // Counted cash never goes into the treasury (22 Oct): the tests' treasury top-ups are the
        // leader's USDC swapped in (deposit_usdc_treasury), at 100 nIN a USDC for exact amounts.
        if purpose == Purpose::Treasury {
            let _ = receipt;
            return self.treasury_usdc(amount).map(|_| 0);
        }
        let args = (
            self.leader.clone(),
            self.union.clone(),
            purpose,
            member.clone(),
            amount,
            hash(&self.env, receipt),
            loan.clone(),
        );
        self.env.mock_auths(&[MockAuth {
            address: &self.leader,
            invoke: &MockAuthInvoke {
                contract: &self.fx_id,
                fn_name: "cash_scan_mint",
                args: args.into_val(&self.env),
                sub_invokes: &[],
            },
        }]);
        let r = self.fx().try_cash_scan_mint(
            &self.leader,
            &self.union,
            &purpose,
            member,
            &amount,
            &hash(&self.env, receipt),
            &loan,
        );
        self.env.mock_all_auths();
        match r {
            Ok(Ok(id)) => Ok(id),
            Err(Ok(e)) => Err(e),
            other => panic!("receipt {receipt}: {other:?}"),
        }
    }

    fn authorized(&self, who: &Address) -> bool {
        StellarAssetClient::new(&self.env, &self.nin).authorized(who)
    }

    /// fx_pool holds only escrowed cash-outs; here none are open.
    fn assert_fx_empty(&self) {
        assert_eq!(self.token().balance(&self.fx_id), 0);
    }
}

/// Cash at hand may pass the treasury (executive decision 24 Oct): what a scan adds above it opens a Collect offer
/// for the union's LPs, in whole rupees, for as long as the cash batch lasts; a scan within the treasury opens none.
/// Merged (25 Oct): while that offer is open and no LP has accepted it, the next part above the treasury is added
/// to it (up to the max scan) instead of a new offer; once an LP accepted it, a new one opens.
#[test]
fn cash_over_the_treasury_opens_a_collect_offer() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 1_500 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.borrower, 1_000 * NIN, 2, None)
        .unwrap();
    assert_eq!(f.offers_of(&fx.union).len(), 0);
    // 700 more: 200 above the treasury.
    fx.scan(Purpose::Invest, &fx.neighbour, 700 * NIN, 3, None)
        .unwrap();
    assert_eq!(f.cash_at_hand(&fx.union), 1_700 * NIN);
    assert_eq!(c.union_treasury(&fx.union), 1_500 * NIN);
    let ids = f.offers_of(&fx.union);
    assert_eq!(ids.len(), 1);
    let o = f.offer(&ids.get(0).unwrap()).unwrap();
    assert_eq!(o.kind, nila_fx_pool::OfferKind::Collect);
    assert_eq!(o.inr, 200 * NIN);
    // Another 100: added to the open offer, no new one.
    fx.scan(Purpose::Invest, &fx.neighbour, 100 * NIN, 4, None)
        .unwrap();
    let ids = f.offers_of(&fx.union);
    assert_eq!(ids.len(), 1);
    assert_eq!(f.offer(&ids.get(0).unwrap()).unwrap().inr, 300 * NIN);
    // Up to the max scan only: a part that would take it past gets an offer of its own.
    f.set_max_scan(&(350 * NIN));
    fx.scan(Purpose::Invest, &fx.neighbour, 100 * NIN, 5, None)
        .unwrap();
    let ids = f.offers_of(&fx.union);
    assert_eq!(ids.len(), 2);
    assert_eq!(f.offer(&ids.get(1).unwrap()).unwrap().inr, 100 * NIN);
    fx.assert_conserved();
}

#[test]
fn cash_scan_invest_draw_repay_with_strict_auth() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    assert_eq!(
        fx.scan(Purpose::Treasury, &fx.leader, 1_500 * NIN, 2, None),
        Ok(0)
    );
    assert_eq!(c.union_treasury(&fx.union), 1_500 * NIN);
    assert_eq!(f.cash_at_hand(&fx.union), 0);

    let e1 = fx
        .scan(Purpose::Invest, &fx.borrower, 1_000 * NIN, 3, None)
        .unwrap();
    assert_eq!(e1, 1);
    assert_eq!(c.position(&fx.union, &fx.borrower).invested, 1_000 * NIN);
    assert_eq!(f.cash_at_hand(&fx.union), 1_000 * NIN);
    fx.assert_fx_empty();
    fx.assert_conserved();

    // The draw's cash part leaves the cash at hand (D9).
    let id = loan_id(&fx.env, 1);
    fx.attest(&fx.borrower, &id, 400 * NIN);
    fx.env.mock_auths(&[MockAuth {
        address: &fx.borrower,
        invoke: &MockAuthInvoke {
            contract: &fx.core_id,
            fn_name: "draw_loan",
            args: (
                fx.union.clone(),
                fx.borrower.clone(),
                id.clone(),
                400 * NIN,
                0u32,
            )
                .into_val(&fx.env),
            sub_invokes: &[],
        },
    }]);
    c.draw_loan(&fx.union, &fx.borrower, &id, &(400 * NIN), &0);
    fx.env.mock_all_auths();
    assert_eq!(f.cash_at_hand(&fx.union), 600 * NIN);
    assert_eq!(f.get_escrow(&fx.union, &e1).unwrap().remaining, 600 * NIN);
    assert_eq!(fx.token().balance(&fx.borrower), 0);

    // Repay in cash, more than owed: only what she owes is minted; the leader gives change.
    fx.at(T0 + YEAR / 2);
    let interest = 400 * NIN * 1250 / 10_000 / 2;
    let e2 = fx
        .scan(Purpose::Repay, &fx.borrower, 500 * NIN, 5, Some(id.clone()))
        .unwrap();
    assert!(c.get_loan(&fx.union, &id).unwrap().closed);
    assert_eq!(
        f.get_escrow(&fx.union, &e2).unwrap().amount,
        400 * NIN + interest
    );
    assert_eq!(f.cash_at_hand(&fx.union), 1_000 * NIN + interest);
    assert_eq!(
        fx.scan(Purpose::Repay, &fx.borrower, NIN, 6, None),
        Err(FxError::NoLoan)
    );
    fx.assert_fx_empty();
    fx.assert_conserved();
}

#[test]
fn scan_caps_and_roles() {
    let fx = setup();
    fx.scan(Purpose::Treasury, &fx.leader, 10_000 * NIN, 1, None)
        .unwrap();
    // ₹10,000 per scan (R76).
    assert_eq!(
        fx.scan(Purpose::Invest, &fx.borrower, 10_000 * NIN + 1, 2, None),
        Err(FxError::CapExceeded)
    );
    assert_eq!(
        fx.scan(Purpose::Invest, &fx.borrower, 0, 3, None),
        Err(FxError::AmountZero)
    );
    // A non-member cannot be invested for: fund_core refuses, nothing is minted.
    let stranger = Address::generate(&fx.env);
    assert!(fx
        .fx()
        .try_cash_scan_mint(
            &fx.leader,
            &fx.union,
            &Purpose::Invest,
            &stranger,
            &NIN,
            &hash(&fx.env, 4),
            &None
        )
        .is_err());
    // Only a leader of the union scans.
    assert_eq!(
        fx.fx().try_cash_scan_mint(
            &fx.borrower,
            &fx.union,
            &Purpose::Invest,
            &fx.borrower,
            &NIN,
            &hash(&fx.env, 5),
            &None
        ),
        Err(Ok(FxError::NotLeader))
    );
    // The admin raises the cap in steps, evented (C15).
    fx.fx().set_max_scan(&(20_000 * NIN));
    assert_eq!(fx.fx().max_scan(), 20_000 * NIN);
    fx.assert_fx_empty();
    fx.assert_conserved();
}

#[test]
fn unsettled_cash_at_hand_burns_treasury_after_7_days() {
    let fx = setup();
    let c = fx.core();
    fx.scan(Purpose::Treasury, &fx.leader, 1_000 * NIN, 1, None)
        .unwrap();
    let e = fx
        .scan(Purpose::Invest, &fx.borrower, 800 * NIN, 2, None)
        .unwrap();
    // The treasury cannot be withdrawn below the cash at hand.
    assert_eq!(
        c.try_withdraw_union_treasury(&fx.leader, &fx.union, &(201 * NIN), &false),
        Err(Ok(Error::CashAtHand))
    );
    assert_eq!(
        fx.fx().try_burn_expired_escrow(&fx.union, &e),
        Err(Ok(FxError::NotExpired))
    );
    fx.at(T0 + 7 * 86_400);
    // Anyone may call it once expired; the union kept the rupees.
    fx.env.set_auths(&[]);
    assert_eq!(fx.fx().burn_expired_escrow(&fx.union, &e), 800 * NIN);
    fx.env.mock_all_auths();
    assert_eq!(c.union_treasury(&fx.union), 200 * NIN);
    assert_eq!(fx.fx().cash_at_hand(&fx.union), 0);
    // Her investment is untouched.
    assert_eq!(c.position(&fx.union, &fx.borrower).invested, 800 * NIN);
    fx.assert_conserved();
}

#[test]
fn trustline_opens_only_inside_the_payout() {
    let fx = setup();
    let c = fx.core();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
    fx.make_leader(&fx.neighbour);
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(400 * NIN));
    fx.at(T0 + 14 * 86_400);
    c.claim_junior(&fx.union, &fx.neighbour, &0);
    assert_eq!(fx.token().balance(&fx.neighbour), 400 * NIN);
    // R10: deauthorised again right after; she cannot send it anywhere herself.
    assert!(!fx.authorized(&fx.neighbour));
    assert!(fx
        .token()
        .try_transfer(&fx.neighbour, &fx.borrower, &NIN)
        .is_err());
    // Only fund_core may ask fx_pool to authorise, and only for a title holder or leader.
    fx.env.set_auths(&[]);
    assert!(fx
        .fx()
        .try_set_member_auth(&fx.union, &fx.neighbour, &true)
        .is_err());
    fx.env.mock_all_auths();
    let stranger = Address::generate(&fx.env);
    assert_eq!(
        fx.fx().try_set_member_auth(&fx.union, &stranger, &true),
        Err(Ok(FxError::NotMember))
    );
    // Treasury to a leader goes the same way.
    c.withdraw_union_treasury(&fx.leader, &fx.union, &(500 * NIN), &false);
    assert_eq!(fx.token().balance(&fx.leader), 500 * NIN);
    assert!(!fx.authorized(&fx.leader));
    fx.assert_conserved();
}

/// The signed cash-out (24 Oct, a machine owner's income): she signs (union, amount) in her QR, any leader
/// of her union submits it once the notes are handed over; her nIN is clawed back, cash at hand goes down.
#[test]
fn cash_out_signed_burns_her_nin_at_the_desk() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
    // Her nIN in her wallet (in the app: a machine owner's prepay and collected dues).
    fx.make_leader(&fx.neighbour);
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(600 * NIN));
    fx.at(T0 + 14 * 86_400);
    c.claim_junior(&fx.union, &fx.neighbour, &0);
    assert_eq!(fx.token().balance(&fx.neighbour), 600 * NIN);

    // Without her signature of (union, amount) a leader cannot burn her nIN.
    fx.env.mock_auths(&[MockAuth {
        address: &fx.leader,
        invoke: &MockAuthInvoke {
            contract: &fx.fx_id,
            fn_name: "cash_out_signed",
            args: (
                fx.leader.clone(),
                fx.union.clone(),
                fx.neighbour.clone(),
                250 * NIN,
            )
                .into_val(&fx.env),
            sub_invokes: &[],
        },
    }]);
    assert!(f
        .try_cash_out_signed(&fx.leader, &fx.union, &fx.neighbour, &(250 * NIN))
        .is_err());
    // Her signature is for one amount: another amount is not authorised.
    fx.env.mock_auths(&[
        MockAuth {
            address: &fx.neighbour,
            invoke: &MockAuthInvoke {
                contract: &fx.fx_id,
                fn_name: "cash_out_signed",
                args: (fx.union.clone(), 250 * NIN).into_val(&fx.env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &fx.leader,
            invoke: &MockAuthInvoke {
                contract: &fx.fx_id,
                fn_name: "cash_out_signed",
                args: (
                    fx.leader.clone(),
                    fx.union.clone(),
                    fx.neighbour.clone(),
                    300 * NIN,
                )
                    .into_val(&fx.env),
                sub_invokes: &[],
            },
        },
    ]);
    assert!(f
        .try_cash_out_signed(&fx.leader, &fx.union, &fx.neighbour, &(300 * NIN))
        .is_err());
    fx.env.mock_all_auths();
    assert_eq!(
        f.try_cash_out_signed(&fx.borrower, &fx.union, &fx.neighbour, &(250 * NIN)),
        Err(Ok(FxError::NotLeader))
    );
    let stranger = Address::generate(&fx.env);
    assert_eq!(
        f.try_cash_out_signed(&fx.leader, &fx.union, &stranger, &NIN),
        Err(Ok(FxError::NotMember))
    );
    assert_eq!(
        f.try_cash_out_signed(&fx.leader, &fx.union, &fx.neighbour, &(10_000 * NIN + 1)),
        Err(Ok(FxError::CapExceeded))
    );

    let hand = f.cash_at_hand(&fx.union);
    assert_eq!(
        f.cash_out_signed(&fx.leader, &fx.union, &fx.neighbour, &(250 * NIN)),
        250 * NIN
    );
    let auths = fx.env.auths();
    assert!(auths.iter().any(|a| a.0 == fx.neighbour
        && matches!(&a.1.function, AuthorizedFunction::Contract((_, f, args))
            if *f == Symbol::new(&fx.env, "cash_out_signed") && args.len() == 2)));
    assert_eq!(fx.token().balance(&fx.neighbour), 350 * NIN);
    assert_eq!(f.cash_at_hand(&fx.union), hand - 250 * NIN);
    fx.assert_fx_empty();
}

#[test]
fn cash_out_pay_out_and_reclaim() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
    fx.make_leader(&fx.neighbour);
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(600 * NIN));
    fx.at(T0 + 14 * 86_400);
    c.claim_junior(&fx.union, &fx.neighbour, &0);
    let now = T0 + 14 * 86_400;

    // A7: her nIN into fx_pool under the code's hash; only she signs.
    let code = Bytes::from_array(&fx.env, b"meena-code-1");
    let code_hash: BytesN<32> = fx.env.crypto().sha256(&code).into();
    let expires = now + 86_400;
    fx.env.mock_auths(&[MockAuth {
        address: &fx.neighbour,
        invoke: &MockAuthInvoke {
            contract: &fx.fx_id,
            fn_name: "request_cash_out",
            args: (
                fx.neighbour.clone(),
                fx.union.clone(),
                250 * NIN,
                code_hash.clone(),
                expires,
            )
                .into_val(&fx.env),
            sub_invokes: &[MockAuthInvoke {
                contract: &fx.nin,
                fn_name: "transfer",
                args: (fx.neighbour.clone(), fx.fx_id.clone(), 250 * NIN).into_val(&fx.env),
                sub_invokes: &[],
            }],
        },
    }]);
    f.request_cash_out(&fx.neighbour, &fx.union, &(250 * NIN), &code_hash, &expires);
    fx.env.mock_all_auths();
    assert!(!fx.authorized(&fx.neighbour));
    assert_eq!(fx.token().balance(&fx.fx_id), 250 * NIN);
    assert_eq!(fx.token().balance(&fx.neighbour), 350 * NIN);

    // The leader scans the code and counts out the rupees: burned, cash at hand down.
    let hand = f.cash_at_hand(&fx.union);
    assert_eq!(
        f.try_pay_out(&fx.borrower, &code),
        Err(Ok(FxError::NotLeader))
    );
    assert_eq!(f.pay_out(&fx.leader, &code), 250 * NIN);
    assert_eq!(f.cash_at_hand(&fx.union), hand - 250 * NIN);
    assert_eq!(f.try_pay_out(&fx.leader, &code), Err(Ok(FxError::NotFound)));
    fx.assert_fx_empty();

    // Caps: ₹10,000, at most 7 days, a code is used once while open.
    let h2 = hash(&fx.env, 9);
    assert_eq!(
        f.try_request_cash_out(&fx.neighbour, &fx.union, &(10_000 * NIN + 1), &h2, &expires),
        Err(Ok(FxError::CapExceeded))
    );
    assert_eq!(
        f.try_request_cash_out(&fx.neighbour, &fx.union, &NIN, &h2, &(now + 8 * 86_400)),
        Err(Ok(FxError::BadExpiry))
    );
    let stranger = Address::generate(&fx.env);
    assert_eq!(
        f.try_request_cash_out(&stranger, &fx.union, &NIN, &h2, &expires),
        Err(Ok(FxError::NotMember))
    );

    // Never collected: after expiry anyone returns it to her wallet, deauthorised again.
    f.request_cash_out(&fx.neighbour, &fx.union, &(100 * NIN), &h2, &expires);
    assert_eq!(
        f.try_request_cash_out(&fx.neighbour, &fx.union, &NIN, &h2, &expires),
        Err(Ok(FxError::CodeUsed))
    );
    assert_eq!(f.try_reclaim_cash_out(&h2), Err(Ok(FxError::NotExpired)));
    fx.at(expires);
    let code2 = Bytes::from_array(&fx.env, &[9; 32]);
    assert_eq!(
        f.try_pay_out(&fx.leader, &code2),
        Err(Ok(FxError::NotFound))
    );
    fx.env.set_auths(&[]);
    assert_eq!(f.reclaim_cash_out(&h2), 100 * NIN);
    fx.env.mock_all_auths();
    assert_eq!(fx.token().balance(&fx.neighbour), 350 * NIN);
    assert!(!fx.authorized(&fx.neighbour));
    fx.assert_fx_empty();
    fx.assert_conserved();
}

#[test]
fn fx_pool_slot_set_once_and_codes_used_once() {
    let fx = setup();
    let c = fx.core();
    assert_eq!(
        c.try_set_fx_pool(&Address::generate(&fx.env)),
        Err(Ok(Error::FxPoolAlreadySet))
    );
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
    fx.make_leader(&fx.neighbour);
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(400 * NIN));
    fx.at(T0 + 14 * 86_400);
    c.claim_junior(&fx.union, &fx.neighbour, &0);
    let code = Bytes::from_array(&fx.env, b"one-time");
    let h: BytesN<32> = fx.env.crypto().sha256(&code).into();
    let exp = T0 + 15 * 86_400;
    f.request_cash_out(&fx.neighbour, &fx.union, &(100 * NIN), &h, &exp);
    f.pay_out(&fx.leader, &code);
    // R16: the code is closed; its preimage is public now, so it is never accepted again.
    assert_eq!(
        f.try_request_cash_out(&fx.neighbour, &fx.union, &(100 * NIN), &h, &exp),
        Err(Ok(FxError::CodeUsed))
    );
    // While fund_core is paused no expiry burns the treasury.
    let e = fx
        .scan(Purpose::Invest, &fx.borrower, 100 * NIN, 3, None)
        .unwrap();
    fx.at(T0 + 30 * 86_400);
    c.pause();
    assert!(f.try_burn_expired_escrow(&fx.union, &e).is_err());
    c.unpause();
    f.burn_expired_escrow(&fx.union, &e);
    fx.assert_conserved();
}

// ---------- C4: storage layout guard ----------

/// sha256 over the XDR of one sample of every stored key and value. If this changes, data a
/// deployed fund_core holds no longer decodes after an upgrade: write a migration, or leave
/// the stored shape alone (docs/STORAGE_LAYOUT.md).
#[test]
fn storage_layout_is_frozen() {
    use soroban_sdk::{xdr::ToXdr, Val};
    let env = Env::default();
    let a = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let h = BytesN::from_array(&env, &[7; 32]);
    let mut v: soroban_sdk::Vec<Val> = soroban_sdk::Vec::new(&env);
    for k in [
        DataKey::Config,
        DataKey::Paused,
        DataKey::UnionCfg(a.clone()),
        DataKey::SeniorMarket(a.clone()),
        DataKey::JuniorMarket(a.clone()),
        DataKey::SeniorInv(a.clone(), a.clone()),
        DataKey::JuniorInv(a.clone(), a.clone()),
        DataKey::Milestone(a.clone(), h.clone()),
        DataKey::LoansOf(a.clone(), a.clone()),
        DataKey::Union(a.clone()),
        DataKey::FoodToken(a.clone(), h.clone()),
        DataKey::Vouchers(a.clone(), h.clone()),
        DataKey::OwnerBalance(a.clone(), a.clone()),
        DataKey::FoodTokenFinal(a.clone(), h.clone()),
        DataKey::CropCalendar,
    ] {
        v.push_back(k.into_val(&env));
    }
    // Appended 25 Oct: the last draw date and lapse of a food token, beside its unchanged entry.
    v.push_back(
        FoodTokenFinal {
            final_date: 1,
            lapsed: false,
        }
        .into_val(&env),
    );
    v.push_back(
        Config {
            admin: a.clone(),
            roles: a.clone(),
            land_title: a.clone(),
            nin: a.clone(),
            fx_pool: Some(a.clone()),
            machinery: Some(a.clone()),
        }
        .into_val(&env),
    );
    v.push_back(union_cfg().into_val(&env));
    v.push_back(
        Market {
            cash: 1,
            index: 2,
            total_shares: 3,
            total_borrows: 4,
            claimable_principal: 5,
            exists: true,
        }
        .into_val(&env),
    );
    v.push_back(
        Investor {
            shares: 1,
            pending: 2,
            claimable: 3,
            request_ts: 4,
            cost: 5,
            pending_principal_snap: 6,
        }
        .into_val(&env),
    );
    v.push_back(
        types::Milestone {
            milestone: 9,
            digest: h.clone(),
            ts: 10,
        }
        .into_val(&env),
    );
    v.push_back(
        FoodToken {
            member: a.clone(),
            amount: 1,
            hours: 3,
            from_date: 2,
            loan: types::LoanState {
                principal: 1,
                rate_bp: 2,
                drawdown_ts: 3,
                maturity_ts: 4,
                principal_paid: 5,
                interest_paid: 6,
                interest_accrued: 7,
                last_accrual_ts: 8,
                flags: 15,
            },
        }
        .into_val(&env),
    );
    v.push_back(nila_common::upgrade::UpgradeKey::UpgDelay.into_val(&env));
    v.push_back(nila_common::upgrade::UpgradeKey::UpgPending.into_val(&env));
    v.push_back(nila_common::upgrade::UpgradeKey::UpgPendingDelay.into_val(&env));
    v.push_back(
        nila_common::upgrade::PendingUpgrade {
            wasm_hash: h.clone(),
            eta: 1,
        }
        .into_val(&env),
    );
    v.push_back(nila_common::upgrade::PendingDelay { delay: 1, eta: 2 }.into_val(&env));
    v.push_back(
        VoucherBook {
            hours: 1,
            per_hour: 2,
            value: 3,
            repaid: 4,
            interest_accrued: 5,
            interest_paid: 6,
            last_accrual_ts: 7,
            reserve: 8,
            fee_due: 9,
        }
        .into_val(&env),
    );
    v.push_back(
        types::UnionBook {
            borrows: 1,
            treasury: 2,
            rainy: 3,
            earmark: 4,
            on_hold: 5,
        }
        .into_val(&env),
    );
    v.push_back(nila_common::Tranche::Junior.into_val(&env));
    v.push_back(nila_common::Tranche::Senior.into_val(&env));
    let got: BytesN<32> = env.crypto().sha256(&v.to_xdr(&env)).into();
    let hex: std::string::String = got
        .to_array()
        .iter()
        .map(|b| std::format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex, "4e78348ff1d40aff87d6a38344bb18cf3ba661585e63a90dde1697035b413440",
        "fund_core storage layout changed"
    );
}

#[test]
fn the_rate_never_exceeds_the_cap_in_code() {
    let fx = setup();
    let c = fx.core();
    let mut uc = union_cfg();
    uc.rate_bp = MAX_RATE_BP + 1;
    assert_eq!(
        c.try_set_union_config(&fx.union, &uc),
        Err(Ok(Error::AboveMaxRate))
    );
    uc.rate_bp = MAX_RATE_BP;
    c.set_union_config(&fx.union, &uc);
    // A discount below the cap is fine.
    uc.rate_bp = MAX_RATE_BP - 300;
    c.set_union_config(&fx.union, &uc);
    assert_eq!(MAX_RATE_BP, 1250);
}

// ---------- C5a: senior route (a senior investor's USDC in, nIN into the senior pool) ----------

use nila_fx_pool::Tag;

#[test]
fn senior_route_usdc_in_nin_into_the_senior_pool() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    let investor = Address::generate(&fx.env);
    let usdc = StellarAssetClient::new(&fx.env, &fx.usdc);
    usdc.mint(&investor, &(1_000 * NIN));
    let kyc = loan_id(&fx.env, 0x4B);

    // Not accepted yet; no rate yet.
    assert_eq!(
        f.try_mint_senior(&investor, &fx.union, &(100 * NIN), &Tag::Offshore),
        Err(Ok(FxError::Unauthorized))
    );
    f.set_senior_investor(&investor, &true, &kyc);
    assert_eq!(
        f.try_mint_senior(&investor, &fx.union, &(100 * NIN), &Tag::Offshore),
        Err(Ok(FxError::StaleOracle))
    );
    // The trust sets the first fix; the attester then follows the market within 5%, valid at
    // most 2 days. A bigger move is the trust's again.
    let rate = 95 * NIN; // ₹95 per USD
    assert_eq!(
        f.try_set_rate(&fx.attester, &rate, &(T0 + 86_400)),
        Err(Ok(FxError::BadRate))
    );
    f.override_rate(&(94 * NIN), &(T0 + 86_400));
    assert_eq!(
        f.try_set_rate(&fx.oracle, &rate, &(T0 + 86_400)),
        Err(Ok(FxError::Unauthorized))
    );
    assert_eq!(
        f.try_set_rate(&fx.attester, &rate, &(T0 + 3 * 86_400)),
        Err(Ok(FxError::BadRate))
    );
    assert_eq!(
        f.try_set_rate(&fx.attester, &(50 * NIN), &(T0 + 86_400)),
        Err(Ok(FxError::BadRate))
    );
    f.set_rate(&fx.attester, &rate, &(T0 + 86_400));

    // Only the investor signs: her USDC transfer, fx_pool's own pull into fund_core.
    let args = (investor.clone(), fx.union.clone(), 100 * NIN, Tag::Offshore);
    fx.env.mock_auths(&[MockAuth {
        address: &investor,
        invoke: &MockAuthInvoke {
            contract: &fx.fx_id,
            fn_name: "mint_senior",
            args: args.into_val(&fx.env),
            sub_invokes: &[MockAuthInvoke {
                contract: &fx.usdc,
                fn_name: "transfer",
                args: (investor.clone(), fx.fx_id.clone(), 100 * NIN).into_val(&fx.env),
                sub_invokes: &[],
            }],
        },
    }]);
    assert_eq!(
        f.mint_senior(&investor, &fx.union, &(100 * NIN), &Tag::Offshore),
        9_500 * NIN
    );
    fx.env.mock_all_auths();
    assert_eq!(c.get_senior_market(&fx.union).cash, 9_500 * NIN);
    assert_eq!(
        TokenClient::new(&fx.env, &fx.usdc).balance(&fx.fx_id),
        100 * NIN
    );
    assert_eq!(f.senior_usdc(&fx.union, &Tag::Offshore), 100 * NIN);
    assert_eq!(f.senior_usdc(&fx.union, &Tag::Domestic), 0);
    // The senior investor never holds nIN.
    assert_eq!(fx.token().balance(&investor), 0);
    fx.assert_fx_empty();
    fx.assert_conserved();

    // A member cannot be a senior investor: fund_core refuses title holders, nothing moves.
    f.set_senior_investor(&fx.borrower, &true, &kyc);
    usdc.mint(&fx.borrower, &(10 * NIN));
    assert!(f
        .try_mint_senior(&fx.borrower, &fx.union, &(10 * NIN), &Tag::Domestic)
        .is_err());
    // A dropped investor, a stale fix, a paused desk.
    f.set_senior_investor(&investor, &false, &kyc);
    assert_eq!(
        f.try_mint_senior(&investor, &fx.union, &NIN, &Tag::Domestic),
        Err(Ok(FxError::Unauthorized))
    );
    f.set_senior_investor(&investor, &true, &kyc);
    fx.at(T0 + 86_400 + 1);
    assert_eq!(
        f.try_mint_senior(&investor, &fx.union, &NIN, &Tag::Domestic),
        Err(Ok(FxError::StaleOracle))
    );
    f.set_rate(&fx.attester, &rate, &(T0 + 2 * 86_400));
    f.pause();
    assert_eq!(
        f.try_mint_senior(&investor, &fx.union, &NIN, &Tag::Domestic),
        Err(Ok(FxError::Paused))
    );
}

// ---------- C5b: LP offers (member LPs swap cash and USDC; they never lend) ----------

use nila_fx_pool::{OfferKind, OfferState};
use soroban_sdk::String as SString;

impl Fx {
    /// Anchor accepts the neighbour as LP; ₹95 fix; the union has cash at hand.
    fn lp_ready(&self) -> (Address, Address) {
        let f = self.fx();
        let anchor = Address::generate(&self.env);
        f.set_anchor(&anchor);
        f.set_id_checked(&self.neighbour, &true, &loan_id(&self.env, 0x4B));
        f.set_lp(
            &self.union,
            &self.neighbour,
            &true,
            &loan_id(&self.env, 0x4C),
        );
        f.override_rate(&(95 * NIN), &(T0 + 86_400));
        self.scan(Purpose::Treasury, &self.leader, 5_000 * NIN, 1, None)
            .unwrap();
        self.scan(Purpose::Invest, &self.borrower, 2_000 * NIN, 2, None)
            .unwrap();
        (anchor, self.neighbour.clone())
    }

    fn ids(&self) -> (SString, SString) {
        (
            SString::from_str(&self.env, "sep24-tx-1"),
            SString::from_str(&self.env, "sep38-q-1"),
        )
    }
}

/// The anchor's swap key in the tests (C9b): signs each accept like nila-anchor does.
fn swap_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[9u8; 32])
}

type TryAcc = Result<Result<i128, soroban_sdk::Error>, Result<FxError, soroban_sdk::InvokeError>>;

impl Fx {
    /// The anchor's signature for an accept at today's quote (0 USDC when there is no quote).
    fn signed(&self, lp: &Address, id: u64, s24: &SString, q: &SString) -> (i128, u64, BytesN<64>) {
        use ed25519_dalek::Signer;
        use soroban_sdk::xdr::ToXdr;
        let f = self.fx();
        let o = f.offer(&id).unwrap();
        let usdc = f
            .try_quote(&o.union, &o.kind, &o.inr)
            .ok()
            .and_then(|r| r.ok())
            .map(|q| q.usdc)
            .unwrap_or(0);
        let until = self.env.ledger().timestamp() + 600;
        let mut msg = soroban_sdk::Bytes::from_slice(&self.env, nila_fx_pool::ACCEPT_PREFIX);
        msg.append(
            &(
                self.fx_id.clone(),
                id,
                lp.clone(),
                s24.clone(),
                q.clone(),
                usdc,
                until,
            )
                .to_xdr(&self.env),
        );
        let raw: std::vec::Vec<u8> = msg.iter().collect();
        let sig = swap_key().sign(&raw).to_bytes();
        (usdc, until, BytesN::from_array(&self.env, &sig))
    }

    fn acc(&self, lp: &Address, id: &u64, s24: &SString, q: &SString) -> i128 {
        let (usdc, until, sig) = self.signed(lp, *id, s24, q);
        self.fx().accept(lp, id, s24, q, &usdc, &until, &sig)
    }

    fn try_acc(&self, lp: &Address, id: &u64, s24: &SString, q: &SString) -> TryAcc {
        let (usdc, until, sig) = self.signed(lp, *id, s24, q);
        self.fx().try_accept(lp, id, s24, q, &usdc, &until, &sig)
    }
}

#[test]
fn collect_then_bring_through_an_lp() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    let usdc = TokenClient::new(&fx.env, &fx.usdc);
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    let (s24, q) = fx.ids();

    // C4: ₹950 spare cash; she pays 10 USDC less her 1% fee and takes the rupees.
    let c = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Collect,
        &(950 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(fx.acc(&lp, &c, &s24, &q), 99_000_000);
    let hand = f.cash_at_hand(&fx.union);
    assert_eq!(f.confirm(&fx.leader, &c), 99_000_000);
    assert_eq!(usdc.balance(&lp), 50 * NIN - 99_000_000);
    assert_eq!(f.lp_usdc(&fx.union), 99_000_000);
    assert_eq!(f.cash_at_hand(&fx.union), hand - 950 * NIN);
    assert_eq!(f.offer(&c).unwrap().state, OfferState::Done);

    // L11 #short: ₹475 needed; she brings it and gets 5 USDC plus 1%, from LP-collected USDC.
    let b = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(475 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(fx.acc(&lp, &b, &s24, &q), 50_500_000);
    assert_eq!(f.lp_usdc(&fx.union), 99_000_000 - 50_500_000);
    f.confirm(&fx.leader, &b);
    assert_eq!(usdc.balance(&lp), 50 * NIN - 99_000_000 + 50_500_000);
    // fx_pool also holds the treasury's USDC backing (the fixture's treasury top-up).
    assert_eq!(
        usdc.balance(&fx.fx_id) - f.treasury_usdc(&fx.union),
        99_000_000 - 50_500_000
    );
    assert_eq!(f.offers_of(&fx.union).len(), 0);
    // The LP never holds nIN; nIN books are untouched by swaps.
    assert_eq!(fx.token().balance(&lp), 0);
    fx.assert_fx_empty();
    fx.assert_conserved();
}

#[test]
fn quote_matches_what_accept_fixes() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    let (s24, q) = fx.ids();
    assert_eq!(f.lp_fee(), 100);

    // C9: SEP-38 reads the same numbers accept fixes on the offer.
    let qc = f.quote(&fx.union, &OfferKind::Collect, &(950 * NIN));
    assert_eq!((qc.usdc, qc.fee, qc.fee_bp), (99_000_000, 1_000_000, 100));
    assert_eq!((qc.inr_per_usd, qc.valid_until), (95 * NIN, T0 + 86_400));
    let c = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Collect,
        &(950 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(fx.acc(&lp, &c, &s24, &q), qc.usdc);
    // Nothing collected yet and no senior USDC: a Bring has no USDC to pay from.
    assert_eq!(qc.usdc_free, 0);
    assert_eq!(qc.day_left, f.daily_cap());
    f.confirm(&fx.leader, &c);
    let qb = f.quote(&fx.union, &OfferKind::Bring, &(475 * NIN));
    assert_eq!((qb.usdc, qb.fee), (50_500_000, 500_000));
    assert_eq!(qb.usdc_free, 99_000_000);

    // The fee follows set_lp_fee; amounts outside the scan cap are refused like post_offer.
    f.set_lp_fee(&200);
    assert_eq!(f.lp_fee(), 200);
    assert_eq!(
        f.quote(&fx.union, &OfferKind::Bring, &(475 * NIN)).usdc,
        51_000_000
    );
    assert_eq!(
        f.try_quote(&fx.union, &OfferKind::Bring, &0),
        Err(Ok(FxError::AmountZero))
    );
    assert_eq!(
        f.try_quote(&fx.union, &OfferKind::Bring, &(10_001 * NIN)),
        Err(Ok(FxError::CapExceeded))
    );

    // After the fix expires there is no quote, and no accept.
    fx.at(T0 + 86_401);
    assert_eq!(
        f.try_quote(&fx.union, &OfferKind::Collect, &(950 * NIN)),
        Err(Ok(FxError::StaleOracle))
    );
}

#[test]
fn bring_draws_senior_usdc_only_above_the_voucher_floor() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    let (s24, q) = fx.ids();
    // A senior investor's 100 USDC, offshore; 90 of it kept back for vouchers.
    let investor = Address::generate(&fx.env);
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&investor, &(100 * NIN));
    f.set_senior_investor(&investor, &true, &loan_id(&fx.env, 0x49));
    f.mint_senior(&investor, &fx.union, &(100 * NIN), &Tag::Offshore);
    f.set_voucher_floor(&fx.union, &(90 * NIN));

    // ₹1,900 → 20.2 USDC: would cut into the floor.
    let big = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(1_900 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(
        fx.try_acc(&lp, &big, &s24, &q),
        Err(Ok(FxError::BelowVoucherFloor))
    );
    // Withdrawn, so the next amount is an offer of its own (an open one would take it, 25 Oct).
    f.cancel_offer(&fx.leader, &big);
    // Only 10 USDC are free above the floor: ₹950 (10.1 USDC with the fee) is refused, ₹900 fits.
    let mid = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(950 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(
        fx.try_acc(&lp, &mid, &s24, &q),
        Err(Ok(FxError::BelowVoucherFloor))
    );
    f.cancel_offer(&fx.leader, &mid);
    let ok = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(900 * NIN),
        &(T0 + 3_600),
    );
    let paid = fx.acc(&lp, &ok, &s24, &q);
    let o = f.offer(&ok).unwrap();
    assert_eq!((o.from_lp, o.from_domestic, o.from_offshore), (0, 0, paid));
    assert_eq!(f.senior_usdc(&fx.union, &Tag::Offshore), 100 * NIN - paid);
    // Cancelling hands the reservation back; expiry does the same.
    f.cancel_offer(&fx.leader, &ok);
    assert_eq!(f.senior_usdc(&fx.union, &Tag::Offshore), 100 * NIN);
    let again = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(900 * NIN),
        &(T0 + 3_600),
    );
    fx.acc(&lp, &again, &s24, &q);
    assert_eq!(f.try_expire_offer(&again), Err(Ok(FxError::NotExpired)));
    fx.at(T0 + 3_600);
    assert_eq!(
        f.try_confirm(&fx.leader, &again),
        Err(Ok(FxError::OfferState))
    );
    fx.env.set_auths(&[]);
    f.expire_offer(&again);
    fx.env.mock_all_auths();
    assert_eq!(f.senior_usdc(&fx.union, &Tag::Offshore), 100 * NIN);
    assert_eq!(f.offer(&again).unwrap().state, OfferState::Expired);
}

#[test]
fn only_member_lps_accepted_by_the_anchor() {
    let fx = setup();
    let f = fx.fx();
    let (anchor, lp) = fx.lp_ready();
    let (s24, q) = fx.ids();
    let stranger = Address::generate(&fx.env);
    // K1 (22 Oct): the anchor checked her identity first.
    assert_eq!(
        f.try_set_lp(&fx.union, &stranger, &true, &loan_id(&fx.env, 1)),
        Err(Ok(FxError::NotIdChecked))
    );
    f.set_id_checked(&stranger, &true, &loan_id(&fx.env, 2));
    // R20: a member with a title the union minted.
    assert_eq!(
        f.try_set_lp(&fx.union, &stranger, &true, &loan_id(&fx.env, 1)),
        Err(Ok(FxError::NotMember))
    );
    // Only the anchor accepts LPs.
    f.set_id_checked(&fx.borrower, &true, &loan_id(&fx.env, 3));
    fx.env.set_auths(&[]);
    assert!(f
        .try_set_lp(&fx.union, &fx.borrower, &true, &loan_id(&fx.env, 1))
        .is_err());
    fx.env.mock_all_auths();
    let _ = anchor;
    let c = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Collect,
        &(95 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(
        fx.try_acc(&fx.borrower, &c, &s24, &q),
        Err(Ok(FxError::NotLp))
    );
    // Only a leader posts and confirms; offers are capped at ₹10,000 and 7 days.
    assert_eq!(
        f.try_post_offer(&lp, &fx.union, &OfferKind::Bring, &NIN, &(T0 + 3_600)),
        Err(Ok(FxError::NotLeader))
    );
    assert_eq!(
        f.try_post_offer(
            &fx.leader,
            &fx.union,
            &OfferKind::Bring,
            &(10_001 * NIN),
            &(T0 + 3_600)
        ),
        Err(Ok(FxError::CapExceeded))
    );
    assert_eq!(
        f.try_post_offer(
            &fx.leader,
            &fx.union,
            &OfferKind::Bring,
            &NIN,
            &(T0 + 8 * 86_400)
        ),
        Err(Ok(FxError::BadExpiry))
    );
    // A Bring with no USDC in the union.
    let b = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(95 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(fx.try_acc(&lp, &b, &s24, &q), Err(Ok(FxError::NoUsdc)));
    // A dropped LP cannot accept.
    f.set_lp(&fx.union, &lp, &false, &loan_id(&fx.env, 1));
    assert_eq!(fx.try_acc(&lp, &c, &s24, &q), Err(Ok(FxError::NotLp)));
}

#[test]
fn collect_accept_needs_only_the_lp_signature() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    let (s24, q) = fx.ids();
    let c = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Collect,
        &(950 * NIN),
        &(T0 + 3_600),
    );
    let expiry_ledger = fx.env.ledger().sequence() + (3_600 / 5 + 1) as u32;
    let (usdc, until, sig) = fx.signed(&lp, c, &s24, &q);
    fx.env.mock_auths(&[MockAuth {
        address: &lp,
        invoke: &MockAuthInvoke {
            contract: &fx.fx_id,
            fn_name: "accept",
            args: (
                lp.clone(),
                c,
                s24.clone(),
                q.clone(),
                usdc,
                until,
                sig.clone(),
            )
                .into_val(&fx.env),
            sub_invokes: &[MockAuthInvoke {
                contract: &fx.usdc,
                fn_name: "approve",
                args: (lp.clone(), fx.fx_id.clone(), 99_000_000i128, expiry_ledger)
                    .into_val(&fx.env),
                sub_invokes: &[],
            }],
        },
    }]);
    f.accept(&lp, &c, &s24, &q, &usdc, &until, &sig);
    fx.env.mock_all_auths();
    assert_eq!(
        TokenClient::new(&fx.env, &fx.usdc).allowance(&lp, &fx.fx_id),
        99_000_000
    );
}

// ---------- C5: machinery vouchers ----------

use nila_machinery::{MachineryContract, MachineryContractClient, RateParts};

const PRE: i128 = 700 * NIN; // diesel 500 + half the driver part 400

/// A tractor hour: ₹1,450 in parts, scaled by `k` (NIN = as set, 15_000_000 = +50%).
fn tractor(k: i128) -> RateParts {
    RateParts {
        driver: 400 * k,
        diesel: 500 * k,
        maintenance: 200 * k,
        insurance: 100 * k,
        depreciation: 250 * k,
        margin: 0,
    }
}
const PEND: i128 = 750 * NIN; // the other half of driver, maintenance, insurance, depreciation
const HOUR: i128 = PRE + PEND; // a tractor hour's whole value: what a voucher hour is lent at (26 Oct)

struct Vx {
    fx: Fx,
    m: Address,
    owner: Address,
    machine: u64,
}

impl Vx {
    fn mach(&self) -> MachineryContractClient<'_> {
        MachineryContractClient::new(&self.fx.env, &self.m)
    }

    /// fund_core nIN = pools + treasury + rainy + the voucher reserve + the owner's balance.
    fn assert_conserved(&self) {
        let c = self.fx.core();
        let u = &self.fx.union;
        let books = c.get_junior_market(u).cash
            + c.get_senior_market(u).cash
            + c.union_treasury(u)
            + c.union_rainy_day(u)
            + c.senior_earmark(u)
            + c.owner_balance(u, &self.owner);
        assert_eq!(self.fx.token().balance(&self.fx.core_id), books);
    }
}

/// Machinery wired in, a ₹1,450/h tractor rate, a confirmed machine owned by the neighbour,
/// 10,000 nIN of senior money, and the borrower invested past the gate.
fn vsetup() -> Vx {
    let fx = setup();
    let env = &fx.env;
    let roles = fx.core().get_config().roles;
    let admin = fx.core().get_config().admin;
    let m = env.register(MachineryContract, (admin, roles, fx.titles_id.clone()));
    let mc = MachineryContractClient::new(env, &m);
    mc.set_fund_core(&fx.core_id);
    fx.core().set_machinery(&m);
    // The union account (its leaders) sets the price of a tractor hour in parts (R57).
    mc.set_rate(&fx.union, &1, &tractor(NIN));
    let owner = fx.neighbour.clone();
    let machine = mc.register_machine(&fx.leader, &fx.union, &owner, &1, &40, &loan_id(env, 0x50));
    mc.confirm_machine(&owner, &machine);
    // Senior money for the prepay (N4).
    let f = fx.fx();
    let investor = Address::generate(env);
    StellarAssetClient::new(env, &fx.usdc).mint(&investor, &(200 * NIN));
    f.set_senior_investor(&investor, &true, &loan_id(env, 0x49));
    f.override_rate(&(50 * NIN), &(T0 + 86_400));
    f.mint_senior(
        &investor,
        &fx.union,
        &(200 * NIN),
        &nila_fx_pool::Tag::Offshore,
    );
    fx.invest(&fx.borrower, 2_000 * NIN);
    Vx {
        fx,
        m,
        owner,
        machine,
    }
}

/// Executive decision 26 Oct: the voucher's value is debt from the draw, with interest that senior
/// earns; owners are paid in full at the claim; repaying never touches the hours, which stay hers.
#[test]
fn voucher_is_debt_from_the_draw_and_its_hours_stay_hers() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(400 * NIN),
        &600,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(400 * NIN), &600);
    // 6 tractor hours lent by senior at the draw: out of senior cash into the reserve, owed by her.
    let value = 6 * HOUR;
    assert_eq!(c.senior_earmark(&fx.union), value);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!((sm.cash, sm.total_borrows), (10_000 * NIN - value, value));
    assert_eq!(mc.voucher(&fx.union, &id).unwrap().hours, 600);
    // Her lock counts the voucher's value too (R27): she owes more than her 2,000.
    let p = c.position(&fx.union, &fx.borrower);
    assert_eq!(p.locked, p.invested);
    v.assert_conserved();

    // A8a: she signs for up to 6 hours; the owner works 4 and is paid all of it at once.
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &600, &400);
    use soroban_sdk::{testutils::AuthorizedFunction, IntoVal, Symbol};
    let signed: soroban_sdk::Vec<soroban_sdk::Val> =
        (fx.union.clone(), id.clone(), 600u32).into_val(&fx.env);
    assert!(fx.env.auths().iter().any(|(a, inv)| *a == fx.borrower
        && matches!(&inv.function, AuthorizedFunction::Contract((c, f, args))
            if *c == mc.address && *f == Symbol::new(&fx.env, "claim") && *args == signed)));
    assert_eq!(fx.token().balance(&v.owner), 4 * HOUR);
    assert_eq!(c.senior_earmark(&fx.union), 2 * HOUR);
    // The claim changes nothing in what she owes or what senior lent.
    assert_eq!(c.get_senior_market(&fx.union).total_borrows, value);
    v.assert_conserved();

    // Half a year on she repays everything: the voucher's interest and value to senior, then
    // junior's interest and principal. Interest runs on the whole (₹400 cash and ₹8,700 vouchers).
    fx.at(T0 + YEAR / 2);
    let j_int = 400 * NIN * 1250 / 10_000 / 2;
    let v_int = value * 1250 / 10_000 / 2;
    let total = v_int + value + j_int + 400 * NIN;
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(total + 50 * NIN));
    assert!(c.repay_loan(&fx.union, &id, &(total + 50 * NIN)));
    let sm = c.get_senior_market(&fx.union);
    assert_eq!((sm.cash, sm.total_borrows), (10_000 * NIN + v_int, 0));
    // Closed, and her 2 unused hours stay hers, with their reserve.
    assert_eq!(c.senior_earmark(&fx.union), 2 * HOUR);
    assert!(!mc.voucher(&fx.union, &id).unwrap().void);
    v.assert_conserved();
    // She uses them after the loan is repaid: the owner is paid from the reserve.
    fx.at(T0 + YEAR / 2 + 8 * 86_400);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &200, &200);
    assert_eq!(fx.token().balance(&v.owner), 6 * HOUR);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    v.assert_conserved();
}

#[test]
fn claims_stay_inside_signed_hours_voucher_week_and_availability() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &600);
    // A voucher-only loan: no cash part, but the voucher's value is owed (26 Oct).
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &600);
    use nila_machinery::Error as ME;
    // The owner alone cannot claim: her own authorisation is required.
    {
        use soroban_sdk::{
            testutils::{MockAuth, MockAuthInvoke},
            IntoVal,
        };
        fx.env.mock_auths(&[MockAuth {
            address: &v.owner,
            invoke: &MockAuthInvoke {
                contract: &mc.address,
                fn_name: "claim",
                args: (&v.owner, v.machine, &fx.union, &id, 10u32, 1u32).into_val(&fx.env),
                sub_invokes: &[],
            },
        }]);
        assert!(mc
            .try_claim(&v.owner, &v.machine, &fx.union, &id, &1000, &100)
            .is_err());
        fx.env.mock_all_auths();
    }
    // Not her machine; more than she signed for; more than her voucher; more than a machine's
    // hours a week.
    assert_eq!(
        mc.try_claim(&fx.borrower, &v.machine, &fx.union, &id, &1000, &100),
        Err(Ok(ME::NotOwner))
    );
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &1000, &1100),
        Err(Ok(ME::TooManyHours))
    );
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &1500, &1500),
        Err(Ok(ME::TooManyHours))
    );
    let small = mc.register_machine(&fx.leader, &fx.union, &v.owner, &1, &3, &id);
    mc.confirm_machine(&v.owner, &small);
    assert_eq!(
        mc.try_claim(&v.owner, &small, &fx.union, &id, &1000, &400),
        Err(Ok(ME::TooManyHours))
    );
    // On hold (R83): no claims.
    mc.set_hold(&v.owner, &v.machine, &true, &1);
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &1000, &100),
        Err(Ok(ME::Unavailable))
    );
    mc.set_hold(&v.owner, &v.machine, &false, &0);
    // Only a leader registers; only members own machines; rates are the union's (3 of 5).
    assert_eq!(
        mc.try_register_machine(&fx.borrower, &fx.union, &v.owner, &1, &40, &id),
        Err(Ok(ME::NotLeader))
    );
    let stranger = Address::generate(&fx.env);
    assert_eq!(
        mc.try_register_machine(&fx.leader, &fx.union, &stranger, &1, &40, &id),
        Err(Ok(ME::NotMember))
    );
    // Not enough senior money for the value of the hours asked.
    let id2 = loan_id(&fx.env, 2);
    c.attest_food_token(&fx.attester, &fx.union, &id2, &fx.borrower, &0, &2000);
    assert_eq!(
        c.try_draw_loan(&fx.union, &fx.borrower, &id2, &0, &2000),
        Err(Ok(Error::NoSeniorBacking))
    );
}

#[test]
fn default_takes_the_voucher_value_from_her_investment_first() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    // Enough invested to cover all she will owe.
    fx.invest(&fx.borrower, 3_000 * NIN);
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(300 * NIN),
        &200,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(300 * NIN), &200);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &200, &200);
    c.report_maturity(&fx.oracle, &fx.union, &id, &T0);
    fx.at(T0 + 111 * 86_400 + 1);
    c.mark_default(&fx.union, &id);
    // Her investment covers it all: the voucher's value and interest to senior, junior in full.
    assert_eq!(fx.token().balance(&v.owner), 2 * HOUR);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(sm.total_borrows, 0);
    assert!(sm.cash > 10_000 * NIN);
    assert!(mc.voucher(&fx.union, &id).unwrap().void);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    v.assert_conserved();
}

#[test]
fn default_unused_hours_go_back_to_senior_and_it_takes_the_rest() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(300 * NIN),
        &400,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(300 * NIN), &400);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &200, &200);
    c.report_maturity(&fx.oracle, &fx.union, &id, &T0);
    fx.at(T0 + 111 * 86_400 + 1);
    c.mark_default(&fx.union, &id);
    // Her 2 unused hours lapse and their reserve takes ₹2,900 off the ₹5,800 she owed senior. Her
    // 2,000 invested goes to senior first, as far as the junior pool holds cash (₹1,700 after the
    // ₹300 draw), then to her junior principal; senior loses the rest.
    assert_eq!(fx.token().balance(&v.owner), 2 * HOUR);
    assert!(mc.voucher(&fx.union, &id).unwrap().void);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(sm.total_borrows, 0);
    assert_eq!(sm.cash, 10_000 * NIN - 2 * HOUR + 1_700 * NIN);
    assert_eq!(c.position(&fx.union, &fx.borrower).invested, 0);
    v.assert_conserved();
}

#[test]
fn self_claim_rate_rise_and_handing_hours_back() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    use nila_machinery::Error as ME;
    // An owner cannot claim on her own voucher (here the borrower owns a machine too).
    let mine = mc.register_machine(
        &fx.leader,
        &fx.union,
        &fx.borrower,
        &1,
        &40,
        &loan_id(&fx.env, 9),
    );
    mc.confirm_machine(&fx.borrower, &mine);
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &400);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &400);
    assert_eq!(
        mc.try_claim(&fx.borrower, &mine, &fx.union, &id, &400, &100),
        Err(Ok(ME::SelfClaim))
    );
    // The union raises its tractor rate 50% after the draw: the owner is paid what the voucher
    // set aside, never another voucher's reserve.
    mc.set_rate(&fx.union, &1, &tractor(15_000_000));
    let id2 = loan_id(&fx.env, 2);
    mc.set_rate(&fx.union, &1, &tractor(NIN));
    c.attest_food_token(&fx.attester, &fx.union, &id2, &fx.borrower, &0, &200);
    c.draw_loan(&fx.union, &fx.borrower, &id2, &0, &200);
    mc.set_rate(&fx.union, &1, &tractor(15_000_000));
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &400, &400);
    assert_eq!(fx.token().balance(&v.owner), 4 * HOUR);
    assert_eq!(c.senior_earmark(&fx.union), 2 * HOUR);
    mc.set_rate(&fx.union, &1, &tractor(NIN));

    // She hands back 1 of her 2 unused hours on loan 2: ₹1,450 off what she owes senior, its
    // reserve back to senior cash, and 5% (₹72.5) owed to the union treasury.
    let sm0 = c.get_senior_market(&fx.union);
    assert_eq!(
        c.return_voucher(&fx.borrower, &fx.union, &id2, &100),
        HOUR * 95 / 100
    );
    let sm1 = c.get_senior_market(&fx.union);
    assert_eq!(
        (sm1.cash - sm0.cash, sm0.total_borrows - sm1.total_borrows),
        (HOUR, HOUR)
    );
    assert_eq!(mc.voucher(&fx.union, &id2).unwrap().hours, 100);
    assert_eq!(c.senior_earmark(&fx.union), HOUR);
    // Not more than she has unclaimed.
    assert!(c
        .try_return_voucher(&fx.borrower, &fx.union, &id2, &200)
        .is_err());
    // Repaying loan 2: the rest of the voucher's value to senior, the 5% to the treasury.
    let t0 = c.union_treasury(&fx.union);
    let owed = HOUR + HOUR * 5 / 100;
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &owed);
    assert!(c.repay_loan(&fx.union, &id2, &owed));
    assert_eq!(c.union_treasury(&fx.union) - t0, HOUR * 5 / 100);
    // Nothing owed any more: handing back is refused, the hour stays hers.
    assert!(c
        .try_return_voucher(&fx.borrower, &fx.union, &id2, &100)
        .is_err());
    assert_eq!(
        mc.voucher(&fx.union, &id2).unwrap().hours - mc.voucher(&fx.union, &id2).unwrap().used,
        100
    );
    v.assert_conserved();
}

#[test]
fn bring_respects_the_daily_cap_and_the_voucher_earmark() {
    let v = vsetup();
    let (fx, c, f) = (&v.fx, v.fx.core(), v.fx.fx());
    let anchor = Address::generate(&fx.env);
    f.set_anchor(&anchor);
    f.set_id_checked(&v.owner, &true, &loan_id(&fx.env, 0x4B));
    f.set_lp(&fx.union, &v.owner, &true, &loan_id(&fx.env, 0x4C));
    fx.scan(Purpose::Treasury, &fx.leader, 5_000 * NIN, 1, None)
        .unwrap();
    let (s24, q) = fx.ids();
    // 200 senior USDC at ₹50; a voucher of 6.76 tractor hours sets aside ₹9,802 = 196.04 USDC.
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &676);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &676);
    // ₹250 → 5 USDC + 1% would cut into the 196 USDC held for the voucher.
    let b = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(250 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(
        fx.try_acc(&v.owner, &b, &s24, &q),
        Err(Ok(FxError::BelowVoucherFloor))
    );
    f.cancel_offer(&fx.leader, &b);
    // ₹150 → 3.03 USDC fits in the 4 free.
    let ok = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(150 * NIN),
        &(T0 + 3_600),
    );
    fx.acc(&v.owner, &ok, &s24, &q);
    // The daily cap: set it below the payout and the confirm is refused.
    f.set_daily_cap(&NIN);
    assert_eq!(
        f.try_confirm(&fx.leader, &ok),
        Err(Ok(FxError::CapExceeded))
    );
    f.set_daily_cap(&(2_500 * NIN));
    f.confirm(&fx.leader, &ok);
}

#[test]
fn a_title_in_any_union_makes_her_junior_never_senior() {
    let fx = setup();
    let f = fx.fx();
    // A member of another union: she holds a title there, not here.
    let other_union = Address::generate(&fx.env);
    let her = Address::generate(&fx.env);
    LandTitleContractClient::new(&fx.env, &fx.titles_id).set_title(
        &900,
        &fx.attester,
        &her,
        &other_union,
        &BytesN::from_array(&fx.env, &[0xAB; 20]),
        &BytesN::from_array(&fx.env, &[1; 32]),
        &BytesN::from_array(&fx.env, &[2; 32]),
    );
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&her, &(10 * NIN));
    f.set_senior_investor(&her, &true, &loan_id(&fx.env, 1));
    f.override_rate(&(95 * NIN), &(T0 + 86_400));
    assert!(f
        .try_mint_senior(&her, &fx.union, &(10 * NIN), &Tag::Domestic)
        .is_err());
    // The attester's band is adjustable by the trust.
    f.set_rate_band(&1_000);
    f.set_rate(&fx.attester, &(104 * NIN), &(T0 + 86_400));
    assert_eq!(
        f.try_set_rate(&fx.attester, &(120 * NIN), &(T0 + 86_400)),
        Err(Ok(FxError::BadRate))
    );
}

#[test]
fn handing_hours_back_is_hers_only() {
    let v = vsetup();
    let (fx, c) = (&v.fx, v.fx.core());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &200);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &200);
    assert_eq!(
        c.try_return_voucher(&fx.neighbour, &fx.union, &id, &100),
        Err(Ok(Error::NotBorrower))
    );
    // All of it back: her debt is the 5% for the treasury only.
    c.return_voucher(&fx.borrower, &fx.union, &id, &200);
    let b = c.voucher_book(&fx.union, &id).unwrap();
    assert_eq!(
        (b.value - b.repaid, b.fee_due, b.reserve),
        (0, 2 * HOUR * 5 / 100, 0)
    );
}

#[test]
fn repaid_cash_goes_out_by_collect_and_a_loan_comes_in_by_bring() {
    let fx = setup();
    let (c, f) = (fx.core(), fx.fx());
    let (_anchor, lp) = fx.lp_ready(); // treasury 5,000; the borrower invested 2,000 (escrow)
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(100 * NIN));
    let (s24, q) = fx.ids();
    // No other borrower for the 2,000 in the drawer: an LP collects it for USDC.
    let col = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Collect,
        &(1_900 * NIN),
        &(T0 + 3_600),
    );
    fx.acc(&lp, &col, &s24, &q);
    f.confirm(&fx.leader, &col);
    assert_eq!(f.cash_at_hand(&fx.union), 100 * NIN);
    // Later a member wants 950 in cash and the drawer is short: an LP brings it.
    let br = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(950 * NIN),
        &(T0 + 3_600),
    );
    fx.acc(&lp, &br, &s24, &q);
    f.confirm(&fx.leader, &br);
    // Her rupees are cash at hand until the draw hands them out.
    assert_eq!(f.cash_at_hand(&fx.union), 1_050 * NIN);
    let id = loan_id(&fx.env, 1);
    fx.attest(&fx.borrower, &id, 950 * NIN);
    c.draw_loan(&fx.union, &fx.borrower, &id, &(950 * NIN), &0);
    assert_eq!(f.cash_at_hand(&fx.union), 100 * NIN);
    // A scan can never open a Bring escrow itself.
    assert!(fx.scan(Purpose::Bring, &fx.borrower, NIN, 9, None).is_err());
    fx.assert_conserved();
}

#[test]
fn exactly_the_gate_passes_after_interest_raised_the_share_price() {
    let fx = setup();
    let c = fx.core();
    // Interest earned on a repaid loan lifts the junior index above 1.
    fx.invest(&fx.neighbour, 2_000 * NIN);
    fx.invest(&fx.borrower, 1_000 * NIN);
    let first = loan_id(&fx.env, 1);
    fx.draw(&first, 500 * NIN);
    fx.at(T0 + YEAR / 3);
    fx.repay(&first, 600 * NIN);
    assert!(c.get_junior_market(&fx.union).index > nila_math::RAY);
    // A new member invests exactly ₹600: her position reads a hair under it, and still passes.
    let her = Address::generate(&fx.env);
    fx.give_title(&her);
    fx.invest(&her, 600 * NIN);
    assert!(c.position(&fx.union, &her).invested < 600 * NIN);
    let tok = loan_id(&fx.env, 2);
    fx.attest(&her, &tok, 100 * NIN);
    c.draw_loan(&fx.union, &her, &tok, &(100 * NIN), &0);
    assert!(c.get_loan(&fx.union, &tok).is_some());
}

/// The move (M1): her burned Polygon nIN is minted once, by the attester, only while she holds an
/// attested title, as a cash-out under her code (22 Oct: no nIN in a member's wallet). It never expires;
/// a leader of her union pays it at the desk.
#[test]
fn the_move_mints_her_polygon_nin_once_as_a_desk_cash_out() {
    let f = setup();
    let burn = BytesN::from_array(&f.env, &[9; 32]);
    let code = Bytes::from_array(&f.env, b"moved-code-1");
    let ch: BytesN<32> = f.env.crypto().sha256(&code).into();
    let stranger = Address::generate(&f.env);
    // No budget set: nothing can be minted for the move.
    assert_eq!(
        f.fx()
            .try_mint_moved(&f.attester, &f.union, &f.borrower, &(77 * NIN), &burn, &ch),
        Err(Ok(nila_fx_pool::Error::MoveBudget))
    );
    f.fx().add_move_budget(&(150 * NIN));
    f.fx().add_move_budget(&(-50 * NIN)); // taken back: 100 left
    assert_eq!(
        f.fx().try_add_move_budget(&(-101 * NIN)),
        Err(Ok(nila_fx_pool::Error::AmountZero))
    );
    assert_eq!(
        f.fx()
            .try_mint_moved(&f.attester, &f.union, &stranger, &(77 * NIN), &burn, &ch),
        Err(Ok(nila_fx_pool::Error::NotMember))
    );
    assert_eq!(
        f.fx()
            .try_mint_moved(&f.leader, &f.union, &f.borrower, &(77 * NIN), &burn, &ch),
        Err(Ok(nila_fx_pool::Error::Unauthorized))
    );
    f.fx()
        .mint_moved(&f.attester, &f.union, &f.borrower, &(77 * NIN), &burn, &ch);
    // Nothing in her wallet: the nIN waits in fx_pool under her code.
    assert_eq!(f.token().balance(&f.borrower), 0);
    assert_eq!(f.token().balance(&f.fx_id), 77 * NIN);
    assert!(f.fx().moved(&burn));
    assert_eq!(
        f.fx().try_mint_moved(
            &f.attester,
            &f.union,
            &f.borrower,
            &(77 * NIN),
            &burn,
            &hash(&f.env, 40)
        ),
        Err(Ok(nila_fx_pool::Error::AlreadyMoved))
    );
    // Her code is open: a second move cannot reuse it.
    assert_eq!(
        f.fx().try_mint_moved(
            &f.attester,
            &f.union,
            &f.borrower,
            &NIN,
            &BytesN::from_array(&f.env, &[6; 32]),
            &ch
        ),
        Err(Ok(nila_fx_pool::Error::CodeUsed))
    );
    assert_eq!(
        f.fx().try_mint_moved(
            &f.attester,
            &f.union,
            &f.borrower,
            &(10_001 * NIN),
            &BytesN::from_array(&f.env, &[8; 32]),
            &hash(&f.env, 41)
        ),
        Err(Ok(nila_fx_pool::Error::CapExceeded))
    );
    // The budget is drawn down: 23 left of 100.
    assert_eq!(f.fx().move_budget(), 23 * NIN);
    assert_eq!(
        f.fx().try_mint_moved(
            &f.attester,
            &f.union,
            &f.borrower,
            &(24 * NIN),
            &BytesN::from_array(&f.env, &[7; 32]),
            &hash(&f.env, 42)
        ),
        Err(Ok(nila_fx_pool::Error::MoveBudget))
    );

    // Years later it is still there: a moved cash-out never expires and can never be reclaimed.
    f.at(T0 + 10 * YEAR);
    f.env.set_auths(&[]);
    assert_eq!(
        f.fx().try_reclaim_cash_out(&ch),
        Err(Ok(nila_fx_pool::Error::NotExpired))
    );
    f.env.mock_all_auths();
    // Another union's leader cannot pay it.
    let other_union = Address::generate(&f.env);
    let other_leader = Address::generate(&f.env);
    nila_roles::RolesContractClient::new(&f.env, &f.roles_id).set_leader(
        &other_union,
        &other_leader,
        &true,
    );
    assert_eq!(
        f.fx().try_pay_out(&other_leader, &code),
        Err(Ok(nila_fx_pool::Error::NotLeader))
    );
    // A leader of her union pays it: burned, cash at hand down; her wallet stays at 0.
    f.scan(Purpose::Treasury, &f.leader, 2_000 * NIN, 1, None)
        .unwrap();
    f.scan(Purpose::Invest, &f.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
    let hand = f.fx().cash_at_hand(&f.union);
    assert_eq!(f.fx().pay_out(&f.leader, &code), 77 * NIN);
    assert_eq!(f.fx().cash_at_hand(&f.union), hand - 77 * NIN);
    assert_eq!(f.token().balance(&f.borrower), 0);
    assert_eq!(f.token().balance(&f.fx_id), 0);
    assert_eq!(
        f.fx().try_pay_out(&f.leader, &code),
        Err(Ok(nila_fx_pool::Error::NotFound))
    );
}

/// Her investment collected over the desk (22 Oct): she and a leader sign; the claim is burned and
/// booked against cash at hand like a loan's cash part; nothing reaches her wallet.
#[test]
fn claim_junior_cash_pays_over_the_desk() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(400 * NIN));
    fx.at(T0 + 14 * 86_400);

    // A member can no longer claim to her wallet.
    assert_eq!(
        c.try_claim_junior(&fx.union, &fx.neighbour, &0),
        Err(Ok(Error::NotLeader))
    );
    // Not a leader on the desk side.
    assert_eq!(
        c.try_claim_junior_cash(&fx.union, &fx.neighbour, &fx.borrower, &0),
        Err(Ok(Error::NotLeader))
    );
    // She must sign it herself: a leader alone cannot collect her investment.
    fx.env.mock_auths(&[MockAuth {
        address: &fx.leader,
        invoke: &MockAuthInvoke {
            contract: &fx.core_id,
            fn_name: "claim_junior_cash",
            args: (
                fx.union.clone(),
                fx.neighbour.clone(),
                fx.leader.clone(),
                0i128,
            )
                .into_val(&fx.env),
            sub_invokes: &[],
        },
    }]);
    assert!(c
        .try_claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0)
        .is_err());
    // And the leader must sign too: she alone cannot collect cash at the desk. Her QR (24 Oct) signs
    // (union, max_shares) only, so it names no leader.
    fx.env.mock_auths(&[MockAuth {
        address: &fx.neighbour,
        invoke: &MockAuthInvoke {
            contract: &fx.core_id,
            fn_name: "claim_junior_cash",
            args: (fx.union.clone(), 0i128).into_val(&fx.env),
            sub_invokes: &[],
        },
    }]);
    assert!(c
        .try_claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0)
        .is_err());
    fx.env.mock_all_auths();

    // Her signature of the full call (with a leader in it) is not hers for this claim: the args differ.
    fx.env.mock_auths(&[
        MockAuth {
            address: &fx.neighbour,
            invoke: &MockAuthInvoke {
                contract: &fx.core_id,
                fn_name: "claim_junior_cash",
                args: (
                    fx.union.clone(),
                    fx.neighbour.clone(),
                    fx.leader.clone(),
                    0i128,
                )
                    .into_val(&fx.env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &fx.leader,
            invoke: &MockAuthInvoke {
                contract: &fx.core_id,
                fn_name: "claim_junior_cash",
                args: (
                    fx.union.clone(),
                    fx.neighbour.clone(),
                    fx.leader.clone(),
                    0i128,
                )
                    .into_val(&fx.env),
                sub_invokes: &[],
            },
        },
    ]);
    assert!(c
        .try_claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0)
        .is_err());
    fx.env.mock_all_auths();

    let hand = f.cash_at_hand(&fx.union);
    let supply_core = fx.token().balance(&fx.core_id);
    c.claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0);
    let auths = fx.env.auths();
    assert!(auths.iter().any(|a| a.0 == fx.neighbour
        && matches!(&a.1.function, AuthorizedFunction::Contract((_, f, args))
            if *f == Symbol::new(&fx.env, "claim_junior_cash") && args.len() == 2)));
    assert!(auths.iter().any(|a| a.0 == fx.leader));
    assert_eq!(fx.token().balance(&fx.neighbour), 0);
    assert_eq!(fx.token().balance(&fx.core_id), supply_core - 400 * NIN);
    assert_eq!(f.cash_at_hand(&fx.union), hand - 400 * NIN);
    assert_eq!(c.position(&fx.union, &fx.neighbour).invested, 600 * NIN);
    assert_eq!(
        c.try_claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0),
        Err(Ok(Error::NothingToClaim))
    );
    fx.assert_conserved();
}

/// Cash at hand short: as for a loan's cash part, the claim books what is at hand and still pays.
#[test]
fn claim_junior_cash_with_cash_at_hand_short() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
    fx.invest(&fx.borrower, 1_000 * NIN);
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(400 * NIN));
    fx.at(T0 + 14 * 86_400);
    // The leader already handed out most of the counted cash (a loan's cash part).
    fx.attest(&fx.borrower, &loan_id(&fx.env, 1), 900 * NIN);
    c.draw_loan(
        &fx.union,
        &fx.borrower,
        &loan_id(&fx.env, 1),
        &(700 * NIN),
        &0,
    );
    let hand = f.cash_at_hand(&fx.union);
    assert!(hand < 400 * NIN);
    c.claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0);
    assert_eq!(f.cash_at_hand(&fx.union), 0);
    assert_eq!(fx.token().balance(&fx.neighbour), 0);
    assert_eq!(fx.token().balance(&fx.borrower), 0);
}

/// No member path leaves nIN in her wallet: draw, claim over the desk, the move.
#[test]
fn a_member_holds_no_nin_after_any_member_path() {
    let fx = setup();
    let c = fx.core();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.invest(&fx.neighbour, 1_000 * NIN);
    fx.draw(&loan_id(&fx.env, 1), 300 * NIN);
    assert_eq!(fx.token().balance(&fx.borrower), 0);
    c.request_unbond_junior(&fx.union, &fx.neighbour, &(500 * NIN));
    fx.at(T0 + 14 * 86_400);
    c.claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0);
    assert_eq!(fx.token().balance(&fx.neighbour), 0);
    fx.fx().add_move_budget(&(50 * NIN));
    fx.fx().mint_moved(
        &fx.attester,
        &fx.union,
        &fx.neighbour,
        &(50 * NIN),
        &BytesN::from_array(&fx.env, &[3; 32]),
        &hash(&fx.env, 43),
    );
    assert_eq!(fx.token().balance(&fx.neighbour), 0);
    assert_eq!(fx.token().balance(&fx.borrower), 0);
}

#[test]
fn accept_needs_the_anchors_signed_quote() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    let (s24, q) = fx.ids();
    let c = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Collect,
        &(950 * NIN),
        &(T0 + 3_600),
    );
    let (usdc, until, sig) = fx.signed(&lp, c, &s24, &q);
    // Another SEP-24 id, another USDC amount or another LP than the anchor signed: the signature
    // does not verify (a host trap, not a contract error code).
    let other = SString::from_str(&fx.env, "sep24-other");
    assert!(matches!(
        f.try_accept(&lp, &c, &other, &q, &usdc, &until, &sig),
        Err(Err(_))
    ));
    assert!(matches!(
        f.try_accept(&lp, &c, &s24, &q, &(usdc + 1), &until, &sig),
        Err(Err(_))
    ));
    f.set_id_checked(&fx.borrower, &true, &loan_id(&fx.env, 0x4B));
    f.set_lp(&fx.union, &fx.borrower, &true, &loan_id(&fx.env, 0x4D));
    assert!(matches!(
        f.try_accept(&fx.borrower, &c, &s24, &q, &usdc, &until, &sig),
        Err(Err(_))
    ));
    // A rotated key voids the signature; the right key works again.
    f.set_swap_key(&BytesN::from_array(&fx.env, &[1u8; 32]));
    assert!(matches!(
        f.try_accept(&lp, &c, &s24, &q, &usdc, &until, &sig),
        Err(Err(_))
    ));
    f.set_swap_key(&BytesN::from_array(
        &fx.env,
        &swap_key().verifying_key().to_bytes(),
    ));
    // The quote ran out.
    fx.at(until + 1);
    assert_eq!(
        f.try_accept(&lp, &c, &s24, &q, &usdc, &until, &sig),
        Err(Ok(FxError::QuoteExpired))
    );
    fx.at(T0);
    // The fix moved after the anchor quoted: the signed USDC no longer matches.
    f.override_rate(&(96 * NIN), &(T0 + 86_400));
    assert_eq!(
        f.try_accept(&lp, &c, &s24, &q, &usdc, &until, &sig),
        Err(Ok(FxError::QuoteChanged))
    );
    // A fresh signed quote goes through; the offer keeps the anchor's ids.
    let at96 = f.quote(&fx.union, &OfferKind::Collect, &(950 * NIN)).usdc;
    assert!(at96 < usdc);
    assert_eq!(fx.acc(&lp, &c, &s24, &q), at96);
    assert_eq!(f.offer(&c).unwrap().sep24_id, s24);
    assert_eq!(
        f.swap_key().unwrap().to_array(),
        swap_key().verifying_key().to_bytes()
    );
}

#[test]
fn leader_deposits_her_own_nin_into_the_treasury() {
    // C10 (executive decision, 20 Oct): as on Polygon, a leader puts her own nIN into the treasury; withdraw
    // and deposit are a round trip, and no rupees are scanned twice.
    let fx = setup();
    let c = fx.core();
    fx.scan(Purpose::Treasury, &fx.leader, 1_000 * NIN, 1, None)
        .unwrap();
    c.withdraw_union_treasury(&fx.leader, &fx.union, &(400 * NIN), &false);
    assert_eq!(fx.token().balance(&fx.leader), 400 * NIN);
    assert_eq!(c.union_treasury(&fx.union), 600 * NIN);
    c.leader_deposit_treasury(&fx.leader, &fx.union, &(250 * NIN));
    // Only she signs: the call and, inside it, the nIN transfer from her wallet (one auth tree).
    let auths = fx.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, fx.leader);
    assert_eq!(auths[0].1.sub_invocations.len(), 1);
    assert_eq!(fx.token().balance(&fx.leader), 150 * NIN);
    assert_eq!(c.union_treasury(&fx.union), 850 * NIN);
    fx.assert_conserved();
    // Her trustline is authorised only inside the call (R10).
    assert!(!StellarAssetClient::new(&fx.env, &fx.nin).authorized(&fx.leader));
    // Only a leader of the union, only a positive amount, only nIN she has.
    assert_eq!(
        c.try_leader_deposit_treasury(&fx.borrower, &fx.union, &NIN),
        Err(Ok(Error::NotLeader))
    );
    assert_eq!(
        c.try_leader_deposit_treasury(&fx.leader, &fx.union, &0),
        Err(Ok(Error::AmountZero))
    );
    assert!(c
        .try_leader_deposit_treasury(&fx.leader, &fx.union, &(151 * NIN))
        .is_err());
    // Paused: no money moves in.
    c.pause();
    assert_eq!(
        c.try_leader_deposit_treasury(&fx.leader, &fx.union, &NIN),
        Err(Ok(Error::Paused))
    );
}

// ---------- 22 Oct: the treasury is USDC-backed, never counted cash ----------

#[test]
fn treasury_cash_scan_is_refused() {
    let fx = setup();
    let f = fx.fx();
    fx.env.mock_all_auths();
    assert_eq!(
        f.try_cash_scan_mint(
            &fx.leader,
            &fx.union,
            &Purpose::Treasury,
            &fx.leader,
            &(1_000 * NIN),
            &hash(&fx.env, 0x71),
            &None,
        ),
        Err(Ok(FxError::Unauthorized))
    );
    assert_eq!(fx.core().union_treasury(&fx.union), 0);
}

#[test]
fn leader_usdc_into_the_treasury() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.env.mock_all_auths();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&fx.leader, &(100 * NIN));
    // No rate yet: refused.
    assert_eq!(
        f.try_deposit_usdc_treasury(&fx.leader, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::StaleOracle))
    );
    f.override_rate(&(95 * NIN), &(T0 + 86_400));

    // Only the leader signs: her USDC transfer, fx_pool's own pull into fund_core.
    let args = (fx.leader.clone(), fx.union.clone(), 100 * NIN);
    fx.env.mock_auths(&[MockAuth {
        address: &fx.leader,
        invoke: &MockAuthInvoke {
            contract: &fx.fx_id,
            fn_name: "deposit_usdc_treasury",
            args: args.into_val(&fx.env),
            sub_invokes: &[MockAuthInvoke {
                contract: &fx.usdc,
                fn_name: "transfer",
                args: (fx.leader.clone(), fx.fx_id.clone(), 100 * NIN).into_val(&fx.env),
                sub_invokes: &[],
            }],
        },
    }]);
    assert_eq!(
        f.deposit_usdc_treasury(&fx.leader, &fx.union, &(100 * NIN)),
        9_500 * NIN
    );
    fx.env.mock_all_auths();
    assert_eq!(c.union_treasury(&fx.union), 9_500 * NIN);
    assert_eq!(f.treasury_usdc(&fx.union), 100 * NIN);
    assert_eq!(
        TokenClient::new(&fx.env, &fx.usdc).balance(&fx.fx_id),
        100 * NIN
    );
    assert_eq!(fx.token().balance(&fx.fx_id), 0);

    // Above the per-call cap (₹10,000): refused.
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&fx.leader, &(200 * NIN));
    assert_eq!(
        f.try_deposit_usdc_treasury(&fx.leader, &fx.union, &(200 * NIN)),
        Err(Ok(FxError::CapExceeded))
    );
    // Not a leader of the union: refused.
    let outsider = Address::generate(&fx.env);
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&outsider, &(10 * NIN));
    assert_eq!(
        f.try_deposit_usdc_treasury(&outsider, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::NotLeader))
    );
    // The rate went stale: refused.
    fx.env.ledger().set_timestamp(T0 + 2 * 86_400);
    assert_eq!(
        f.try_deposit_usdc_treasury(&fx.leader, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::StaleOracle))
    );
    assert_eq!(c.union_treasury(&fx.union), 9_500 * NIN);
}

#[test]
fn the_anchor_marks_identity_checked_and_can_withdraw_it() {
    let fx = setup();
    let f = fx.fx();
    let leader = fx.leader.clone();
    // No anchor set yet: nobody can mark it.
    assert_eq!(
        f.try_set_id_checked(&leader, &true, &loan_id(&fx.env, 9)),
        Err(Ok(FxError::NotAnchor))
    );
    let anchor = Address::generate(&fx.env);
    f.set_anchor(&anchor);
    assert!(!f.id_checked(&leader));
    f.set_id_checked(&leader, &true, &loan_id(&fx.env, 9));
    assert!(f.id_checked(&leader));
    // Only the anchor signs it.
    fx.env.set_auths(&[]);
    assert!(f
        .try_set_id_checked(&leader, &false, &loan_id(&fx.env, 9))
        .is_err());
    fx.env.mock_all_auths();
    assert!(f.id_checked(&leader));
    // Withdrawn.
    f.set_id_checked(&leader, &false, &loan_id(&fx.env, 9));
    assert!(!f.id_checked(&leader));
}

#[test]
fn leader_swaps_usdc_to_nin_and_back_from_her_bucket_only() {
    let fx = setup();
    let f = fx.fx();
    fx.env.mock_all_auths();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&fx.leader, &(100 * NIN));
    f.override_rate(&(95 * NIN), &(T0 + 86_400));
    // Identity not checked: refused.
    assert_eq!(
        f.try_swap_usdc_to_nin(&fx.leader, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::NotIdChecked))
    );
    f.set_anchor(&Address::generate(&fx.env));
    f.set_id_checked(&fx.leader, &true, &loan_id(&fx.env, 0x51));
    // Not a leader of the union: refused.
    let outsider = Address::generate(&fx.env);
    assert_eq!(
        f.try_swap_usdc_to_nin(&outsider, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::NotLeader))
    );
    // Treasury USDC sits in its own bucket and is never paid out by a swap back.
    fx.treasury_usdc(950 * NIN).unwrap();
    assert_eq!(
        f.try_swap_nin_to_usdc(&fx.leader, &fx.union, &(95 * NIN)),
        Err(Ok(FxError::NoUsdc))
    );
    // USDC → nIN in her wallet; the USDC stays in fx_pool in the leader-swap bucket.
    f.override_rate(&(95 * NIN), &(T0 + 86_400));
    assert_eq!(
        f.swap_usdc_to_nin(&fx.leader, &fx.union, &(10 * NIN)),
        950 * NIN
    );
    assert_eq!(fx.token().balance(&fx.leader), 950 * NIN);
    assert_eq!(f.leader_swap_usdc(&fx.union), 10 * NIN);
    assert_eq!(f.treasury_usdc(&fx.union), 95 * NIN / 10);
    // Back: more than the bucket holds is refused; within it, her nIN burns and USDC comes back.
    assert_eq!(
        f.try_swap_nin_to_usdc(&fx.leader, &fx.union, &(1_900 * NIN)),
        Err(Ok(FxError::NoUsdc))
    );
    assert_eq!(
        f.swap_nin_to_usdc(&fx.leader, &fx.union, &(475 * NIN)),
        5 * NIN
    );
    assert_eq!(fx.token().balance(&fx.leader), 475 * NIN);
    assert_eq!(f.leader_swap_usdc(&fx.union), 5 * NIN);
    assert_eq!(f.treasury_usdc(&fx.union), 95 * NIN / 10);
    // A stale rate: refused both ways.
    fx.env.ledger().set_timestamp(T0 + 2 * 86_400);
    assert_eq!(
        f.try_swap_usdc_to_nin(&fx.leader, &fx.union, &NIN),
        Err(Ok(FxError::StaleOracle))
    );
    assert_eq!(
        f.try_swap_nin_to_usdc(&fx.leader, &fx.union, &NIN),
        Err(Ok(FxError::StaleOracle))
    );
}

/// The last draw date and the public lapse (executive decision 25 Oct): no floor, drawn up to and on its last
/// date, too late after it; an undrawn token past its last date lapses by anyone and is never drawn or attested
/// again; a drawn one cannot lapse; a voucher-only share lapses the same way.
#[test]
fn last_draw_date_and_public_lapse() {
    let fx = setup();
    let c = fx.core();
    fx.invest(&fx.borrower, 3_000 * NIN);
    let last = T0 + 10 * DAY;
    fx.window(Some((0, last)));
    let attest = |b: u8, amount: i128, hours: u32| {
        c.attest_food_token(
            &fx.attester,
            &fx.union,
            &loan_id(&fx.env, b),
            &fx.borrower,
            &amount,
            &hours,
        )
    };
    // A sowing in none of its crop's windows (or no calendar): refused.
    fx.window(None);
    assert_eq!(
        c.try_attest_food_token(
            &fx.attester,
            &fx.union,
            &loan_id(&fx.env, 9),
            &fx.borrower,
            &(100 * NIN),
            &0,
        )
        .unwrap_err(),
        Ok(Error::BadDates)
    );
    fx.window(Some((0, last)));
    for b in 1..=4 {
        attest(b, 100 * NIN, 0);
    }
    attest(5, 0, 300); // voucher-only
    let draw = |b: u8| {
        c.try_draw_loan(
            &fx.union,
            &fx.borrower,
            &loan_id(&fx.env, b),
            &(100 * NIN),
            &0,
        )
    };
    // No floor: drawn right after attestation.
    assert!(draw(1).is_ok());
    assert_eq!(
        c.food_token(&fx.union, &loan_id(&fx.env, 1))
            .unwrap()
            .final_date,
        last
    );
    // Not lapsed before or at its last date.
    let lapse = |b: u8| c.try_lapse_food_token(&fx.union, &loan_id(&fx.env, b));
    assert_eq!(lapse(2).unwrap_err(), Ok(Error::NotLapsedYet));
    fx.at(last);
    assert_eq!(lapse(2).unwrap_err(), Ok(Error::NotLapsedYet));
    // On its last date: still drawn.
    assert!(draw(2).is_ok());
    // After it: too late, and anyone may lapse an undrawn one (no signature at all).
    fx.at(last + 1);
    assert_eq!(draw(3).unwrap_err(), Ok(Error::TooLate));
    fx.env.set_auths(&[]);
    c.lapse_food_token(&fx.union, &loan_id(&fx.env, 3));
    c.lapse_food_token(&fx.union, &loan_id(&fx.env, 5));
    fx.env.mock_all_auths();
    let v = c.food_token(&fx.union, &loan_id(&fx.env, 3)).unwrap();
    assert!(v.lapsed && !v.loan.drawn());
    assert!(
        c.food_token(&fx.union, &loan_id(&fx.env, 5))
            .unwrap()
            .lapsed
    );
    // Lapsed once only; never drawn or attested again.
    assert_eq!(lapse(3).unwrap_err(), Ok(Error::FoodTokenLapsed));
    assert_eq!(draw(3).unwrap_err(), Ok(Error::FoodTokenLapsed));
    assert_eq!(
        c.try_attest_food_token(
            &fx.attester,
            &fx.union,
            &loan_id(&fx.env, 3),
            &fx.borrower,
            &(100 * NIN),
            &0,
        )
        .unwrap_err(),
        Ok(Error::FoodTokenLapsed)
    );
    // A drawn token cannot lapse; an unknown one is not there.
    assert_eq!(lapse(1).unwrap_err(), Ok(Error::FoodTokenUsed));
    assert_eq!(lapse(77).unwrap_err(), Ok(Error::NoFoodToken));
    // Undrawn and past its date but not lapsed yet: still lapsable.
    assert!(lapse(4).is_ok());
    fx.assert_conserved();
}

/// Usable in its window (executive decision 25 Oct): from the crop calendar's opening (a week
/// before its crop's sowing window) up to its last sowing day.
#[test]
fn a_crop_share_is_usable_only_in_its_attested_window() {
    let fx = setup();
    let c = fx.core();
    fx.invest(&fx.borrower, 3_000 * NIN);
    let id = loan_id(&fx.env, 1);
    let (open, last) = (T0 + 40 * DAY, T0 + 100 * DAY);
    fx.window(Some((open, last)));
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &(100 * NIN), &0);
    let v = c.food_token(&fx.union, &id).unwrap();
    assert_eq!((v.from_date, v.final_date), (open, last));
    let draw = || c.try_draw_loan(&fx.union, &fx.borrower, &id, &(100 * NIN), &0);
    fx.at(open - 1);
    assert_eq!(draw().unwrap_err(), Ok(Error::NotOpenYet));
    fx.at(open);
    assert!(draw().is_ok());
}

/// The real crop calendar and a packed Polygon token id (25 Oct): maize (family 6) sown 20 Jun
/// 2027 opens 25 May, a week before its Jun–Jul window, and closes at the end of 31 July.
#[test]
fn the_window_comes_from_the_crop_calendar_by_the_token_ids_crop_and_sowing() {
    use nila_crop_calendar::{CropCalendarContract, CropCalendarContractClient, Window};
    let fx = setup();
    let c = fx.core();
    let cal = fx
        .env
        .register(CropCalendarContract, (Address::generate(&fx.env),));
    CropCalendarContractClient::new(&fx.env, &cal).set_windows(
        &6,
        &soroban_sdk::vec![&fx.env, Window { first: 6, last: 7 }],
    );
    c.set_crop_calendar(&cal);
    // packTokenId(land 42, crop 6000, sos 2027-06-20, area 4000 m², fields 0b1)
    let pack = |crop: u128, sos: u128| {
        let hi: u128 = (42 << 96) | (crop << 76) | (sos << 44) | (4000 << 20);
        let mut b = [0u8; 32];
        b[..16].copy_from_slice(&hi.to_be_bytes());
        b[16..].copy_from_slice(&(1u128 << 20).to_be_bytes());
        BytesN::from_array(&fx.env, &b)
    };
    let id = pack(6000, 1_813_449_600);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &(100 * NIN), &0);
    let v = c.food_token(&fx.union, &id).unwrap();
    assert_eq!((v.from_date, v.final_date), (1_811_203_200, 1_817_078_399));
    // April: not a maize sowing month here.
    assert_eq!(
        c.try_attest_food_token(
            &fx.attester,
            &fx.union,
            &pack(6000, 1_806_537_600),
            &fx.borrower,
            &(100 * NIN),
            &0,
        )
        .unwrap_err(),
        Ok(Error::BadDates)
    );
}
