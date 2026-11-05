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

// The loan of a drawn food token, as the app reads it (food_token; get_loan was dropped 30 Oct).
fn loan(c: &FundCoreClient, union: &Address, token_id: &BytesN<32>) -> Option<Loan> {
    c.food_token(union, token_id)
        .filter(|f| f.loan.drawn())
        .map(|f| to_view(&f.member, &f.loan))
}

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
    senior_id: Address,
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

    /// `claim` (junior) pays a leader's own investment to her wallet (members collect over the desk with
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
            + c.union_book(&self.union).rainy
            + c.senior_owed_total(&self.union);
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
    roles.admit_union(&union, &soroban_sdk::BytesN::from_array(&env, &[1; 32]));
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
    // The senior contract (2 Nov): all senior USDC; the trust's cap set high here.
    let senior_id = env.register(
        nila_senior::SeniorContract,
        (
            admin.clone(),
            usdc.clone(),
            core_id.clone(),
            fx_id.clone(),
            roles_keep.clone(),
        ),
    );
    FxPoolContractClient::new(&env, &fx_id).set_senior(&senior_id);
    nila_senior::SeniorContractClient::new(&env, &senior_id).set_cap(&(1_000_000 * NIN));
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
        senior_id,
        next_title: core::cell::Cell::new(1),
    };
    fx.core().set_fx_pool(&fx.fx_id);
    fx.core().set_senior(&fx.senior_id);
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
    assert_eq!(c.union_book(&fx.union).borrows, 500 * NIN);
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
    let ln = loan(&c, &fx.union, &id).unwrap();
    assert!(ln.closed && !ln.defaulted);
    assert_eq!(ln.interest_paid, interest);
    assert_eq!(c.union_treasury(&fx.union), interest / 100);
    assert_eq!(c.union_book(&fx.union).rainy, interest * 77 / 10_000);
    assert_eq!(c.union_book(&fx.union).borrows, 0);
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
    // R34's ₹10,000 per loan is the plan's and the leaders' since 30 Oct, not code.
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
        c.try_request_unbond(&fx.union, &fx.borrower, &(201 * NIN), &false)
            .unwrap_err(),
        Ok(Error::BadShares)
    );
    c.request_unbond(&fx.union, &fx.borrower, &(200 * NIN), &false);

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
    c.request_unbond(&fx.union, &fx.neighbour, &(1_000 * NIN), &false);
    assert_eq!(
        c.try_claim(&fx.union, &fx.neighbour, &0, &false)
            .unwrap_err(),
        Ok(Error::NotEligibleMaturity)
    );
    fx.at(T0 + YEAR + 14 * 86_400);
    c.claim(&fx.union, &fx.neighbour, &0, &false);
    c.request_unbond(&fx.union, &fx.borrower, &(1_000 * NIN), &false);
    fx.at(T0 + YEAR + 28 * 86_400);
    c.claim(&fx.union, &fx.borrower, &0, &false);

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
    assert_eq!(c.union_book(&fx.union).on_hold, 1);
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
    assert_eq!(c.union_book(&fx.union).on_hold, 1);
    assert!(fx.repay(&id, 1_000 * NIN));
    assert_eq!(c.union_book(&fx.union).on_hold, 0);
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
    assert_eq!(c.union_book(&fx.union).borrows, 0);
    assert_eq!(c.get_junior_market(&fx.union).total_borrows, 0);
    let ln = loan(&c, &fx.union, &id).unwrap();
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
    c.request_unbond(&fx.union, &lp, &(1_000 * NIN), &true);
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
    // Testing (gate off): the union may give more than attested (no ₹10,000 cap in code since 30 Oct).
    c.set_union_config(&fx.union, &union_cfg());
    c.draw_loan(&fx.union, &fx.borrower, &tok, &(800 * NIN), &0);
    // The loan lives inside the token's entry; the token id is the loan id (C6).
    assert!(c.food_token(&fx.union, &tok).unwrap().loan.drawn());
    assert_eq!(loan(&c, &fx.union, &tok).unwrap().principal, 800 * NIN);

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
    IntoVal, Symbol,
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

/// Cash at the office may pass the treasury up to `cash_room` (executive decision 30 Oct, on-chain 31 Oct): the
/// treasury, withdrawals started and not collected, and the cash parts of undrawn food tokens closing within
/// `ROOM_WEEKS`. Above it a scan is refused; within it no Collect offer opens (the cash goes back out to members).
#[test]
fn cash_scan_is_refused_above_the_cash_room() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 1_500 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.borrower, 1_000 * NIN, 2, None)
        .unwrap();
    assert_eq!(c.cash_room(&fx.union), 1_500 * NIN);
    // 700 more would make 1,700 at the office: refused.
    assert_eq!(
        fx.scan(Purpose::Invest, &fx.neighbour, 700 * NIN, 3, None),
        Err(FxError::AboveTreasury)
    );
    // A crop share's cash part waiting to be taken up makes room for it.
    let id = loan_id(&fx.env, 1);
    let fin = {
        fx.attest(&fx.borrower, &id, 400 * NIN);
        c.food_token(&fx.union, &id).unwrap().final_date
    };
    let near = fin / (7 * 86_400) <= fx.env.ledger().timestamp() / (7 * 86_400) + crate::ROOM_WEEKS;
    assert_eq!(
        c.cash_room(&fx.union),
        1_500 * NIN + if near { 400 * NIN } else { 0 }
    );
    // A correction replaces its part, never adds twice.
    fx.attest(&fx.borrower, &id, 300 * NIN);
    assert_eq!(
        c.cash_room(&fx.union),
        1_500 * NIN + if near { 300 * NIN } else { 0 }
    );
    if near {
        fx.scan(Purpose::Invest, &fx.neighbour, 300 * NIN, 3, None)
            .unwrap();
        assert_eq!(f.cash_at_hand(&fx.union), 1_300 * NIN);
        assert_eq!(f.offers_of(&fx.union).len(), 0);
    }
    // Taken up: its part leaves the room again.
    fx.at(c.food_token(&fx.union, &id).unwrap().from_date);
    c.draw_loan(&fx.union, &fx.borrower, &id, &(300 * NIN), &0);
    assert_eq!(c.cash_room(&fx.union), c.union_treasury(&fx.union));
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
    assert!(loan(&c, &fx.union, &id).unwrap().closed);
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
    // Room for the scan below (31 Oct: cash at the office within `cash_room`).
    fx.scan(Purpose::Treasury, &fx.leader, NIN, 1, None)
        .unwrap();
    // No ₹10,000 per scan since 30 Oct (R76 is the leaders' agreement).
    fx.scan(Purpose::Invest, &fx.borrower, 10_000 * NIN + 1, 2, None)
        .unwrap();
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
    c.request_unbond(&fx.union, &fx.neighbour, &(400 * NIN), &false);
    fx.at(T0 + 14 * 86_400);
    c.claim(&fx.union, &fx.neighbour, &0, &false);
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
    c.request_unbond(&fx.union, &fx.neighbour, &(600 * NIN), &false);
    fx.at(T0 + 14 * 86_400);
    c.claim(&fx.union, &fx.neighbour, &0, &false);
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
fn fx_pool_slot_set_once() {
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
    c.request_unbond(&fx.union, &fx.neighbour, &(400 * NIN), &false);
    fx.at(T0 + 14 * 86_400);
    c.claim(&fx.union, &fx.neighbour, &0, &false);
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
        DataKey::ShareCash(a.clone(), 1),
        DataKey::Dues(a.clone(), h.clone()),
        DataKey::SeniorOwed(a.clone(), a.clone()),
        DataKey::SeniorIncome(a.clone()),
        DataKey::SeniorOwedTotal(a.clone()),
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
            senior: Some(a.clone()),
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
    // Appended 1 Nov (owner split): what a voucher's owners are still owed, beside the unchanged book.
    v.push_back(
        Dues {
            owners: soroban_sdk::vec![
                &env,
                OwnerDue {
                    owner: a.clone(),
                    amount: 1,
                }
            ],
            credit: 2,
            union_due: 3,
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
        hex, "44ec2c0939fe67cfc0393f29da1f11e89dfe3a21ee64e6f036ee11b566353936",
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

// ---------- C5a: senior route (2 Nov: the senior contract holds the USDC; nIN into the senior pool) ----------

use nila_senior::{Error as SError, SeniorContractClient, Tag};

impl Fx {
    fn senior(&self) -> SeniorContractClient<'_> {
        SeniorContractClient::new(&self.env, &self.senior_id)
    }
}

#[test]
fn senior_route_usdc_in_nin_into_the_senior_pool() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    let s = fx.senior();
    let investor = Address::generate(&fx.env);
    let usdc = StellarAssetClient::new(&fx.env, &fx.usdc);
    usdc.mint(&investor, &(1_000 * NIN));
    let kyc = loan_id(&fx.env, 0x4B);

    // Not accepted yet; no rate yet.
    assert_eq!(
        s.try_deposit(&investor, &fx.union, &(100 * NIN)),
        Err(Ok(SError::NotAccepted))
    );
    s.set_investor(&investor, &true, &kyc, &Tag::Offshore);
    assert_eq!(
        s.try_deposit(&investor, &fx.union, &(100 * NIN)),
        Err(Ok(SError::StaleRate))
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
    // At most one attester fix per 12 hours (2 Nov); the trust's override is not limited.
    assert_eq!(
        f.try_set_rate(&fx.attester, &rate, &(T0 + 86_400)),
        Err(Ok(FxError::RateTooSoon))
    );

    // Only the investor signs: the USDC transfer into the senior contract; the senior contract's call to
    // fx_pool and fx_pool's pull into fund_core are their own.
    let args = (investor.clone(), fx.union.clone(), 100 * NIN);
    fx.env.mock_auths(&[MockAuth {
        address: &investor,
        invoke: &MockAuthInvoke {
            contract: &fx.senior_id,
            fn_name: "deposit",
            args: args.into_val(&fx.env),
            sub_invokes: &[MockAuthInvoke {
                contract: &fx.usdc,
                fn_name: "transfer",
                args: (investor.clone(), fx.senior_id.clone(), 100 * NIN).into_val(&fx.env),
                sub_invokes: &[],
            }],
        },
    }]);
    assert_eq!(s.deposit(&investor, &fx.union, &(100 * NIN)), 9_500 * NIN);
    fx.env.mock_all_auths();
    assert_eq!(c.get_senior_market(&fx.union).cash, 9_500 * NIN);
    let tok = TokenClient::new(&fx.env, &fx.usdc);
    assert_eq!(tok.balance(&fx.senior_id), 100 * NIN);
    assert_eq!(tok.balance(&fx.fx_id), 0);
    assert_eq!(s.union_usdc(&fx.union, &Tag::Offshore), 100 * NIN);
    assert_eq!(s.union_usdc(&fx.union, &Tag::Domestic), 0);
    assert_eq!(s.total(), 100 * NIN);
    // The senior investor never holds nIN.
    assert_eq!(fx.token().balance(&investor), 0);
    fx.assert_fx_empty();
    fx.assert_conserved();
    // Only the senior contract mints senior nIN.
    let stranger = Address::generate(&fx.env);
    fx.env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &fx.fx_id,
            fn_name: "mint_senior_nin",
            args: (fx.union.clone(), investor.clone(), NIN).into_val(&fx.env),
            sub_invokes: &[],
        },
    }]);
    assert!(f.try_mint_senior_nin(&fx.union, &investor, &NIN).is_err());
    fx.env.mock_all_auths();
    assert_eq!(
        f.try_set_senior(&stranger),
        Err(Ok(FxError::SeniorAlreadySet))
    );
    assert_eq!(
        c.try_set_senior(&stranger),
        Err(Ok(Error::SeniorAlreadySet))
    );
    // A senior exit goes through the senior contract only.
    assert_eq!(
        c.try_claim(&fx.union, &investor, &0, &true),
        Err(Ok(Error::UseSeniorContract))
    );

    // A member cannot be a senior investor: fund_core refuses title holders, nothing moves.
    s.set_investor(&fx.borrower, &true, &kyc, &Tag::Domestic);
    usdc.mint(&fx.borrower, &(10 * NIN));
    assert!(s.try_deposit(&fx.borrower, &fx.union, &(10 * NIN)).is_err());
    assert_eq!(usdc.balance(&fx.borrower), 10 * NIN);
    // A union fund_core does not know: refused before any USDC moves.
    let nowhere = Address::generate(&fx.env);
    assert_eq!(
        s.try_deposit(&investor, &nowhere, &NIN),
        Err(Ok(SError::UnknownUnion))
    );
    assert_eq!(s.total(), 100 * NIN);
    // The cap (the contract is unaudited): above it, refused.
    s.set_cap(&(150 * NIN));
    assert_eq!(
        s.try_deposit(&investor, &fx.union, &(60 * NIN)),
        Err(Ok(SError::AboveCap))
    );
    // A dropped investor, a stale fix, a paused senior contract.
    s.set_investor(&investor, &false, &kyc, &Tag::Offshore);
    assert_eq!(
        s.try_deposit(&investor, &fx.union, &NIN),
        Err(Ok(SError::NotAccepted))
    );
    s.set_investor(&investor, &true, &kyc, &Tag::Offshore);
    fx.at(T0 + 86_400 + 1);
    assert_eq!(
        s.try_deposit(&investor, &fx.union, &NIN),
        Err(Ok(SError::StaleRate))
    );
    f.set_rate(&fx.attester, &rate, &(T0 + 2 * 86_400));
    s.set_paused(&true);
    assert_eq!(
        s.try_deposit(&investor, &fx.union, &NIN),
        Err(Ok(SError::Paused))
    );
    s.set_paused(&false);
    // fx_pool paused: no fix and no settle either (2 Nov).
    f.pause();
    assert!(s.try_deposit(&investor, &fx.union, &NIN).is_err());
    fx.at(T0 + 2 * 86_400);
    assert_eq!(
        f.try_set_rate(&fx.attester, &rate, &(T0 + 3 * 86_400)),
        Err(Ok(FxError::Paused))
    );
}

