#![cfg(test)]

extern crate std;

use super::*;
use nila_fx_pool::{FxPoolContract, FxPoolContractClient};
use nila_land_title::{LandTitleContract, LandTitleContractClient};
use nila_math::{NIN, YEAR};
use nila_roles::RolesContract;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

const T0: u64 = 1_700_000_000;

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
    attester: Address,
    next_title: core::cell::Cell<u64>,
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

    /// F2: the attester copies her food token, open from T0. Tests use the loan id as token id.
    fn attest(&self, member: &Address, token: &BytesN<32>, amount: i128) {
        self.core()
            .attest_food_token(&self.attester, &self.union, token, member, &amount, &T0);
    }

    fn draw(&self, id: &BytesN<32>, cash: i128) {
        self.attest(&self.borrower, id, cash);
        self.core()
            .draw_loan(&self.union, &self.borrower, id, id, &cash, &0);
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
        escrow_duration: 7 * 86_400,
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

    let roles_id = env.register(RolesContract, (admin.clone(),));
    let roles = nila_roles::RolesContractClient::new(&env, &roles_id);
    roles.set_role(&oracle, &nila_roles::Role::Oracle, &true);
    roles.set_role(&attester, &nila_roles::Role::Attester, &true);
    roles.set_leader(&union, &leader, &true);

    let titles_id = env.register(LandTitleContract, (admin.clone(), roles_id.clone()));
    let core_id = env.register(
        FundCore,
        (admin.clone(), roles_id, titles_id.clone(), nin.clone()),
    );
    let usdc = Address::generate(&env);
    let fx_id = env.register(
        FxPoolContract,
        (admin.clone(), usdc, nin.clone(), 25_000 * NIN),
    );
    FxPoolContractClient::new(&env, &fx_id).set_fund_core(&core_id);

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
        attester,
        next_title: core::cell::Cell::new(1),
    };
    fx.core().set_fx_pool(&fx.fx_id);
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
        .try_draw_loan(&fx.union, &stranger, &id, &id, &(10 * NIN), &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NftRequired));
    // R26: 600 nIN invested before any crop share cash.
    fx.attest(&fx.borrower, &id, 20_000 * NIN);
    let err = c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &id, &(10 * NIN), &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::BelowGate));

    fx.invest(&fx.borrower, NIN);
    fx.invest(&fx.neighbour, 20_000 * NIN);
    // R34: ₹10,000 per loan.
    let err = c
        .try_draw_loan(&fx.union, &fx.borrower, &id, &id, &(10_000 * NIN + 1), &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::CapExceeded));
    c.draw_loan(&fx.union, &fx.borrower, &id, &id, &(10_000 * NIN), &0);
    let token2 = loan_id(&fx.env, 2);
    fx.attest(&fx.borrower, &token2, NIN);
    let err = c
        .try_draw_loan(&fx.union, &fx.borrower, &token2, &id, &NIN, &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::LoanExists));

    // Cash comes from the junior pool only.
    let other = setup();
    other.invest(&other.borrower, 600 * NIN);
    let t = loan_id(&other.env, 1);
    other.attest(&other.borrower, &t, 1_000 * NIN);
    let err = other
        .core()
        .try_draw_loan(&other.union, &other.borrower, &t, &t, &(601 * NIN), &0)
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
        .try_draw_loan(&fx.union, &fx.neighbour, &id2, &id2, &NIN, &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::LendingOnHold));

    // A part payment keeps the hold; the full payment lifts it.
    fx.repay(&id, 100 * NIN);
    assert_eq!(c.on_hold(&fx.union), 1);
    assert!(fx.repay(&id, 1_000 * NIN));
    assert_eq!(c.on_hold(&fx.union), 0);
    c.draw_loan(&fx.union, &fx.neighbour, &id2, &id2, &NIN, &0);
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
fn draw_closes_the_cash_at_hand_escrow() {
    let fx = setup();
    fx.invest(&fx.borrower, 1_000 * NIN);
    fx.fx()
        .open_escrow(&7, &fx.union, &(1_000 * NIN), &(7 * 86_400));
    let id = loan_id(&fx.env, 1);
    fx.attest(&fx.borrower, &id, 400 * NIN);
    fx.core()
        .draw_loan(&fx.union, &fx.borrower, &id, &id, &(400 * NIN), &7);
    assert_eq!(fx.fx().get_escrow(&7).unwrap().remaining, 600 * NIN);
    assert_eq!(
        fx.core()
            .get_loan(&fx.union, &loan_id(&fx.env, 1))
            .unwrap()
            .escrow_id,
        7
    );
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
        .try_draw_loan(
            &fx.union,
            &fx.borrower,
            &loan_id(&fx.env, 1),
            &loan_id(&fx.env, 1),
            &NIN,
            &0,
        )
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
fn upgrade_delay_is_lowered_only_through_the_delay() {
    let fx = setup();
    let c = fx.core();
    c.set_upgrade_delay(&(48 * 3600));
    c.set_upgrade_delay(&0);
    assert_eq!(c.get_config().upgrade_delay, 48 * 3600);
    fx.at(T0 + 48 * 3600);
    c.set_upgrade_delay(&0);
    assert_eq!(c.get_config().upgrade_delay, 0);
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
    let draw = |token: &BytesN<32>, id: &BytesN<32>, cash: i128| {
        c.try_draw_loan(&fx.union, &fx.borrower, token, id, &cash, &0)
            .unwrap_err()
    };

    assert_eq!(draw(&tok, &l1, NIN), Ok(Error::NoFoodToken));
    // Someone else's token is not hers.
    fx.attest(&fx.neighbour, &tok, 500 * NIN);
    assert_eq!(draw(&tok, &l1, NIN), Ok(Error::NoFoodToken));
    // Attester only.
    assert_eq!(
        c.try_attest_food_token(&fx.leader, &fx.union, &tok, &fx.borrower, &(500 * NIN), &T0)
            .unwrap_err(),
        Ok(Error::NotAttester)
    );
    // Corrected before use, open from day 30.
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &tok,
        &fx.borrower,
        &(500 * NIN),
        &(T0 + 30 * DAY),
    );
    assert_eq!(draw(&tok, &l1, NIN), Ok(Error::NotOpenYet));
    fx.at(T0 + 30 * DAY);
    // With the amount gate on, at most the attested amount.
    let mut uc = union_cfg();
    uc.enforce_token_amount = true;
    c.set_union_config(&fx.union, &uc);
    assert_eq!(draw(&tok, &l1, 500 * NIN + 1), Ok(Error::AboveFoodToken));
    // Testing (gate off): the union may give more than attested, within the ₹10,000 cap.
    c.set_union_config(&fx.union, &union_cfg());
    assert_eq!(draw(&tok, &l1, 10_000 * NIN + 1), Ok(Error::CapExceeded));
    c.draw_loan(&fx.union, &fx.borrower, &tok, &l1, &(800 * NIN), &0);
    assert_eq!(
        c.food_token(&fx.union, &tok).unwrap().loan_id,
        Some(l1.clone())
    );
    assert_eq!(c.get_loan(&fx.union, &l1).unwrap().token_id, tok);

    // One loan per token, and no re-attesting a used one.
    assert_eq!(draw(&tok, &l2, NIN), Ok(Error::FoodTokenUsed));
    assert_eq!(
        c.try_attest_food_token(
            &fx.attester,
            &fx.union,
            &tok,
            &fx.borrower,
            &(500 * NIN),
            &T0
        )
        .unwrap_err(),
        Ok(Error::FoodTokenUsed)
    );
}
