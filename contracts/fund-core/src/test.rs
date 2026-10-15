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
            .attest_food_token(&self.attester, &self.union, token, member, &amount, &0, &T0);
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
        c.try_attest_food_token(
            &fx.leader,
            &fx.union,
            &tok,
            &fx.borrower,
            &(500 * NIN),
            &0,
            &T0
        )
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
        &0,
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
            &T0
        )
        .unwrap_err(),
        Ok(Error::FoodTokenUsed)
    );
}

// ---------- C3: the cash desk through the real fx_pool, strict auth ----------

use nila_fx_pool::{Error as FxError, Purpose};
use soroban_sdk::{
    testutils::{MockAuth, MockAuthInvoke},
    Bytes, IntoVal,
};

fn hash(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

impl Fx {
    /// The leader scans rupees in; only the leader signs (fx_pool signs its own pull).
    fn scan(
        &self,
        purpose: Purpose,
        member: &Address,
        amount: i128,
        receipt: u8,
        loan: Option<BytesN<32>>,
    ) -> Result<u64, FxError> {
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

#[test]
fn cash_scan_invest_draw_repay_with_strict_auth() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    // Cash at hand may not exceed the treasury: the union funds it first (R15, R17).
    assert_eq!(
        fx.scan(Purpose::Invest, &fx.borrower, 1_000 * NIN, 1, None),
        Err(FxError::AboveTreasury)
    );
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
    // A receipt is used once.
    assert_eq!(
        fx.scan(Purpose::Invest, &fx.borrower, NIN, 3, None),
        Err(FxError::ScanUsed)
    );
    assert_eq!(
        fx.scan(Purpose::Invest, &fx.neighbour, 501 * NIN, 4, None),
        Err(FxError::AboveTreasury)
    );
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
    assert_eq!(f.get_escrow(&e1).unwrap().remaining, 600 * NIN);
    assert_eq!(fx.token().balance(&fx.borrower), 0);

    // Repay in cash, more than owed: only what she owes is minted; the leader gives change.
    fx.at(T0 + YEAR / 2);
    let interest = 400 * NIN * 1250 / 10_000 / 2;
    let e2 = fx
        .scan(Purpose::Repay, &fx.borrower, 500 * NIN, 5, Some(id.clone()))
        .unwrap();
    assert!(c.get_loan(&fx.union, &id).unwrap().closed);
    assert_eq!(f.get_escrow(&e2).unwrap().amount, 400 * NIN + interest);
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
        fx.fx().try_burn_expired_escrow(&e),
        Err(Ok(FxError::NotExpired))
    );
    fx.at(T0 + 7 * 86_400);
    // Anyone may call it once expired; the union kept the rupees.
    fx.env.set_auths(&[]);
    assert_eq!(fx.fx().burn_expired_escrow(&e), 800 * NIN);
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

#[test]
fn cash_out_pay_out_and_reclaim() {
    let fx = setup();
    let c = fx.core();
    let f = fx.fx();
    fx.scan(Purpose::Treasury, &fx.leader, 2_000 * NIN, 1, None)
        .unwrap();
    fx.scan(Purpose::Invest, &fx.neighbour, 1_000 * NIN, 2, None)
        .unwrap();
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
    assert!(f.try_burn_expired_escrow(&e).is_err());
    c.unpause();
    f.burn_expired_escrow(&e);
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
    ] {
        v.push_back(k.into_val(&env));
    }
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
            earmark: 2,
            earmark_per_hour: 3,
            senior_out: 4,
            dues: soroban_sdk::vec![
                &env,
                Due {
                    owner: a.clone(),
                    amount: 5,
                },
            ],
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
        hex, "fe30eea98285703bcefcefc3fdcdf55b42dc5be5ac9fb1d23c5298323b9abb68",
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
    assert_eq!(f.accept(&lp, &c, &s24, &q), 99_000_000);
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
    assert_eq!(f.accept(&lp, &b, &s24, &q), 50_500_000);
    assert_eq!(f.lp_usdc(&fx.union), 99_000_000 - 50_500_000);
    f.confirm(&fx.leader, &b);
    assert_eq!(usdc.balance(&lp), 50 * NIN - 99_000_000 + 50_500_000);
    assert_eq!(usdc.balance(&fx.fx_id), 99_000_000 - 50_500_000);
    assert_eq!(f.offers_of(&fx.union).len(), 0);
    // The LP never holds nIN; nIN books are untouched by swaps.
    assert_eq!(fx.token().balance(&lp), 0);
    fx.assert_fx_empty();
    fx.assert_conserved();
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
        f.try_accept(&lp, &big, &s24, &q),
        Err(Ok(FxError::BelowVoucherFloor))
    );
    // Only 10 USDC are free above the floor: ₹950 (10.1 USDC with the fee) is refused, ₹900 fits.
    let mid = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(950 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(
        f.try_accept(&lp, &mid, &s24, &q),
        Err(Ok(FxError::BelowVoucherFloor))
    );
    let ok = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(900 * NIN),
        &(T0 + 3_600),
    );
    let paid = f.accept(&lp, &ok, &s24, &q);
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
    f.accept(&lp, &again, &s24, &q);
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
    // R20: a member with a title the union minted.
    assert_eq!(
        f.try_set_lp(&fx.union, &stranger, &true, &loan_id(&fx.env, 1)),
        Err(Ok(FxError::NotMember))
    );
    // Only the anchor accepts LPs.
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
        f.try_accept(&fx.borrower, &c, &s24, &q),
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
    assert_eq!(f.try_accept(&lp, &b, &s24, &q), Err(Ok(FxError::NoUsdc)));
    // A dropped LP cannot accept.
    f.set_lp(&fx.union, &lp, &false, &loan_id(&fx.env, 1));
    assert_eq!(f.try_accept(&lp, &c, &s24, &q), Err(Ok(FxError::NotLp)));
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
    fx.env.mock_auths(&[MockAuth {
        address: &lp,
        invoke: &MockAuthInvoke {
            contract: &fx.fx_id,
            fn_name: "accept",
            args: (lp.clone(), c, s24.clone(), q.clone()).into_val(&fx.env),
            sub_invokes: &[MockAuthInvoke {
                contract: &fx.usdc,
                fn_name: "approve",
                args: (lp.clone(), fx.fx_id.clone(), 99_000_000i128, expiry_ledger)
                    .into_val(&fx.env),
                sub_invokes: &[],
            }],
        },
    }]);
    f.accept(&lp, &c, &s24, &q);
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

    /// fund_core nIN = pools + treasury + rainy + the owner's collectable balance.
    fn assert_conserved(&self) {
        let c = self.fx.core();
        let u = &self.fx.union;
        let books = c.get_junior_market(u).cash
            + c.get_senior_market(u).cash
            + c.union_treasury(u)
            + c.union_rainy_day(u)
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

#[test]
fn voucher_draw_claim_repay_and_collect() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    // The plan recommends 400 cash and 10 hours (not caps, 9 Oct).
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(400 * NIN),
        &10,
        &T0,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(400 * NIN), &10);
    assert_eq!(c.senior_earmark(&fx.union), 10 * PRE);
    assert_eq!(mc.voucher(&fx.union, &id).unwrap().hours, 10);
    // Senior cash held back is not claimable by seniors.
    assert_eq!(c.get_senior_market(&fx.union).cash, 10_000 * NIN);

    // A8a: she opens a code for up to 6 hours; the owner works 4 and scans it.
    let code = Bytes::from_array(&fx.env, b"tractor-code-1");
    let h: BytesN<32> = fx.env.crypto().sha256(&code).into();
    mc.open_code(&fx.borrower, &fx.union, &id, &6, &h);
    assert_eq!(
        mc.claim(&v.owner, &v.machine, &fx.union, &id, &code, &4),
        (4 * PRE, 4 * PEND)
    );
    // The prepay is in the owner's wallet at once, from senior money (D18, N4).
    assert_eq!(fx.token().balance(&v.owner), 4 * PRE);
    assert!(!fx.authorized(&v.owner));
    assert_eq!(c.get_senior_market(&fx.union).cash, 10_000 * NIN - 4 * PRE);
    assert_eq!(c.senior_earmark(&fx.union), 6 * PRE);
    let b = c.voucher_book(&fx.union, &id).unwrap();
    assert_eq!(
        (b.senior_out, b.dues.get(0).unwrap().amount),
        (4 * PRE, 4 * PEND)
    );
    // One claim per code; the rest of her hours stay with her.
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &code, &1),
        Err(Ok(nila_machinery::Error::BadCode))
    );
    // Her lock counts the voucher part too (R27): she owes more than her 2,000, so all of it.
    let p = c.position(&fx.union, &fx.borrower);
    assert_eq!(p.locked, p.invested);
    v.assert_conserved();

    // She repays everything: owners first, then interest, principal, the senior prepay.
    fx.at(T0 + YEAR / 2);
    let interest = 400 * NIN * 1250 / 10_000 / 2;
    let total = 4 * PEND + interest + 400 * NIN + 4 * PRE;
    StellarAssetClient::new(&fx.env, &fx.nin).mint(&fx.fx_id, &(total + 50 * NIN));
    assert!(c.repay_loan(&fx.union, &id, &(total + 50 * NIN)));
    assert_eq!(c.owner_balance(&fx.union, &v.owner), 4 * PEND);
    assert_eq!(c.get_senior_market(&fx.union).cash, 10_000 * NIN);
    assert_eq!(c.get_senior_market(&fx.union).total_borrows, 0);
    // Closed: the unclaimed 6 hours lapse and their earmark is released.
    assert_eq!(c.senior_earmark(&fx.union), 0);
    assert!(mc.voucher(&fx.union, &id).unwrap().void);
    v.assert_conserved();

    // A13: the owner collects her settled part to her wallet.
    assert_eq!(c.collect_dues(&v.owner, &fx.union), 4 * PEND);
    assert_eq!(fx.token().balance(&v.owner), 4 * PRE + 4 * PEND);
    assert!(!fx.authorized(&v.owner));
    v.assert_conserved();
}