// ---------- LP positions (executive decision 31 Oct; was the 30 Oct pledge) ----------

use nila_account::{AccountContract, AccountContractClient, Signer as AcctSigner};
use nila_fx_pool::{OfferKind, OfferState};
use soroban_sdk::String as SString;

impl Fx {
    /// The LP: a Nila account that named the union (whose settings in roles name this fx_pool and USDC), with a
    /// title, accepted by the anchor; ₹95 fix; the union has cash at hand and 50 USDC of its own.
    fn lp_ready(&self) -> (Address, Address) {
        let f = self.fx();
        let anchor = Address::generate(&self.env);
        f.set_anchor(&anchor);
        let roles = self.core().get_config().roles;
        nila_roles::RolesContractClient::new(&self.env, &roles).set_union_settings(
            &self.union,
            &nila_roles::UnionSettings {
                wait: 3_600,
                session_len: 300,
                session_contracts: soroban_sdk::Vec::<Address>::new(&self.env),
                sms_key: None,
                fx_pool: Some(self.fx_id.clone()),
                usdc: Some(self.usdc.clone()),
            },
        );
        let lp = self.env.register(
            AccountContract,
            (
                roles,
                AcctSigner::Ed25519(BytesN::from_array(
                    &self.env,
                    &lp_key().verifying_key().to_bytes(),
                )),
            ),
        );
        AccountContractClient::new(&self.env, &lp).set_union(&self.union);
        self.give_title(&lp);
        f.set_id_checked(&lp, &true, &loan_id(&self.env, 0x4B));
        f.set_lp(&self.union, &lp, &true, &loan_id(&self.env, 0x4C));
        f.override_rate(&(95 * NIN), &(T0 + 86_400));
        self.scan(Purpose::Treasury, &self.leader, 5_000 * NIN, 1, None)
            .unwrap();
        self.scan(Purpose::Invest, &self.borrower, 2_000 * NIN, 2, None)
            .unwrap();
        (anchor, lp)
    }

    fn acct(&self, lp: &Address) -> AccountContractClient<'_> {
        AccountContractClient::new(&self.env, lp)
    }

    fn offer_of(&self, kind: OfferKind, inr: i128) -> u64 {
        self.fx()
            .post_offer(&self.leader, &self.union, &kind, &inr, &(T0 + 3_600))
    }

    fn ids(&self) -> (SString, SString) {
        (
            SString::from_str(&self.env, "sep24-tx-1"),
            SString::from_str(&self.env, "sep38-q-1"),
        )
    }
}

/// The anchor's swap key in the tests (C9b): signs each accept like nila-anchor does.
/// The LP account's own key (2 Nov: real signatures in the senior income test).
fn lp_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[5u8; 32])
}

fn swap_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[9u8; 32])
}

type TryAcc = Result<Result<i128, soroban_sdk::Error>, Result<FxError, soroban_sdk::InvokeError>>;

