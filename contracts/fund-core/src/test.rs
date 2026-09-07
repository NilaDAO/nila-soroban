#![cfg(test)]

extern crate std;

use super::*;
use nila_fund_viewer::FundViewerContract;
use nila_math::{NIN, RAY, YEAR};
use nila_membership::MembershipContract;
use nila_roles::RolesContract;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env,
};

struct Fx {
    env: Env,
    admin: Address,
    oracle: Address,
    leader: Address,
    union: Address,
    borrower: Address,
    junior_lp: Address,
    senior_lp: Address,
    loan_type: BytesN<32>,
    core_id: Address,
    nin: Address,
    membership_id: Address,
}

impl Fx {
    fn core(&self) -> FundCoreClient<'_> {
        FundCoreClient::new(&self.env, &self.core_id)
    }

    fn token(&self) -> TokenClient<'_> {
        TokenClient::new(&self.env, &self.nin)
    }

    fn mint(&self, to: &Address, amount: i128) {
        StellarAssetClient::new(&self.env, &self.nin).mint(to, &amount);
    }

    fn confirm(&self, member: &Address) {
        let m = nila_membership::MembershipContractClient::new(&self.env, &self.membership_id);
        m.confirm_member(
            &self.union,
            member,
            &self.leader,
            &BytesN::from_array(&self.env, &[0xAB; 20]),
            &vec![&self.env, 1u64],
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
    let oracle = Address::generate(&env);
    let leader = Address::generate(&env);
    let union = Address::generate(&env);
    let borrower = Address::generate(&env);
    let junior_lp = Address::generate(&env);
    let senior_lp = Address::generate(&env);
    let loan_type = BytesN::from_array(&env, &[0x11; 32]);

    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let nin = sac.address();

    let roles_id = env.register(RolesContract, (admin.clone(),));
    let roles = nila_roles::RolesContractClient::new(&env, &roles_id);
    roles.set_oracle(&oracle, &true);
    roles.set_leader(&union, &leader, &true);

    let membership_id = env.register(MembershipContract, (admin.clone(), attester.clone()));
    let membership = nila_membership::MembershipContractClient::new(&env, &membership_id);
    membership.set_roles(&roles_id);

    let viewer_id = env.register(FundViewerContract, (admin.clone(),));
    let viewer = nila_fund_viewer::FundViewerContractClient::new(&env, &viewer_id);

    let core_id = env.register(
        FundCore,
        (
            admin.clone(),
            roles_id.clone(),
            membership_id.clone(),
            viewer_id.clone(),
            nin.clone(),
        ),
    );
    viewer.set_core(&core_id);

    let fx = Fx {
        env,
        admin,
        oracle,
        leader,
        union,
        borrower,
        junior_lp,
        senior_lp,
        loan_type,
        core_id,
        nin,
        membership_id,
    };
    fx.core()
        .set_reserve_config(&fx.admin, &fx.union, &0, &0, &false, &0);
    fx.confirm(&fx.borrower);
    fx.confirm(&fx.junior_lp);
    fx
}

fn draw(
    fx: &Fx,
    loan_id: &BytesN<32>,
    amount: i128,
    rate_bp: u32,
    min_rate_bp: u32,
    fast_draw: bool,
    maturity_ts: u64,
) {
    fx.core().draw_loan(
        &fx.borrower,
        &fx.oracle,
        &fx.union,
        &DrawParams {
            loan_id: loan_id.clone(),
            loan_type: fx.loan_type.clone(),
            amount,
            rate_bp,
            maturity_ts,
            max_amount: amount,
            min_rate_bp,
            fast_draw,
            escrow_id: 0,
            sos_date: if fast_draw {
                fx.env.ledger().timestamp()
            } else {
                0
            },
        },
    );
}

fn seed_liquidity(fx: &Fx, junior: i128, senior: i128) {
    if junior > 0 {
        fx.mint(&fx.junior_lp, junior);
        fx.core()
            .deposit_junior(&fx.junior_lp, &fx.union, &fx.loan_type, &junior);
    }
    if senior > 0 {
        fx.mint(&fx.senior_lp, senior);
        fx.core().deposit_senior(&fx.senior_lp, &fx.union, &senior);
    }
}

#[test]
fn deposit_gates() {
    let fx = setup();
    fx.mint(&fx.senior_lp, 10 * NIN);
    fx.core()
        .deposit_senior(&fx.senior_lp, &fx.union, &(10 * NIN));
    assert_eq!(fx.core().get_senior_market(&fx.union).cash, 10 * NIN);

    // member cannot be senior
    fx.mint(&fx.junior_lp, 5 * NIN);
    let err = fx
        .core()
        .try_deposit_senior(&fx.junior_lp, &fx.union, &(5 * NIN))
        .unwrap_err();
    assert_eq!(err, Ok(Error::NftForbidden));

    // non-member cannot be junior
    let err = fx
        .core()
        .try_deposit_junior(&fx.senior_lp, &fx.union, &fx.loan_type, &NIN)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NftRequired));
}