#[test]
fn claims_stay_inside_code_voucher_week_and_availability() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &14, &T0);
    // A voucher-only loan: no cash part.
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &14);
    assert_eq!(
        c.try_repay_loan(&fx.union, &id, &NIN),
        Err(Ok(Error::NothingOwed))
    );
    let code = Bytes::from_array(&fx.env, b"c1");
    let h: BytesN<32> = fx.env.crypto().sha256(&code).into();
    use nila_machinery::Error as ME;
    assert_eq!(
        mc.try_open_code(&fx.borrower, &fx.union, &id, &15, &h),
        Err(Ok(ME::TooManyHours))
    );
    mc.open_code(&fx.borrower, &fx.union, &id, &10, &h);
    assert_eq!(
        mc.try_open_code(&fx.borrower, &fx.union, &id, &1, &h),
        Err(Ok(ME::CodeOpen))
    );
    // Not her machine; more than the code; more than a machine's hours a week.
    assert_eq!(
        mc.try_claim(&fx.borrower, &v.machine, &fx.union, &id, &code, &1),
        Err(Ok(ME::NotOwner))
    );
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &code, &11),
        Err(Ok(ME::TooManyHours))
    );
    let small = mc.register_machine(&fx.leader, &fx.union, &v.owner, &1, &3, &id);
    mc.confirm_machine(&v.owner, &small);
    assert_eq!(
        mc.try_claim(&v.owner, &small, &fx.union, &id, &code, &4),
        Err(Ok(ME::TooManyHours))
    );
    // On hold (R83): no claims.
    mc.set_hold(&v.owner, &v.machine, &true, &1);
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &code, &1),
        Err(Ok(ME::Unavailable))
    );
    mc.set_hold(&v.owner, &v.machine, &false, &0);
    // A wrong code, then an expired one (48 h).
    let wrong = Bytes::from_array(&fx.env, b"c2");
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &wrong, &1),
        Err(Ok(ME::BadCode))
    );
    fx.at(T0 + 2 * 86_400);
    assert_eq!(
        mc.try_claim(&v.owner, &v.machine, &fx.union, &id, &code, &1),
        Err(Ok(ME::CodeExpired))
    );
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
    // Not enough senior money for the prepay of the hours asked.
    let id2 = loan_id(&fx.env, 2);
    c.attest_food_token(&fx.attester, &fx.union, &id2, &fx.borrower, &0, &20, &T0);
    assert_eq!(
        c.try_draw_loan(&fx.union, &fx.borrower, &id2, &0, &20),
        Err(Ok(Error::NoSeniorBacking))
    );
}