impl Fx {
    /// The anchor's signature for an accept at today's quote (0 USDC when there is none).
    fn signed(&self, lp: &Address, id: u64, s24: &SString, q: &SString) -> (i128, u64, BytesN<64>) {
        use ed25519_dalek::Signer;
        use soroban_sdk::xdr::ToXdr;
        let f = self.fx();
        let o = f.offer(&id).unwrap();
        // The USDC the accept moves for this LP: closed plus opened.
        let usdc = f
            .try_lp_quote(&o.union, lp, &o.kind, &o.inr)
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

/// A free LP takes the union's spare cash: its own USDC goes into fx_pool, held for it. A Bring of
/// the same LP closes it pro rata: the LP hands over the rupees less the 2% fee (the union's cost,
/// from its treasury) and gets that USDC back, free.
#[test]
fn collect_then_bring_by_the_same_lp() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    let usdc = TokenClient::new(&fx.env, &fx.usdc);
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    let (s24, q) = fx.ids();
    let pool = usdc.balance(&fx.fx_id);
    let own = f.treasury_usdc(&fx.union);

    let c = fx.offer_of(OfferKind::Collect, 950 * NIN);
    let lq = f.lp_quote(&fx.union, &lp, &OfferKind::Collect, &(950 * NIN));
    assert_eq!((lq.close_usdc, lq.open_usdc, lq.fee_inr), (0, 10 * NIN, 0));
    assert_eq!(fx.acc(&lp, &c, &s24, &q), 10 * NIN);
    assert_eq!(fx.acct(&lp).lien(), 10 * NIN);
    let hand = f.cash_at_hand(&fx.union);
    f.confirm(&fx.leader, &c);
    assert_eq!(usdc.balance(&lp), 40 * NIN);
    assert_eq!(usdc.balance(&fx.fx_id), pool + 10 * NIN);
    assert_eq!(fx.acct(&lp).lien(), 0);
    assert_eq!(f.lp_usdc(&fx.union), 10 * NIN);
    assert_eq!(f.treasury_usdc(&fx.union), own);
    assert_eq!(f.cash_at_hand(&fx.union), hand - 950 * NIN);
    let p = f.position(&fx.union, &lp).unwrap();
    assert_eq!(
        (p.kind, p.usdc, p.inr),
        (OfferKind::Collect, 10 * NIN, 950 * NIN)
    );

    // Half back: ₹475 less 2% in whole rupees (₹9) = ₹466 handed over; 5 USDC back, free.
    let treasury = fx.core().union_treasury(&fx.union);
    let b = fx.offer_of(OfferKind::Bring, 475 * NIN);
    let lq = f.lp_quote(&fx.union, &lp, &OfferKind::Bring, &(475 * NIN));
    assert_eq!(
        (lq.close_usdc, lq.open_usdc, lq.fee_inr),
        (5 * NIN, 0, 9 * NIN)
    );
    assert_eq!(fx.acc(&lp, &b, &s24, &q), 5 * NIN);
    let hand = f.cash_at_hand(&fx.union);
    f.confirm(&fx.leader, &b);
    assert_eq!(f.cash_at_hand(&fx.union), hand + 466 * NIN);
    assert_eq!(fx.core().union_treasury(&fx.union), treasury - 9 * NIN);
    assert_eq!(usdc.balance(&lp), 45 * NIN);
    assert_eq!(fx.acct(&lp).lien(), 0);
    let p = f.position(&fx.union, &lp).unwrap();
    assert_eq!((p.usdc, p.inr), (5 * NIN, 475 * NIN));

    // The rest closes the position.
    let b2 = fx.offer_of(OfferKind::Bring, 475 * NIN);
    fx.acc(&lp, &b2, &s24, &q);
    f.confirm(&fx.leader, &b2);
    assert_eq!(f.position(&fx.union, &lp), None);
    assert_eq!(f.lp_usdc(&fx.union), 0);
    assert_eq!(usdc.balance(&lp), 50 * NIN);
    assert_eq!(usdc.balance(&fx.fx_id), pool);
    assert_eq!(f.offers_of(&fx.union).len(), 0);
    // The LP never holds nIN.
    assert_eq!(fx.token().balance(&lp), 0);
    fx.assert_fx_empty();
    fx.assert_conserved();
}

/// A free LP brings cash: the union's own USDC goes to the LP's account, locked there (refused
/// above what the union has). A Collect of the same LP closes it: the union pays the rupees at
/// today's fix plus the 2% fee, and the USDC comes back to the union.
#[test]
fn bring_then_collect_by_the_same_lp() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    let usdc = TokenClient::new(&fx.env, &fx.usdc);
    let (s24, q) = fx.ids();
    let own = f.treasury_usdc(&fx.union);
    assert_eq!(own, 50 * NIN);

    // More than the union's own USDC: refused.
    let big = fx.offer_of(OfferKind::Bring, 5_700 * NIN);
    assert_eq!(fx.try_acc(&lp, &big, &s24, &q), Err(Ok(FxError::NoUsdc)));
    f.cancel_offer(&fx.leader, &big);

    let b = fx.offer_of(OfferKind::Bring, 950 * NIN);
    assert_eq!(fx.acc(&lp, &b, &s24, &q), 10 * NIN);
    assert_eq!(f.treasury_usdc(&fx.union), own - 10 * NIN);
    let hand = f.cash_at_hand(&fx.union);
    f.confirm(&fx.leader, &b);
    assert_eq!(f.cash_at_hand(&fx.union), hand + 950 * NIN);
    assert_eq!(usdc.balance(&lp), 10 * NIN);
    assert_eq!(fx.acct(&lp).lien(), 10 * NIN);
    let p = f.position(&fx.union, &lp).unwrap();
    assert_eq!(
        (p.kind, p.usdc, p.inr),
        (OfferKind::Bring, 10 * NIN, 950 * NIN)
    );

    // ₹969 closes it: ₹950 at the fix plus the ₹19 fee; the 10 USDC go back to the union.
    let c = fx.offer_of(OfferKind::Collect, 969 * NIN);
    let lq = f.lp_quote(&fx.union, &lp, &OfferKind::Collect, &(969 * NIN));
    assert_eq!(
        (lq.close_usdc, lq.close_inr, lq.fee_inr, lq.open_usdc),
        (10 * NIN, 950 * NIN, 19 * NIN, 0)
    );
    fx.acc(&lp, &c, &s24, &q);
    let hand = f.cash_at_hand(&fx.union);
    f.confirm(&fx.leader, &c);
    assert_eq!(f.cash_at_hand(&fx.union), hand - 969 * NIN);
    assert_eq!(usdc.balance(&lp), 0);
    assert_eq!(fx.acct(&lp).lien(), 0);
    assert_eq!(f.treasury_usdc(&fx.union), own);
    assert_eq!(f.position(&fx.union, &lp), None);
    fx.assert_conserved();
}

/// A Collect bigger than the LP's Bring position closes it and opens a Collect position with the
/// rest (the LP's own USDC).
#[test]
fn a_bigger_collect_closes_then_opens() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    let (s24, q) = fx.ids();
    let b = fx.offer_of(OfferKind::Bring, 950 * NIN);
    fx.acc(&lp, &b, &s24, &q);
    f.confirm(&fx.leader, &b);
    // ₹1,919: ₹950 closes (fee ₹19), ₹950 opens 10 USDC of its own.
    let c = fx.offer_of(OfferKind::Collect, 1_919 * NIN);
    let lq = f.lp_quote(&fx.union, &lp, &OfferKind::Collect, &(1_919 * NIN));
    assert_eq!(
        (lq.close_usdc, lq.fee_inr, lq.open_usdc, lq.open_inr),
        (10 * NIN, 19 * NIN, 10 * NIN, 950 * NIN)
    );
    fx.acc(&lp, &c, &s24, &q);
    assert_eq!(fx.acct(&lp).lien(), 20 * NIN);
    f.confirm(&fx.leader, &c);
    assert_eq!(fx.acct(&lp).lien(), 0);
    assert_eq!(TokenClient::new(&fx.env, &fx.usdc).balance(&lp), 40 * NIN);
    let p = f.position(&fx.union, &lp).unwrap();
    assert_eq!(
        (p.kind, p.usdc, p.inr),
        (OfferKind::Collect, 10 * NIN, 950 * NIN)
    );
    assert_eq!(f.lp_usdc(&fx.union), 10 * NIN);
}

/// Past the 5% margin anyone settles: a Collect position's USDC becomes the union's, a Bring
/// position's unlocks for the LP; no cash moves.
#[test]
fn past_the_margin_anyone_settles() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    let (s24, q) = fx.ids();
    let c = fx.offer_of(OfferKind::Collect, 950 * NIN);
    fx.acc(&lp, &c, &s24, &q);
    f.confirm(&fx.leader, &c);
    let own = f.treasury_usdc(&fx.union);
    f.override_rate(&(99 * NIN), &(T0 + 86_400));
    assert_eq!(
        f.try_settle_position(&fx.union, &lp),
        Err(Ok(FxError::InMargin))
    );
    f.override_rate(&(100 * NIN), &(T0 + 86_400));
    fx.env.set_auths(&[]);
    assert_eq!(f.settle_position(&fx.union, &lp), 10 * NIN);
    fx.env.mock_all_auths();
    assert_eq!(f.position(&fx.union, &lp), None);
    assert_eq!(f.lp_usdc(&fx.union), 0);
    assert_eq!(f.treasury_usdc(&fx.union), own + 10 * NIN);

    // A Bring position at ₹100, settled when the rupee gains past 5%: the LP keeps its USDC.
    let b = fx.offer_of(OfferKind::Bring, 1_000 * NIN);
    fx.acc(&lp, &b, &s24, &q);
    f.confirm(&fx.leader, &b);
    assert_eq!(fx.acct(&lp).lien(), 10 * NIN);
    f.override_rate(&(94 * NIN), &(T0 + 86_400));
    f.settle_position(&fx.union, &lp);
    assert_eq!(fx.acct(&lp).lien(), 0);
    assert_eq!(TokenClient::new(&fx.env, &fx.usdc).balance(&lp), 50 * NIN);
    assert_eq!(
        f.try_settle_position(&fx.union, &lp),
        Err(Ok(FxError::NoPosition))
    );
}

#[test]
fn quote_matches_what_accept_fixes() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    assert_eq!(f.lp_fee(), 100);
    // C9: SEP-38 reads the rupees' USDC at the fix and the close fee (2%, whole rupees).
    let qc = f.quote(&fx.union, &OfferKind::Collect, &(950 * NIN));
    assert_eq!((qc.usdc, qc.fee_inr, qc.fee_bp), (10 * NIN, 19 * NIN, 200));
    assert_eq!((qc.inr_per_usd, qc.valid_until), (95 * NIN, T0 + 86_400));
    f.set_lp_fee(&150);
    assert_eq!(
        f.quote(&fx.union, &OfferKind::Bring, &(950 * NIN)).fee_inr,
        28 * NIN
    );
    assert_eq!(
        f.try_quote(&fx.union, &OfferKind::Bring, &0),
        Err(Ok(FxError::AmountZero))
    );
    // After the fix expires there is no quote, and no accept.
    fx.at(T0 + 86_401);
    assert_eq!(
        f.try_lp_quote(&fx.union, &lp, &OfferKind::Collect, &(950 * NIN)),
        Err(Ok(FxError::StaleOracle))
    );
}