#[test]
fn full_loan_lifecycle_fast_draw_and_repay() {
    let fx = setup();
    let junior = 250 * NIN;
    let senior = 750 * NIN;
    seed_liquidity(&fx, junior, senior);

    let amount = 100 * NIN;
    let loan_id = BytesN::from_array(&fx.env, &[0x42; 32]);
    draw(
        &fx,
        &loan_id,
        amount,
        1_200,
        1_000,
        true,
        fx.env.ledger().timestamp() + YEAR,
    );

    assert_eq!(fx.token().balance(&fx.borrower), amount);
    let jm = fx.core().get_junior_market(&fx.union, &fx.loan_type);
    let sm = fx.core().get_senior_market(&fx.union);
    assert_eq!(jm.cash, junior - amount); // junior-first
    assert_eq!(sm.cash, senior);
    assert_eq!(fx.core().union_borrows(&fx.union), amount);

    // accrue 30 days at 12%
    fx.env
        .ledger()
        .set_timestamp(fx.env.ledger().timestamp() + 30 * 24 * 60 * 60);
    let interest = nila_math::accrued_interest(&fx.env, amount, 1_200, 30 * 24 * 60 * 60).unwrap();
    let repay = amount + interest;
    fx.mint(&fx.borrower, interest);
    let fully = fx
        .core()
        .repay_loan(&fx.borrower, &fx.union, &loan_id, &repay);
    assert!(fully);
    assert_eq!(fx.core().union_borrows(&fx.union), 0);

    let ln = fx.core().get_loan(&fx.union, &loan_id).unwrap();
    assert!(ln.liquidated);
    assert_eq!(ln.principal_paid, amount);

    // fees: lower_rate true because rate 1200 >= min 1000 → skip fees
    assert_eq!(fx.core().union_treasury(&fx.union), 0);

    // cash returned to junior (funded 100% junior)
    let jm2 = fx.core().get_junior_market(&fx.union, &fx.loan_type);
    assert_eq!(jm2.cash, junior + interest);
}

#[test]
fn interest_fees_when_below_min_rate() {
    let fx = setup();
    seed_liquidity(&fx, 200 * NIN, 200 * NIN);
    let amount = 50 * NIN;
    let loan_id = BytesN::from_array(&fx.env, &[0x43; 32]);
    // rate 800 < min 1000 → fees apply; fast_draw requires rate >= min, so manual accept
    draw(&fx, &loan_id, amount, 800, 1_000, false, 0);
    assert_eq!(fx.token().balance(&fx.borrower), 0);
    fx.core()
        .accept_loan(&fx.leader, &fx.union, &loan_id, &0u64);
    assert_eq!(fx.token().balance(&fx.borrower), amount);

    fx.env
        .ledger()
        .set_timestamp(fx.env.ledger().timestamp() + YEAR);
    let interest = nila_math::accrued_interest(&fx.env, amount, 800, YEAR).unwrap();
    fx.mint(&fx.borrower, interest);
    fx.core()
        .repay_loan(&fx.borrower, &fx.union, &loan_id, &(amount + interest));

    let expected_treasury = interest * 100 / 10_000;
    let expected_rainy = interest * 200 / 10_000;
    assert_eq!(fx.core().union_treasury(&fx.union), expected_treasury);
    assert_eq!(fx.core().union_rainy_day(&fx.union), expected_rainy);
}

#[test]
fn junior_first_loss_on_default() {
    let fx = setup();
    seed_liquidity(&fx, 100 * NIN, 400 * NIN);
    let amount = 80 * NIN;
    let loan_id = BytesN::from_array(&fx.env, &[0x44; 32]);
    draw(&fx, &loan_id, amount, 1_200, 1_000, true, 0);
    fx.core().mark_default(&fx.oracle, &fx.union, &loan_id);

    let jm = fx.core().get_junior_market(&fx.union, &fx.loan_type);
    // junior underlying haircut by 80; junior had 100
    let under = nila_math::to_underlying(&fx.env, jm.total_shares, jm.index).unwrap();
    assert_eq!(under, 20 * NIN);

    let sm = fx.core().get_senior_market(&fx.union);
    assert_eq!(sm.index, RAY); // senior untouched
}