#[test]
fn default_pays_the_owner_and_senior_from_her_investment_first() {
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
        &2,
        &T0,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(300 * NIN), &2);
    let code = Bytes::from_array(&fx.env, b"c1");
    let h: BytesN<32> = fx.env.crypto().sha256(&code).into();
    mc.open_code(&fx.borrower, &fx.union, &id, &2, &h);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &code, &2);
    c.report_maturity(&fx.oracle, &fx.union, &id, &T0);
    fx.at(T0 + 111 * 86_400 + 1);
    c.mark_default(&fx.union, &id);
    // Her 2,000 covers everything: owners in full, principal, interest, the senior prepay.
    assert_eq!(c.owner_balance(&fx.union, &v.owner), 2 * PEND);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!((sm.cash, sm.total_borrows), (10_000 * NIN, 0));
    assert!(mc.voucher(&fx.union, &id).unwrap().void);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    v.assert_conserved();
}

#[test]
fn default_beyond_her_investment_owner_still_paid_senior_takes_its_loss() {
    let v = vsetup();
    let (fx, c, mc) = (&v.fx, v.fx.core(), v.mach());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(
        &fx.attester,
        &fx.union,
        &id,
        &fx.borrower,
        &(300 * NIN),
        &4,
        &T0,
    );
    c.draw_loan(&fx.union, &fx.borrower, &id, &(300 * NIN), &4);
    let code = Bytes::from_array(&fx.env, b"c1");
    let h: BytesN<32> = fx.env.crypto().sha256(&code).into();
    mc.open_code(&fx.borrower, &fx.union, &id, &4, &h);
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &code, &4);
    c.report_maturity(&fx.oracle, &fx.union, &id, &T0);
    fx.at(T0 + 111 * 86_400 + 1);
    c.mark_default(&fx.union, &id);
    // She owed 3,000 dues + 300 + interest + 2,800 prepay against 2,000 invested. The owner is
    // paid in full (D17); the senior prepay her investment could not cover is senior's loss.
    assert_eq!(c.owner_balance(&fx.union, &v.owner), 4 * PEND);
    assert_eq!(c.voucher_book(&fx.union, &id).unwrap().dues.len(), 0);
    let sm = c.get_senior_market(&fx.union);
    assert_eq!(sm.total_borrows, 0);
    assert!(sm.cash < 10_000 * NIN);
    assert_eq!(c.position(&fx.union, &fx.borrower).invested, 0);
    v.assert_conserved();
}