#[test]
fn an_accept_not_scanned_gives_back_what_it_reserved() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    let (s24, q) = fx.ids();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(50 * NIN));
    // A Collect accepted, then cancelled: the lock goes. One accepted offer at a time.
    let c = fx.offer_of(OfferKind::Collect, 950 * NIN);
    fx.acc(&lp, &c, &s24, &q);
    assert_eq!(fx.acct(&lp).lien(), 10 * NIN);
    let b = fx.offer_of(OfferKind::Bring, 95 * NIN);
    assert_eq!(fx.try_acc(&lp, &b, &s24, &q), Err(Ok(FxError::OfferState)));
    f.cancel_offer(&fx.leader, &c);
    assert_eq!(fx.acct(&lp).lien(), 0);
    // A Bring accepted, not brought before it ran out: the union's USDC comes back.
    let own = f.treasury_usdc(&fx.union);
    fx.acc(&lp, &b, &s24, &q);
    assert_eq!(f.treasury_usdc(&fx.union), own - NIN);
    assert_eq!(f.try_expire_offer(&b), Err(Ok(FxError::NotExpired)));
    fx.at(T0 + 3_600);
    assert_eq!(f.try_confirm(&fx.leader, &b), Err(Ok(FxError::OfferState)));
    fx.env.set_auths(&[]);
    f.expire_offer(&b);
    fx.env.mock_all_auths();
    assert_eq!(f.treasury_usdc(&fx.union), own);
    assert_eq!(f.offer(&b).unwrap().state, OfferState::Expired);
    assert_eq!(f.position(&fx.union, &lp), None);
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
    let c = fx.offer_of(OfferKind::Collect, 95 * NIN);
    assert_eq!(
        fx.try_acc(&fx.borrower, &c, &s24, &q),
        Err(Ok(FxError::NotLp))
    );
    // Only a leader posts and confirms.
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
    // No fixed period (31 Oct): open until accepted or cancelled; only a time already past is refused.
    assert_eq!(
        f.try_post_offer(&fx.leader, &fx.union, &OfferKind::Bring, &NIN, &T0),
        Err(Ok(FxError::BadExpiry))
    );
    let open = f.post_offer(&fx.leader, &fx.union, &OfferKind::Bring, &NIN, &u64::MAX);
    assert_eq!(f.offer(&open).unwrap().expires, u64::MAX);
    // A dropped LP cannot accept.
    f.set_lp(&fx.union, &lp, &false, &loan_id(&fx.env, 1));
    assert_eq!(fx.try_acc(&lp, &c, &s24, &q), Err(Ok(FxError::NotLp)));
}

#[test]
fn a_collect_locks_only_free_usdc_with_the_lps_signature_alone() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, lp) = fx.lp_ready();
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(15 * NIN));
    let (s24, q) = fx.ids();
    let c = fx.offer_of(OfferKind::Collect, 950 * NIN);
    let (usdc, until, sig) = fx.signed(&lp, c, &s24, &q);
    // One signature, on accept, with nothing under it: the lock and the pay are fx_pool's own
    // calls into the account (no approve, no transfer by the LP).
    f.accept(&lp, &c, &s24, &q, &usdc, &until, &sig);
    let auths = fx.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, lp);
    assert!(auths[0].1.sub_invocations.is_empty());
    fx.env.mock_all_auths();
    assert_eq!(
        TokenClient::new(&fx.env, &fx.usdc).allowance(&lp, &fx.fx_id),
        0
    );
    f.confirm(&fx.leader, &c);
    // 5 USDC left: a second Collect of ₹950 (10 USDC) is refused by the account.
    let c2 = fx.offer_of(OfferKind::Collect, 950 * NIN);
    assert!(fx.try_acc(&lp, &c2, &s24, &q).is_err());
    assert_eq!(fx.acct(&lp).lien(), 0);
    // Only fx_pool may lock, unlock or take.
    fx.env.set_auths(&[]);
    assert!(fx.acct(&lp).try_unlock(&NIN).is_err());
    assert!(fx.acct(&lp).try_lock(&NIN).is_err());
    assert!(fx.acct(&lp).try_pay(&NIN).is_err());
    fx.env.mock_all_auths();
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
    // The senior investor: 200 USDC at ₹50 = 10,000 nIN in the senior pool.
    investor: Address,
}

impl Vx {
    fn mach(&self) -> MachineryContractClient<'_> {
        MachineryContractClient::new(&self.fx.env, &self.m)
    }

    /// fund_core nIN = pools + treasury + rainy + the voucher reserve.
    fn assert_conserved(&self) {
        let c = self.fx.core();
        let u = &self.fx.union;
        let books = c.get_junior_market(u).cash
            + c.get_senior_market(u).cash
            + c.union_treasury(u)
            + c.union_book(u).rainy
            + c.senior_earmark(u)
            + c.senior_owed_total(u);
        assert_eq!(self.fx.token().balance(&self.fx.core_id), books);
    }
}

/// Machinery wired in, a ₹1,450/h tractor rate, a machine owned by the neighbour (available from its registration),
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
    // Every part to the owner here (mask 0); the union's parts (2 Nov) have their own tests.
    mc.set_union_parts(&fx.union, &0);
    let owner = fx.neighbour.clone();
    let machine = mc.register_machine(&fx.leader, &fx.union, &owner, &1, &40, &loan_id(env, 0x50));
    // No owner confirmation (3 Nov): available at once.
    assert!(mc.machine(&machine).unwrap().confirmed);
    // Senior money for the prepay (N4).
    let f = fx.fx();
    let investor = Address::generate(env);
    StellarAssetClient::new(env, &fx.usdc).mint(&investor, &(200 * NIN));
    fx.senior()
        .set_investor(&investor, &true, &loan_id(env, 0x49), &Tag::Offshore);
    f.override_rate(&(50 * NIN), &(T0 + 86_400));
    fx.senior().deposit(&investor, &fx.union, &(200 * NIN));
    fx.invest(&fx.borrower, 2_000 * NIN);
    Vx {
        fx,
        m,
        owner,
        machine,
        investor,
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
    assert_eq!(mc.voucher(&fx.union, &id).unwrap().units, 600);
    // Her lock counts the voucher's value too (R27): she owes more than her 2,000.
    let p = c.position(&fx.union, &fx.borrower);
    assert_eq!(p.locked, p.invested);
    v.assert_conserved();

    // A8a: she signs for up to 6 hours; the owner works 4: the prepay now, the rest postpaid (owner
    // split, 1 Nov), held for the owner out of the reserve.
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &600, &400);
    use soroban_sdk::{testutils::AuthorizedFunction, IntoVal, Symbol};
    let signed: soroban_sdk::Vec<soroban_sdk::Val> =
        (fx.union.clone(), id.clone(), 600u32).into_val(&fx.env);
    assert!(fx.env.auths().iter().any(|(a, inv)| *a == fx.borrower
        && matches!(&inv.function, AuthorizedFunction::Contract((c, f, args))
            if *c == mc.address && *f == Symbol::new(&fx.env, "claim") && *args == signed)));
    assert_eq!(fx.token().balance(&v.owner), 4 * PRE);
    assert_eq!(c.senior_earmark(&fx.union), 2 * HOUR + 4 * PEND);
    let d = c.owner_dues(&fx.union, &id);
    assert_eq!(
        (d.owners.len(), d.owners.get(0).unwrap().amount),
        (1, 4 * PEND)
    );
    assert_eq!(c.voucher_book(&fx.union, &id).unwrap().reserve, 2 * HOUR);
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
    // Her repayment settled the owner's postpaid part into the owner's balance; she collects it.
    assert!(c.owner_dues(&fx.union, &id).owners.is_empty());
    assert_eq!(c.owner_balance(&fx.union, &v.owner), 4 * PEND);
    assert_eq!(c.collect(&fx.union, &v.owner), 4 * PEND);
    assert_eq!(fx.token().balance(&v.owner), 4 * HOUR);
    assert_eq!(
        c.try_collect(&fx.union, &v.owner),
        Err(Ok(Error::NothingToClaim))
    );
    // Closed, and her 2 unused hours stay hers, with their reserve.
    assert_eq!(c.senior_earmark(&fx.union), 2 * HOUR);
    assert!(!mc.voucher(&fx.union, &id).unwrap().void);
    v.assert_conserved();
    // She uses them after the loan is repaid: the owner is paid from the reserve, all of it at once.
    fx.at(T0 + YEAR / 2 + 8 * 86_400);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &200, &200);
    assert_eq!(fx.token().balance(&v.owner), 6 * HOUR);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    v.assert_conserved();
}