#[test]
fn senior_unbond_14_day_gate() {
    let fx = setup();
    seed_liquidity(&fx, 0, 50 * NIN);
    let shares = fx
        .core()
        .get_investor_senior(&fx.union, &fx.senior_lp)
        .shares;
    fx.core()
        .request_unbond_senior(&fx.senior_lp, &fx.union, &shares);

    let err = fx
        .core()
        .try_claim_senior(&fx.senior_lp, &fx.union, &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::NothingToClaim));

    fx.env
        .ledger()
        .set_timestamp(fx.env.ledger().timestamp() + 14 * 24 * 60 * 60);
    fx.core().claim_senior(&fx.senior_lp, &fx.union, &0);
    assert_eq!(fx.token().balance(&fx.senior_lp), 50 * NIN);
    assert_eq!(fx.core().get_senior_market(&fx.union).total_shares, 0);
}

#[test]
fn junior_unbond_needs_idle_or_maturity() {
    let fx = setup();
    seed_liquidity(&fx, 40 * NIN, 0);
    let shares = fx
        .core()
        .get_investor_junior(&fx.union, &fx.loan_type, &fx.junior_lp)
        .shares;
    fx.core()
        .request_unbond_junior(&fx.junior_lp, &fx.union, &fx.loan_type, &shares);

    fx.env
        .ledger()
        .set_timestamp(fx.env.ledger().timestamp() + 14 * 24 * 60 * 60);
    // idle cash covers the snap and reserve (floor 0)
    fx.core()
        .claim_junior(&fx.junior_lp, &fx.union, &fx.loan_type, &0);
    assert_eq!(fx.token().balance(&fx.junior_lp), 40 * NIN);
}

#[test]
fn zero_and_pause() {
    let fx = setup();
    let err = fx
        .core()
        .try_deposit_senior(&fx.senior_lp, &fx.union, &0)
        .unwrap_err();
    assert_eq!(err, Ok(Error::AmountZero));

    fx.core().pause(&fx.admin);
    fx.mint(&fx.senior_lp, NIN);
    let err = fx
        .core()
        .try_deposit_senior(&fx.senior_lp, &fx.union, &NIN)
        .unwrap_err();
    assert_eq!(err, Ok(Error::Paused));
    fx.core().unpause(&fx.admin);
    fx.core().deposit_senior(&fx.senior_lp, &fx.union, &NIN);
}

#[test]
fn per_loan_cap() {
    let fx = setup();
    seed_liquidity(&fx, 20_000 * NIN, 0);
    fx.core().set_per_loan_cap(&(10_000 * NIN));
    let loan_id = BytesN::from_array(&fx.env, &[0x45; 32]);
    let amount = 10_001 * NIN;
    let err = fx
        .core()
        .try_draw_loan(
            &fx.borrower,
            &fx.oracle,
            &fx.union,
            &DrawParams {
                loan_id: loan_id.clone(),
                loan_type: fx.loan_type.clone(),
                amount,
                rate_bp: 1_200,
                maturity_ts: 0,
                max_amount: amount,
                min_rate_bp: 1_000,
                fast_draw: true,
                escrow_id: 0,
                sos_date: 0,
            },
        )
        .unwrap_err();
    assert_eq!(err, Ok(Error::CapExceeded));
}

#[test]
fn remove_undrawn_loan_returns_cash() {
    let fx = setup();
    seed_liquidity(&fx, 80 * NIN, 20 * NIN);
    let loan_id = BytesN::from_array(&fx.env, &[0x46; 32]);
    draw(&fx, &loan_id, 50 * NIN, 800, 1_000, false, 0);
    let cash_before = fx.core().get_junior_market(&fx.union, &fx.loan_type).cash
        + fx.core().get_senior_market(&fx.union).cash;
    fx.core().remove_loan(&fx.leader, &fx.union, &loan_id);
    let cash_after = fx.core().get_junior_market(&fx.union, &fx.loan_type).cash
        + fx.core().get_senior_market(&fx.union).cash;
    assert_eq!(cash_after, cash_before + 50 * NIN);
    assert!(fx.core().get_loan(&fx.union, &loan_id).is_none());
}

#[test]
fn cash_conservation_across_deposit_draw_repay() {
    let fx = setup();
    let j = 300 * NIN;
    let s = 200 * NIN;
    seed_liquidity(&fx, j, s);
    let amount = 120 * NIN;
    let loan_id = BytesN::from_array(&fx.env, &[0x47; 32]);
    draw(&fx, &loan_id, amount, 1_200, 1_000, true, 0);
    let markets = fx.core().get_junior_market(&fx.union, &fx.loan_type).cash
        + fx.core().get_senior_market(&fx.union).cash;
    let held = fx.token().balance(&fx.core_id);
    assert_eq!(held, markets); // disbursed amount left the contract
    assert_eq!(markets + amount, j + s);

    fx.core()
        .repay_loan(&fx.borrower, &fx.union, &loan_id, &amount);
    let markets2 = fx.core().get_junior_market(&fx.union, &fx.loan_type).cash
        + fx.core().get_senior_market(&fx.union).cash;
    assert_eq!(markets2, j + s);
    assert_eq!(fx.token().balance(&fx.core_id), j + s);
}