#[test]
fn review_fixes_self_claim_rate_rise_earmarks_close_unused() {
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
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &4, &T0);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &4);
    let code = Bytes::from_array(&fx.env, b"c1");
    let h: BytesN<32> = fx.env.crypto().sha256(&code).into();
    mc.open_code(&fx.borrower, &fx.union, &id, &4, &h);
    assert_eq!(
        mc.try_claim(&fx.borrower, &mine, &fx.union, &id, &code, &1),
        Err(Ok(ME::SelfClaim))
    );
    // Another loan holds back 10 hours: 14 × ₹700 of the 10,000 senior nIN are earmarked.
    let id2 = loan_id(&fx.env, 2);
    c.attest_food_token(&fx.attester, &fx.union, &id2, &fx.borrower, &0, &10, &T0);
    c.draw_loan(&fx.union, &fx.borrower, &id2, &0, &10);
    assert_eq!(c.senior_earmark(&fx.union), 14 * PRE);
    // The union raises its tractor rate 50% after the draw: the higher prepay would eat the
    // other loan's earmark, so the claim is refused.
    mc.set_rate(&fx.union, &1, &tractor(15_000_000));
    assert!(mc
        .try_claim(&v.owner, &v.machine, &fx.union, &id, &code, &4)
        .is_err());
    mc.set_rate(&fx.union, &1, &tractor(NIN));
    mc.claim(&v.owner, &v.machine, &fx.union, &id, &code, &4);

    // She releases the voucher she no longer needs: nothing owed, the earmark goes back.
    assert_eq!(
        c.try_close_unused(&fx.borrower, &fx.union, &id),
        Err(Ok(Error::BadState))
    );
    c.close_unused(&fx.borrower, &fx.union, &id2);
    assert_eq!(c.senior_earmark(&fx.union), 0);
    assert!(mc.voucher(&fx.union, &id2).unwrap().void);
    v.assert_conserved();
}