/// Executive decision 27 Oct: more machine hours on a share already taken up (draw_loan again, cash 0): on the
/// same voucher, owed from then, the earlier hours' interest accrued first; more cash is refused.
#[test]
fn hours_can_be_added_to_a_share_taken_up() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &(400 * NIN), &0);
    c.draw_loan(&fx.union, &fx.borrower, &id, &(400 * NIN), &0);
    assert!(c.voucher_book(&fx.union, &id).is_none());
    // A second draw with cash, or with nothing, is refused.
    assert!(c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &(100 * NIN), &0)
        .is_err());
    assert!(c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &(100 * NIN), &200)
        .is_err());
    assert!(c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &0, &0)
        .is_err());
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &300);
    assert_eq!(mc.voucher(&fx.union, &id).unwrap().units, 300);
    assert_eq!(c.senior_earmark(&fx.union), 3 * HOUR);
    v.assert_conserved();
    // A month on, 2 more hours: the same voucher, the first 3 hours' interest accrued to then.
    fx.at(T0 + 30 * 86_400);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &200);
    assert_eq!(mc.voucher(&fx.union, &id).unwrap().units, 500);
    let b = c.voucher_book(&fx.union, &id).unwrap();
    assert_eq!((b.hours, b.value, b.reserve), (500, 5 * HOUR, 5 * HOUR));
    assert!(b.interest_accrued > 0);
    assert_eq!(c.senior_earmark(&fx.union), 5 * HOUR);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(
        (sm.cash, sm.total_borrows),
        (10_000 * NIN - 5 * HOUR, 5 * HOUR)
    );
    v.assert_conserved();
}

/// A crop share's token id as FoodToken packs it: crop code at bit 204, area in m² at bit 148; `tag` in the low byte.
fn share_token(env: &Env, crop_code: u64, m2: u64, tag: u8) -> BytesN<32> {
    let mut b = [0u8; 32];
    b[31] = tag;
    for (shift, width, v) in [(204u32, 20u32, crop_code), (148, 24, m2)] {
        for i in 0..width {
            if (v >> i) & 1 == 1 {
                let bit = shift + i;
                b[31 - (bit / 8) as usize] |= 1 << (bit % 8);
            }
        }
    }
    BytesN::from_array(env, &b)
}

/// Executive decision 27 Oct: vouchers count acres; a claim never exceeds the crop share's own area (read from its
/// token id), and the union's price for the crop (trailer × sugarcane) beats the type's price.
#[test]
fn acres_are_capped_by_the_shares_area_and_priced_by_its_crop() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = share_token(&fx.env, 2000, 4046, 1); // sugarcane (family 2), one acre: 100 centi acres
    assert_eq!(mc.share_of(&id), (2, 100));
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(400 * NIN),
        &600,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(400 * NIN), &600); // 6 tractor acres
                                                                   // More than the share's area is refused, whatever she signed.
    assert!(mc
        .try_claim(&v.owner, &v.machine, &fx.union, &id, &600, &101)
        .is_err());
    // The union prices tractor work on sugarcane at twice the acre: one acre uses two tractor acres.
    mc.set_crop_rate(&fx.union, &1, &2, &Some(tractor(2 * NIN)));
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &600, &100);
    assert_eq!(mc.voucher(&fx.union, &id).unwrap().used, 200);
    // The prepay of the crop's price now (diesel ₹1,000 + half the driver's ₹800); the rest postpaid.
    assert_eq!(fx.token().balance(&v.owner), 2 * PRE);
    assert_eq!(
        c.owner_dues(&fx.union, &id).owners.get(0).unwrap().amount,
        2 * PEND
    );
    v.assert_conserved();
    // Removing the crop's price brings the type's back.
    mc.set_crop_rate(&fx.union, &1, &2, &None);
    assert_eq!(mc.rate_for_crop(&fx.union, &1, &2), mc.rate(&fx.union, &1));
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
        Err(Ok(ME::TooMuchWork))
    );
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &1500, &1500),
        Err(Ok(ME::TooMuchWork))
    );
    let small = mc.register_machine(&fx.leader, &fx.union, &v.owner, &1, &3, &id);
    // confirm_machine is a no-op kept for old clients: the owner only, nothing changes.
    mc.confirm_machine(&v.owner, &small);
    assert_eq!(
        mc.try_confirm_machine(&fx.borrower, &small),
        Err(Ok(ME::NotOwner))
    );
    assert_eq!(
        mc.try_claim(&v.owner, &small, &fx.union, &id, &1000, &400),
        Err(Ok(ME::TooMuchWork))
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
    // The owner's postpaid part was settled by the default (the guarantee); she collects it.
    c.collect(&fx.union, &v.owner);
    // Her investment covers it all: the voucher's value and interest to senior, junior in full.
    assert_eq!(fx.token().balance(&v.owner), 2 * HOUR);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(sm.total_borrows, 0);
    assert!(sm.cash > 10_000 * NIN);
    assert!(mc.voucher(&fx.union, &id).unwrap().void);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    v.assert_conserved();
}

/// The union's parts of the price (executive decision 2 Nov): one 4-acre job under `mask`, then the
/// loan repaid in full (`default` false) or defaulted. Returns (treasury change, owner paid in all,
/// senior cash after, union due right after the claim).
fn union_parts_run(mask: u32, default: bool) -> (i128, i128, i128, i128) {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    mc.set_union_parts(&fx.union, &mask);
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 3_000 * NIN);
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(300 * NIN),
        &400,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(300 * NIN), &400);
    let t0 = c.union_treasury(&fx.union);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &400, &400);
    let due = c.owner_dues(&fx.union, &id).union_due;
    v.assert_conserved();
    if default {
        c.report_maturity(&fx.oracle, &fx.union, &id, &T0);
        fx.at(T0 + 111 * 86_400 + 1);
        c.mark_default(&fx.union, &id);
    } else {
        fx.at(T0 + YEAR / 2);
        StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(20_000 * NIN));
        assert!(c.repay_loan(&fx.union, &id, &(20_000 * NIN)));
    }
    let d = c.owner_dues(&fx.union, &id);
    assert_eq!((d.owners.len(), d.union_due), (0, 0));
    // Every nIN senior holds or lent is its investor's (the union's parts are not counted twice).
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(
        c.senior_position(&fx.union, &v.investor).value,
        sm.cash + sm.total_borrows
    );
    if c.owner_balance(&fx.union, &v.owner) > 0 {
        c.collect(&fx.union, &v.owner);
    }
    assert_eq!(c.senior_earmark(&fx.union), 0);
    v.assert_conserved();
    (
        c.union_treasury(&fx.union) - t0,
        fx.token().balance(&v.owner),
        c.get_senior_market(&fx.union).cash,
        due,
    )
}

#[test]
fn union_parts_go_to_the_treasury_as_she_repays_and_to_senior_at_a_default() {
    use nila_machinery::{PART_DEPRECIATION, PART_INSURANCE, PART_MAINTENANCE, PART_MARGIN};
    // Default (unset): depreciation and margin are the union's: ₹250 of the ₹1,450 acre.
    let v = vsetup();
    v.mach()
        .set_union_parts(&v.fx.union, &(PART_DEPRECIATION | PART_MARGIN));
    assert_eq!(
        v.mach().union_parts(&Address::generate(&v.fx.env)),
        PART_DEPRECIATION | PART_MARGIN
    );
    // Driver and diesel are always the owner's.
    assert_eq!(
        v.mach().try_set_union_parts(&v.fx.union, &1),
        Err(Ok(nila_machinery::Error::BadArg))
    );
    let dep = 4 * 250 * NIN;
    let (t_all, o_all, _, d_all) = union_parts_run(0, false);
    let (t_dep, o_dep, _, d_dep) = union_parts_run(PART_DEPRECIATION | PART_MARGIN, false);
    // Repaid: the owner gets ₹250 an acre less, the treasury the same more; the prepay is unchanged.
    assert_eq!((d_all, d_dep), (0, dep));
    assert_eq!((o_all - o_dep, t_dep - t_all), (dep, dep));
    // A union taking maintenance, insurance and depreciation too: ₹550 an acre.
    let all4 = PART_MAINTENANCE | PART_INSURANCE | PART_DEPRECIATION | PART_MARGIN;
    let (t_4, o_4, _, d_4) = union_parts_run(all4, false);
    assert_eq!(
        (d_4, o_all - o_4, t_4 - t_all),
        (4 * 550 * NIN, 4 * 550 * NIN, 4 * 550 * NIN)
    );
    assert_eq!(o_4, 4 * PRE + 4 * 200 * NIN);
    // Defaulted: senior pays the owner's part (the guarantee) but keeps the union's: the union's loss.
    let (td_all, od_all, sd_all, _) = union_parts_run(0, true);
    let (td_dep, od_dep, sd_dep, _) = union_parts_run(PART_DEPRECIATION | PART_MARGIN, true);
    assert_eq!(
        (od_all - od_dep, sd_dep - sd_all, td_dep - td_all),
        (dep, dep, 0)
    );
    // A bigger union part at a default: the owner still gets only the owner's part, and senior's books
    // still add up (checked in the run).
    let (_, od_4, _, _) = union_parts_run(all4, true);
    assert_eq!(od_all - od_4, 4 * 550 * NIN);
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
    // ₹300 draw), then to her junior principal; senior loses the rest. The owner's postpaid part was
    // settled by the default; she collects it.
    c.collect(&fx.union, &v.owner);
    assert_eq!(fx.token().balance(&v.owner), 2 * HOUR);
    assert!(mc.voucher(&fx.union, &id).unwrap().void);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(sm.total_borrows, 0);
    assert_eq!(sm.cash, 10_000 * NIN - 2 * HOUR + 1_700 * NIN);
    assert_eq!(c.position(&fx.union, &fx.borrower).invested, 0);
    v.assert_conserved();
}

/// Senior exit (executive decision 2 Nov): seniors hold USD. Principal back at par in USDC from the senior
/// contract after the 14-day unbond; the voucher interest senior earned stays owed in fund_core until a
/// member LP takes those rupees at the office and pays the investor USDC. No USDC of the senior contract
/// moves except at the exit.
#[test]
fn senior_exits_at_par_and_its_interest_comes_in_usdc_through_an_lp() {
    let v = vsetup();
    let (fx, c, s) = (&v.fx, v.fx.core(), v.fx.senior());
    let (_, lp) = fx.lp_ready();
    let tok = TokenClient::new(&fx.env, &fx.usdc);
    assert_eq!(tok.balance(&fx.senior_id), 200 * NIN);
    assert_eq!(s.principal(&fx.union, &v.investor).usdc, 200 * NIN);
    // A voucher loan repaid half a year on: its interest goes to senior and is counted as senior income.
    let id = loan_id(&fx.env, 1);
    fx.invest(&fx.borrower, 3_000 * NIN);
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(300 * NIN),
        &400,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(300 * NIN), &400);
    fx.at(T0 + YEAR / 2);
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(20_000 * NIN));
    assert!(c.repay_loan(&fx.union, &id, &(20_000 * NIN)));
    let v_int = 4 * HOUR * 1250 / 10_000 / 2;
    assert_eq!(c.senior_income(&fx.union), v_int);
    let p = c.senior_position(&fx.union, &v.investor);
    assert_eq!((p.cost, p.value), (10_000 * NIN, 10_000 * NIN + v_int));
    // The exit: unbond, then 14 days; not before. fund_core's error is its own, never read as one of
    // the senior contract's (their codes start at 101).
    c.request_unbond(&fx.union, &v.investor, &p.shares, &true);
    assert!(matches!(
        s.try_redeem(&v.investor, &fx.union, &0),
        Err(Err(_))
    ));
    fx.at(T0 + YEAR / 2 + 14 * 86_400);
    assert_eq!(s.redeem(&v.investor, &fx.union, &0), 200 * NIN);
    assert_eq!(tok.balance(&v.investor), 200 * NIN);
    assert_eq!((tok.balance(&fx.senior_id), s.total()), (0, 0));
    assert_eq!(s.union_usdc(&fx.union, &Tag::Offshore), 0);
    assert_eq!(s.principal(&fx.union, &v.investor).nin, 0);
    // The interest stays owed in rupees; it no longer counts as income still in the pool.
    let owed = c.senior_position(&fx.union, &v.investor).owed;
    assert_eq!((owed, c.senior_income(&fx.union)), (v_int, 0));
    assert_eq!(c.senior_owed_total(&fx.union), v_int);
    assert_eq!(fx.token().balance(&v.investor), 0);
    v.assert_conserved();

    // An LP of the union takes the rupees and pays USDC at the fix less its fee; a leader co-signs.
    fx.fx()
        .override_rate(&(95 * NIN), &(T0 + YEAR / 2 + 15 * 86_400));
    let fair = owed * 10_000_000 / (95 * NIN);
    let pay = fair * i128::from(10_000 - fx.fx().lp_fee()) / 10_000;
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(pay + 50 * NIN));
    // The LP has an open lien (a Bring position): it pays from the free part through its account.
    fx.acct(&lp).lock(&(50 * NIN));
    let lp_usdc = tok.balance(&lp);
    assert_eq!(
        s.try_pay_income(&fx.union, &v.investor, &fx.leader, &lp, &owed, &(pay - 1)),
        Err(Ok(SError::BelowFix))
    );
    assert_eq!(
        s.try_pay_income(&fx.union, &v.investor, &fx.borrower, &lp, &owed, &pay),
        Err(Ok(SError::NotLeader))
    );
    let stranger = Address::generate(&fx.env);
    assert_eq!(
        s.try_pay_income(&fx.union, &v.investor, &fx.leader, &stranger, &owed, &pay),
        Err(Ok(SError::NotLp))
    );
    // Never more than owed.
    assert!(s
        .try_pay_income(
            &fx.union,
            &v.investor,
            &fx.leader,
            &lp,
            &(owed + 1),
            &(pay + 1)
        )
        .is_err());
    // Its own key signs: the account allows paying through `send_usdc` while the lien stands, and still
    // refuses a direct USDC transfer.
    {
        use ed25519_dalek::Signer as _;
        use nila_account::{Error as AcctError, Signature as AcctSig};
        use soroban_sdk::auth::{Context, ContractContext};
        let payload = [3u8; 32];
        let sig = AcctSig::Ed25519(BytesN::from_array(
            &fx.env,
            &lp_key().sign(&payload).to_bytes(),
        ));
        let ctx = |contract: &Address, f: &str| {
            soroban_sdk::vec![
                &fx.env,
                Context::Contract(ContractContext {
                    contract: contract.clone(),
                    fn_name: soroban_sdk::Symbol::new(&fx.env, f),
                    args: soroban_sdk::vec![&fx.env],
                })
            ]
        };
        let mut paying = ctx(&fx.senior_id, "pay_income");
        paying.append(&ctx(&lp, "send_usdc"));
        let check = |c: &soroban_sdk::Vec<Context>| {
            fx.env.try_invoke_contract_check_auth::<AcctError>(
                &lp,
                &BytesN::from_array(&fx.env, &payload),
                sig.clone().into_val(&fx.env),
                c,
            )
        };
        assert_eq!(check(&paying), Ok(()));
        assert_eq!(
            check(&ctx(&fx.usdc, "transfer")),
            Err(Ok(AcctError::Locked))
        );
    }
    // A fake LP account that pays nothing is refused on the investor's balance (re-review, 3 Nov).
    let fake = fx.env.register(fake_lp::FakeLp, ());
    fx.give_title(&fake);
    fx.fx()
        .set_id_checked(&fake, &true, &loan_id(&fx.env, 0x51));
    fx.fx()
        .set_lp(&fx.union, &fake, &true, &loan_id(&fx.env, 0x52));
    assert_eq!(
        s.try_pay_income(&fx.union, &v.investor, &fx.leader, &fake, &owed, &pay),
        Err(Ok(SError::NotPaid))
    );
    assert_eq!(c.senior_position(&fx.union, &v.investor).owed, owed);
    s.pay_income(&fx.union, &v.investor, &fx.leader, &lp, &owed, &pay);
    assert_eq!(c.senior_position(&fx.union, &v.investor).owed, 0);
    assert_eq!(c.senior_owed_total(&fx.union), 0);
    assert_eq!(tok.balance(&v.investor), 200 * NIN + pay);
    assert_eq!(tok.balance(&lp), lp_usdc - pay);
    assert_eq!(tok.balance(&fx.senior_id), 0);
    v.assert_conserved();
    // fund_core's own senior exit is closed: only the senior contract pays it.
    assert_eq!(
        c.try_claim(&fx.union, &v.investor, &0, &true),
        Err(Ok(Error::UseSeniorContract))
    );
}

/// A senior loss (a default the junior side could not cover) is a haircut: the exit pays the USDC pro rata
/// to the principal fund_core still holds; the rest stays in the senior contract.
#[test]
fn a_senior_haircut_pays_less_and_the_rest_stays_in_the_senior_contract() {
    let v = vsetup();
    let (fx, c, mc, s) = (&v.fx, v.fx.core(), v.mach(), v.fx.senior());
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
    // Senior lost ₹1,200 of its ₹10,000 (see the default test above).
    let p = c.senior_position(&fx.union, &v.investor);
    assert_eq!(p.value, 8_800 * NIN);
    c.request_unbond(&fx.union, &v.investor, &p.shares, &true);
    fx.at(T0 + 125 * 86_400 + 1);
    let tok = TokenClient::new(&fx.env, &fx.usdc);
    assert_eq!(s.redeem(&v.investor, &fx.union, &0), 176 * NIN);
    assert_eq!(tok.balance(&v.investor), 176 * NIN);
    // The 24 USDC left stays here, counted apart: out of the total (the cap), into the union's haircut.
    assert_eq!(tok.balance(&fx.senior_id), 24 * NIN);
    assert_eq!(s.total(), 0);
    assert_eq!(s.haircut_usdc(&fx.union), 24 * NIN);
    assert_eq!(s.union_usdc(&fx.union, &Tag::Offshore), 0);
    assert_eq!(s.principal(&fx.union, &v.investor).usdc, 0);
    assert_eq!(c.senior_position(&fx.union, &v.investor).owed, 0);
    // Nothing else takes it out: a second exit has nothing to claim.
    assert!(s.try_redeem(&v.investor, &fx.union, &0).is_err());
    assert_eq!(tok.balance(&fx.senior_id), 24 * NIN);
}