#[test]
fn bring_respects_the_daily_cap_and_the_voucher_earmark() {
    let v = vsetup();
    let (fx, c, f) = (&v.fx, v.fx.core(), v.fx.fx());
    let anchor = Address::generate(&fx.env);
    f.set_anchor(&anchor);
    f.set_lp(&fx.union, &v.owner, &true, &loan_id(&fx.env, 0x4C));
    fx.scan(Purpose::Treasury, &fx.leader, 5_000 * NIN, 1, None)
        .unwrap();
    let (s24, q) = fx.ids();
    // 200 senior USDC at ₹50; a voucher earmarks 14 × ₹700 = 9,800 nIN = 196 USDC.
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &14, &T0);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &14);
    // ₹250 → 5 USDC + 1% would cut into the 196 USDC held for the voucher.
    let b = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(250 * NIN),
        &(T0 + 3_600),
    );
    assert_eq!(
        f.try_accept(&v.owner, &b, &s24, &q),
        Err(Ok(FxError::BelowVoucherFloor))
    );
    // ₹150 → 3.03 USDC fits in the 4 free.
    let ok = f.post_offer(
        &fx.leader,
        &fx.union,
        &OfferKind::Bring,
        &(150 * NIN),
        &(T0 + 3_600),
    );
    f.accept(&v.owner, &ok, &s24, &q);
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
fn close_unused_is_hers_only_and_evented() {
    let v = vsetup();
    let (fx, c) = (&v.fx, v.fx.core());
    let id = loan_id(&fx.env, 1);
    c.attest_food_token(&fx.attester, &fx.union, &id, &fx.borrower, &0, &2, &T0);
    c.draw_loan(&fx.union, &fx.borrower, &id, &0, &2);
    assert_eq!(
        c.try_close_unused(&fx.neighbour, &fx.union, &id),
        Err(Ok(Error::NotBorrower))
    );
    c.close_unused(&fx.borrower, &fx.union, &id);
    assert!(c.get_loan(&fx.union, &id).unwrap().closed);
    assert_eq!(
        c.try_close_unused(&fx.borrower, &fx.union, &id),
        Err(Ok(Error::LoanClosed))
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
    f.accept(&lp, &col, &s24, &q);
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
    f.accept(&lp, &br, &s24, &q);
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