/// The vault (nUSD, later) assigns guarantee with USDC it holds itself: senior nIN in its name, no USDC
/// moves in the senior contract; only the vault may.
#[test]
fn only_the_vault_mints_guaranteed_senior_nin_and_no_usdc_moves() {
    let v = vsetup();
    let (fx, c, s) = (&v.fx, v.fx.core(), v.fx.senior());
    let vault = Address::generate(&fx.env);
    s.set_vault(&vault);
    let tok = TokenClient::new(&fx.env, &fx.usdc);
    let before = (
        tok.balance(&fx.senior_id),
        c.get_senior_market(&fx.union).cash,
    );
    let stranger = Address::generate(&fx.env);
    fx.env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &fx.senior_id,
            fn_name: "mint_guaranteed",
            args: (fx.union.clone(), 10 * NIN).into_val(&fx.env),
            sub_invokes: &[],
        },
    }]);
    assert!(s.try_mint_guaranteed(&fx.union, &(10 * NIN)).is_err());
    fx.env.mock_all_auths();
    // Capped by the trust (0 until nUSD is audited).
    assert_eq!(
        s.try_mint_guaranteed(&fx.union, &(10 * NIN)),
        Err(Ok(SError::AboveGuaranteeCap))
    );
    s.set_guarantee_cap(&(15 * NIN));
    assert_eq!(s.mint_guaranteed(&fx.union, &(10 * NIN)), 500 * NIN);
    assert!(fx.env.auths().iter().any(|(a, _)| *a == vault));
    assert_eq!(tok.balance(&fx.senior_id), before.0);
    assert_eq!(c.get_senior_market(&fx.union).cash, before.1 + 500 * NIN);
    assert_eq!(c.senior_position(&fx.union, &vault).cost, 500 * NIN);
    assert_eq!(s.guaranteed(), 10 * NIN);
    assert_eq!(
        s.try_mint_guaranteed(&fx.union, &(6 * NIN)),
        Err(Ok(SError::AboveGuaranteeCap))
    );
    v.assert_conserved();
}

/// Owner split (1 Nov): her repayment of the voucher's value pays the owners' postpaid parts first,
/// oldest claim first; repaid value not yet needed waits as credit and pays later work at once.
#[test]
fn owners_are_paid_the_rest_from_her_repayment_oldest_claim_first() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let second = Address::generate(&fx.env);
    fx.give_title(&second);
    let m2 = mc.register_machine(
        &fx.leader,
        &fx.union,
        &second,
        &1,
        &40,
        &loan_id(&fx.env, 0x51),
    );
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &600);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &600);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &200, &200);
    mc.claim(&second, &m2, &fx.union, &id, &200, &200);
    assert_eq!(fx.token().balance(&v.owner), 2 * PRE);
    assert_eq!(fx.token().balance(&second), 2 * PRE);
    v.assert_conserved();

    // She brings ₹2,000 the same day (no interest yet): it is the voucher's value, so it settles the
    // first owner's ₹1,500 and ₹500 of the second's into their balances.
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(2_000 * NIN));
    assert!(!c.repay_loan(&fx.union, &id, &(2_000 * NIN)));
    assert_eq!(c.owner_balance(&fx.union, &v.owner), 2 * PEND);
    assert_eq!(c.owner_balance(&fx.union, &second), 500 * NIN);
    let d = c.owner_dues(&fx.union, &id);
    assert_eq!((d.owners.len(), d.credit), (1, 0));
    assert_eq!(d.owners.get(0).unwrap().amount, 1_000 * NIN);
    // Senior got its money back as it came in; what owners are owed stays under the earmark until
    // they collect it.
    assert_eq!(
        c.senior_earmark(&fx.union),
        2 * HOUR + 2 * PEND + 1_500 * NIN
    );
    c.collect(&fx.union, &v.owner);
    assert_eq!(fx.token().balance(&v.owner), 2 * HOUR);
    v.assert_conserved();

    // ₹4,000 more: ₹1,000 to the second owner, ₹3,000 waits as credit for work not done yet.
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(4_000 * NIN));
    assert!(!c.repay_loan(&fx.union, &id, &(4_000 * NIN)));
    assert_eq!(c.collect(&fx.union, &second), 2 * PEND);
    assert_eq!(fx.token().balance(&second), 2 * HOUR);
    assert_eq!(c.owner_dues(&fx.union, &id).credit, 3_000 * NIN);
    // The first owner works her last 2 hours: prepay, and the postpaid part at once from that credit.
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &200, &200);
    assert_eq!(fx.token().balance(&v.owner), 4 * HOUR);
    let d = c.owner_dues(&fx.union, &id);
    assert_eq!((d.owners.len(), d.credit), (0, 1_500 * NIN));
    assert_eq!(c.senior_earmark(&fx.union), 0);
    v.assert_conserved();
}

/// Owner split: a default pays the owners what they are still owed (the guarantee); a ninth owner on
/// one voucher is refused.
#[test]
fn a_default_pays_owners_in_full_and_owners_per_voucher_are_capped() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &600);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &600);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &300, &300);
    assert_eq!(fx.token().balance(&v.owner), 3 * PRE);
    // Seven more owners, ten centi acres each: eight in all.
    let mut owners = std::vec::Vec::new();
    for i in 0..8u8 {
        fx.env.cost_estimate().budget().reset_default();
        let o = Address::generate(&fx.env);
        fx.give_title(&o);
        let m = mc.register_machine(
            &fx.leader,
            &fx.union,
            &o,
            &1,
            &40,
            &loan_id(&fx.env, 0x60 + i),
        );
        owners.push((o, m));
    }
    for (o, m) in owners.iter().take(7) {
        fx.env.cost_estimate().budget().reset_default();
        mc.claim(o, m, &fx.union, &id, &10, &10);
    }
    fx.env.cost_estimate().budget().reset_default();
    assert_eq!(c.owner_dues(&fx.union, &id).owners.len(), 8);
    let (o9, m9) = owners.last().unwrap();
    assert_eq!(
        c.try_pay_voucher(&fx.union, &id, o9, &10, &NIN, &NIN, &0),
        Err(Ok(Error::TooManyOwners))
    );
    assert!(mc.try_claim(o9, m9, &fx.union, &id, &10, &10).is_err());

    c.report_maturity(&fx.oracle, &fx.union, &id, &T0);
    fx.at(T0 + 111 * 86_400 + 1);
    // A default with eight owners owed stays well inside one transaction's budget: no transfers in it.
    fx.env.cost_estimate().budget().reset_default();
    c.mark_default(&fx.union, &id);
    assert!(fx.env.cost_estimate().budget().cpu_instruction_cost() < 50_000_000);
    assert!(c.owner_dues(&fx.union, &id).owners.is_empty());
    // Every owner collects the whole price of the work done; nothing is left owed or held.
    fx.env.cost_estimate().budget().reset_default();
    c.collect(&fx.union, &v.owner);
    assert_eq!(fx.token().balance(&v.owner), 3 * HOUR);
    for (o, _) in owners.iter().take(7) {
        fx.env.cost_estimate().budget().reset_default();
        c.collect(&fx.union, o);
        assert_eq!(fx.token().balance(o), HOUR / 10);
    }
    assert_eq!(c.senior_earmark(&fx.union), 0);
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
    // The claim (and its VoucherClaimed) reports what fund_core booked, capped by the reserve
    // (review 5 Nov), not the ₹4,500 postpaid asked.
    assert_eq!(
        mc.claim(&v.owner, &v.machine, &fx.union, &id, &400, &400),
        (6 * PRE, 1_600 * NIN)
    );
    // Owner split: the prepay at the new price (₹4,200) first, then what the reserve still holds
    // (₹1,600 of the ₹4,500 postpaid) is held for the owner.
    assert_eq!(fx.token().balance(&v.owner), 6 * PRE);
    assert_eq!(
        c.owner_dues(&fx.union, &id).owners.get(0).unwrap().amount,
        1_600 * NIN
    );
    assert_eq!(c.senior_earmark(&fx.union), 2 * HOUR + 1_600 * NIN);
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
    assert_eq!(mc.voucher(&fx.union, &id2).unwrap().units, 100);
    assert_eq!(c.senior_earmark(&fx.union), HOUR + 1_600 * NIN);
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
        mc.voucher(&fx.union, &id2).unwrap().units - mc.voucher(&fx.union, &id2).unwrap().used,
        100
    );
    v.assert_conserved();
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
    fx.senior()
        .set_investor(&her, &true, &loan_id(&fx.env, 1), &Tag::Domestic);
    f.override_rate(&(95 * NIN), &(T0 + 86_400));
    assert!(fx
        .senior()
        .try_deposit(&her, &fx.union, &(10 * NIN))
        .is_err());
    // The attester's band is adjustable by the trust.
    f.set_rate_band(&1_000);
    f.set_rate(&fx.attester, &(104 * NIN), &(T0 + 86_400));
    fx.at(T0 + 12 * 3_600);
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
fn repaid_cash_goes_out_with_an_lp_and_comes_back_for_a_loan() {
    let fx = setup();
    let (c, f) = (fx.core(), fx.fx());
    let (_anchor, lp) = fx.lp_ready(); // treasury 5,000; the borrower invested 2,000 (escrow)
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&lp, &(100 * NIN));
    let (s24, q) = fx.ids();
    // No other borrower for the 2,000 in the drawer: an LP takes 1,900 of it for its own USDC.
    let col = fx.offer_of(OfferKind::Collect, 1_900 * NIN);
    fx.acc(&lp, &col, &s24, &q);
    f.confirm(&fx.leader, &col);
    assert_eq!(f.cash_at_hand(&fx.union), 100 * NIN);
    // Later a member wants cash and the drawer is short: the LP brings ₹950 back (₹931 after its 2% fee).
    let br = fx.offer_of(OfferKind::Bring, 950 * NIN);
    fx.acc(&lp, &br, &s24, &q);
    f.confirm(&fx.leader, &br);
    assert_eq!(f.cash_at_hand(&fx.union), 1_031 * NIN);
    let id = loan_id(&fx.env, 1);
    fx.attest(&fx.borrower, &id, 900 * NIN);
    c.draw_loan(&fx.union, &fx.borrower, &id, &(900 * NIN), &0);
    assert_eq!(f.cash_at_hand(&fx.union), 131 * NIN);
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
    assert!(loan(&c, &fx.union, &tok).is_some());
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
    c.request_unbond(&fx.union, &fx.neighbour, &(400 * NIN), &false);
    fx.at(T0 + 14 * 86_400);

    // A member can no longer claim to her wallet.
    assert_eq!(
        c.try_claim(&fx.union, &fx.neighbour, &0, &false),
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
    c.request_unbond(&fx.union, &fx.neighbour, &(400 * NIN), &false);
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

/// No member path leaves nIN in her wallet: draw, claim over the desk.
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
    c.request_unbond(&fx.union, &fx.neighbour, &(500 * NIN), &false);
    fx.at(T0 + 14 * 86_400);
    c.claim_junior_cash(&fx.union, &fx.neighbour, &fx.leader, &0);
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
    // Not a leader or member of the union: refused.
    let outsider = Address::generate(&fx.env);
    assert_eq!(
        f.try_swap_usdc_to_nin(&outsider, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::NotMember))
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

/// Self mode (executive decision 4 Nov): an ID-checked title holder of the union swaps both ways like a leader;
/// without the ID check, or holding her title in another union, she is refused.
#[test]
fn id_checked_member_swaps_in_self_mode() {
    let fx = setup();
    let f = fx.fx();
    fx.env.mock_all_auths();
    f.override_rate(&(95 * NIN), &(T0 + 86_400));
    f.set_anchor(&Address::generate(&fx.env));
    let her = Address::generate(&fx.env);
    fx.give_title(&her);
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&her, &(10 * NIN));
    // Member, identity not checked: refused.
    assert_eq!(
        f.try_swap_usdc_to_nin(&her, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::NotIdChecked))
    );
    // ID-checked, but her title is in another union: refused.
    let other_union = Address::generate(&fx.env);
    let away = Address::generate(&fx.env);
    LandTitleContractClient::new(&fx.env, &fx.titles_id).set_title(
        &901,
        &fx.attester,
        &away,
        &other_union,
        &BytesN::from_array(&fx.env, &[0xAB; 20]),
        &BytesN::from_array(&fx.env, &[1; 32]),
        &BytesN::from_array(&fx.env, &[2; 32]),
    );
    f.set_id_checked(&away, &true, &loan_id(&fx.env, 0x52));
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&away, &(10 * NIN));
    assert_eq!(
        f.try_swap_usdc_to_nin(&away, &fx.union, &(10 * NIN)),
        Err(Ok(FxError::NotMember))
    );
    assert_eq!(
        f.try_swap_nin_to_usdc(&away, &fx.union, &NIN),
        Err(Ok(FxError::NotMember))
    );
    // ID-checked member of the union: both ways, through the union's swap bucket.
    f.set_id_checked(&her, &true, &loan_id(&fx.env, 0x53));
    assert_eq!(f.swap_usdc_to_nin(&her, &fx.union, &(10 * NIN)), 950 * NIN);
    assert_eq!(fx.token().balance(&her), 950 * NIN);
    assert_eq!(f.leader_swap_usdc(&fx.union), 10 * NIN);
    assert_eq!(f.swap_nin_to_usdc(&her, &fx.union, &(475 * NIN)), 5 * NIN);
    assert_eq!(fx.token().balance(&her), 475 * NIN);
    assert_eq!(f.leader_swap_usdc(&fx.union), 5 * NIN);
    assert_eq!(f.swap_usdc(&fx.union, &her), 5 * NIN);
    // Review 5 Nov: nIN from elsewhere (owner pay, say) never draws on another's backing. A leader
    // swaps 20 USDC in; she gets 475 nIN more from elsewhere, yet swaps back at most her own 5 USDC,
    // never the leader's.
    f.set_id_checked(&fx.leader, &true, &loan_id(&fx.env, 0x54));
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&fx.leader, &(20 * NIN));
    f.swap_usdc_to_nin(&fx.leader, &fx.union, &(20 * NIN));
    let nin = StellarAssetClient::new(&fx.env, &fx.token().address);
    nin.set_authorized(&her, &true);
    nin.mint(&her, &(475 * NIN));
    nin.set_authorized(&her, &false);
    assert_eq!(f.leader_swap_usdc(&fx.union), 25 * NIN);
    assert_eq!(
        f.try_swap_nin_to_usdc(&her, &fx.union, &(950 * NIN)),
        Err(Ok(FxError::NoUsdc))
    );
    assert_eq!(f.swap_nin_to_usdc(&her, &fx.union, &(475 * NIN)), 5 * NIN);
    assert_eq!(f.swap_usdc(&fx.union, &her), 0);
    assert_eq!(f.swap_usdc(&fx.union, &fx.leader), 20 * NIN);
    assert_eq!(f.leader_swap_usdc(&fx.union), 20 * NIN);
    // Her own signature is required.
    fx.env.set_auths(&[]);
    assert!(f.try_swap_nin_to_usdc(&her, &fx.union, &NIN).is_err());
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

/// An LP account whose `send_usdc` pays nothing (re-review, 3 Nov).
mod fake_lp {
    use soroban_sdk::{contract, contractimpl, Address, Env};

    #[contract]
    pub struct FakeLp;

    #[contractimpl]
    impl FakeLp {
        pub fn send_usdc(_env: Env, _to: Address, _amount: i128) {}
        // An upgraded LP account (review 5 Nov): its lien is for show; it locks and pays nothing.
        pub fn lock(_env: Env, amount: i128) -> i128 {
            amount
        }
        pub fn unlock(_env: Env, _amount: i128) -> i128 {
            0
        }
        pub fn pay(_env: Env, _amount: i128) -> i128 {
            0
        }
    }
}

/// Review 5 Nov (finding 2): a Collect is booked only for the USDC that actually arrived in fx_pool; an LP
/// account whose `pay` brings nothing is refused at the scan, so the leader keeps the cash.
#[test]
fn a_collect_is_booked_only_for_usdc_that_arrived() {
    let fx = setup();
    let f = fx.fx();
    let (_anchor, _lp) = fx.lp_ready();
    let (s24, q) = fx.ids();
    let own = f.treasury_usdc(&fx.union);
    // An account whose `pay` brings nothing.
    let fake = fx.env.register(fake_lp::FakeLp, ());
    fx.give_title(&fake);
    f.set_id_checked(&fake, &true, &loan_id(&fx.env, 0x61));
    f.set_lp(&fx.union, &fake, &true, &loan_id(&fx.env, 0x62));
    StellarAssetClient::new(&fx.env, &fx.usdc).mint(&fake, &(50 * NIN));
    let c = fx.offer_of(OfferKind::Collect, 950 * NIN);
    assert_eq!(fx.acc(&fake, &c, &s24, &q), 10 * NIN);
    let hand = f.cash_at_hand(&fx.union);
    assert_eq!(f.try_confirm(&fx.leader, &c), Err(Ok(FxError::NotPaid)));
    assert_eq!(f.cash_at_hand(&fx.union), hand);
    assert_eq!(f.lp_usdc(&fx.union), 0);
    assert_eq!(f.treasury_usdc(&fx.union), own);
}

/// Review 5 Nov (finding 11): with no senior shareholders left, the voucher interest her investment covers at
/// a default goes to the union treasury, as `senior_interest` does at a repayment; it is not left in senior
/// cash that no one can claim.
#[test]
fn default_voucher_interest_goes_to_the_treasury_when_senior_has_no_shares() {
    let v = vsetup();
    let (fx, c, s) = (&v.fx, v.fx.core(), v.fx.senior());
    let id = loan_id(&fx.env, 1);
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
    // A month on she gives every hour back: the voucher value is repaid to senior, its interest is not.
    fx.at(T0 + 30 * 86_400);
    c.return_voucher(&fx.borrower, &fx.union, &id, &200);
    // Every senior investor leaves before the default.
    let p = c.senior_position(&fx.union, &v.investor);
    c.request_unbond(&fx.union, &v.investor, &p.shares, &true);
    fx.at(T0 + 44 * 86_400 + 1);
    s.redeem(&v.investor, &fx.union, &0);
    assert_eq!(c.get_senior_market(&fx.union).total_shares, 0);
    c.report_maturity(&fx.oracle, &fx.union, &id, &T0);
    fx.at(T0 + 111 * 86_400 + 1);
    let treasury = c.union_treasury(&fx.union);
    let senior_cash = c.get_senior_market(&fx.union).cash;
    c.mark_default(&fx.union, &id);
    let gained = c.union_treasury(&fx.union) - treasury;
    assert!(gained > 0);
    assert_eq!(c.senior_income(&fx.union), 0);
    // Senior cash takes back the voucher value only, never the interest.
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(sm.total_borrows, 0);
    assert!(sm.cash >= senior_cash);
    v.assert_conserved();
}
