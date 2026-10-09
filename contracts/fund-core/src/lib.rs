#![no_std]

mod events;
mod types;

use events::*;
use nila_common::{
    bump_instance, FxPoolClient, LandTitleClient, Loan, MarketLite, RolesClient, Tranche, TTL_BUMP,
    TTL_THRESHOLD,
};
use nila_math::{
    accrued_interest, haircut_index, index_bump, to_shares, to_underlying, BPS, DEFAULT_UNBONDING,
    DUST, RAY,
};
use soroban_sdk::{
    contract, contractimpl, contractmeta, contracttype, token::TokenClient, vec, Address, BytesN,
    Env, Vec,
};
use types::{Market, PendingUpgrade};

pub use types::{Config, Error, Investor, Position, UnionConfig};

contractmeta!(key = "binver", val = "0.2.0");
contractmeta!(
    key = "desc",
    val = "Nila fund_core — junior pool, cash-part loans, repay, default waterfall"
);
#[cfg(feature = "testnet-short-windows")]
contractmeta!(
    key = "windows",
    val = "testnet-short: one day is one second"
);

/// One day in ledger seconds. The `testnet-short-windows` build makes it one second so a
/// testnet rehearsal can reach the payback date; never deploy that build to mainnet.
#[cfg(not(feature = "testnet-short-windows"))]
pub const DAY: u64 = 86_400;
#[cfg(feature = "testnet-short-windows")]
pub const DAY: u64 = 1;

/// Payback is never earlier than drawdown + 90 days (LOAN_RULES §1).
pub const MIN_TERM: u64 = 90 * DAY;
/// Repay window after the end of season (LOAN_RULES §1).
pub const REPAY_WINDOW: u64 = 21 * DAY;
/// Loans kept per member for `loans_of` and the lock (R27, R43).
pub const MAX_LOANS_PER_MEMBER: u32 = 32;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    Paused,
    PendingUpgrade,
    UnionCfg(Address),
    SeniorMarket(Address),
    JuniorMarket(Address),
    SeniorInv(Address, Address),
    JuniorInv(Address, Address),
    Loan(Address, BytesN<32>),
    LoansOf(Address, Address),
    UnionClaimable(Address),
    UnionBorrows(Address),
    UnionTreasury(Address),
    UnionRainy(Address),
    /// Loans of the union flagged overdue and not yet repaid or defaulted (R34).
    OnHold(Address),
}

#[contract]
pub struct FundCore;

// ---------- storage ----------

fn cfg(env: &Env) -> Config {
    env.storage().instance().get(&DataKey::Config).unwrap()
}

fn save_cfg(env: &Env, c: &Config) {
    env.storage().instance().set(&DataKey::Config, c);
    bump_instance(env);
}

fn bump_key(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, TTL_THRESHOLD, TTL_BUMP);
}

fn union_cfg(env: &Env, union: &Address) -> Result<UnionConfig, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::UnionCfg(union.clone()))
        .ok_or(Error::NotExists)
}

fn market_key(junior: bool, union: &Address) -> DataKey {
    if junior {
        DataKey::JuniorMarket(union.clone())
    } else {
        DataKey::SeniorMarket(union.clone())
    }
}

fn load_market(env: &Env, junior: bool, union: &Address) -> Market {
    let mut m: Market = env
        .storage()
        .persistent()
        .get(&market_key(junior, union))
        .unwrap_or_default();
    if m.index == 0 {
        m.index = RAY;
    }
    m
}

fn save_market(env: &Env, junior: bool, union: &Address, m: &Market) {
    let key = market_key(junior, union);
    env.storage().persistent().set(&key, m);
    bump_key(env, &key);
}

fn inv_key(junior: bool, union: &Address, who: &Address) -> DataKey {
    if junior {
        DataKey::JuniorInv(union.clone(), who.clone())
    } else {
        DataKey::SeniorInv(union.clone(), who.clone())
    }
}

fn load_inv(env: &Env, junior: bool, union: &Address, who: &Address) -> Investor {
    env.storage()
        .persistent()
        .get(&inv_key(junior, union, who))
        .unwrap_or_default()
}

fn save_inv(env: &Env, junior: bool, union: &Address, who: &Address, inv: &Investor) {
    let key = inv_key(junior, union, who);
    env.storage().persistent().set(&key, inv);
    bump_key(env, &key);
}

fn load_loan(env: &Env, union: &Address, loan_id: &BytesN<32>) -> Result<Loan, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Loan(union.clone(), loan_id.clone()))
        .ok_or(Error::LoanNotExist)
}

fn save_loan(env: &Env, union: &Address, loan_id: &BytesN<32>, ln: &Loan) {
    let key = DataKey::Loan(union.clone(), loan_id.clone());
    env.storage().persistent().set(&key, ln);
    bump_key(env, &key);
}

fn loans_of(env: &Env, union: &Address, member: &Address) -> Vec<BytesN<32>> {
    env.storage()
        .persistent()
        .get(&DataKey::LoansOf(union.clone(), member.clone()))
        .unwrap_or(vec![env])
}

fn i_get(env: &Env, key: &DataKey) -> i128 {
    env.storage().persistent().get(key).unwrap_or(0)
}

fn i_add(env: &Env, key: &DataKey, delta: i128) -> Result<(), Error> {
    let next = i_get(env, key).checked_add(delta).ok_or(Error::Overflow)?;
    if next < 0 {
        return Err(Error::Overflow);
    }
    env.storage().persistent().set(key, &next);
    bump_key(env, key);
    Ok(())
}

fn on_hold_add(env: &Env, union: &Address, delta: i32) {
    let key = DataKey::OnHold(union.clone());
    let n: u32 = env.storage().persistent().get(&key).unwrap_or(0);
    let next = if delta > 0 {
        n + 1
    } else {
        n.saturating_sub(1)
    };
    env.storage().persistent().set(&key, &next);
    bump_key(env, &key);
}

fn on_hold(env: &Env, union: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::OnHold(union.clone()))
        .unwrap_or(0)
}

fn nin(env: &Env) -> TokenClient<'_> {
    TokenClient::new(env, &cfg(env).nin)
}

// ---------- guards ----------

fn require_not_paused(env: &Env) -> Result<(), Error> {
    if env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        Err(Error::Paused)
    } else {
        Ok(())
    }
}

fn require_admin(env: &Env) -> Result<Config, Error> {
    let c = cfg(env);
    c.admin.require_auth();
    Ok(c)
}

/// All money into fund_core comes through fx_pool's cash scan (executive decision, 9 Oct).
fn require_fx_pool(env: &Env) -> Result<Address, Error> {
    let fx = cfg(env).fx_pool.ok_or(Error::NotFxPool)?;
    fx.require_auth();
    Ok(fx)
}

/// Oracle facts only (D21): it never moves money.
fn require_oracle(env: &Env, who: &Address) -> Result<(), Error> {
    who.require_auth();
    if RolesClient::new(env, &cfg(env).roles).is_oracle(who) {
        Ok(())
    } else {
        Err(Error::NotOracle)
    }
}

/// A leader of that union. No admin override (R5, C11).
fn require_leader(env: &Env, union: &Address, who: &Address) -> Result<(), Error> {
    who.require_auth();
    if RolesClient::new(env, &cfg(env).roles).is_leader(union, who) {
        Ok(())
    } else {
        Err(Error::NotLeader)
    }
}

fn require_positive(amount: i128) -> Result<(), Error> {
    if amount < 0 {
        Err(Error::NegativeAmount)
    } else if amount == 0 {
        Err(Error::AmountZero)
    } else {
        Ok(())
    }
}

/// Member = holds a land title her union minted (as `balanceOf(landNFT)` on Polygon).
fn holds_title(env: &Env, union: &Address, who: &Address) -> bool {
    LandTitleClient::new(env, &cfg(env).land_title).holds_title(union, who)
}

// ---------- loan math ----------

/// Brings the loan's accrued interest up to `now` (simple interest on the cash part, R37).
fn accrue(env: &Env, ln: &mut Loan, now: u64) -> Result<(), Error> {
    let out = ln.principal - ln.principal_paid;
    let i = accrued_interest(env, out, ln.rate_bp, now.saturating_sub(ln.last_accrual_ts))
        .ok_or(Error::Overflow)?;
    ln.interest_accrued = ln.interest_accrued.checked_add(i).ok_or(Error::Overflow)?;
    ln.last_accrual_ts = now;
    Ok(())
}

/// Payback due date: max(drawdown + 90 days, end of season) + 21 days (R39).
/// `None` while the oracle has not reported the end of season.
fn due_ts(ln: &Loan) -> Option<u64> {
    if ln.maturity_ts == 0 {
        return None;
    }
    let base = core::cmp::max(ln.drawdown_ts.saturating_add(MIN_TERM), ln.maturity_ts);
    Some(base.saturating_add(REPAY_WINDOW))
}

/// Principal plus interest she owes today on her open loans in the union (R27).
fn member_owed(env: &Env, union: &Address, member: &Address) -> Result<i128, Error> {
    let now = env.ledger().timestamp();
    let mut owed: i128 = 0;
    for id in loans_of(env, union, member).iter() {
        let mut ln = load_loan(env, union, &id)?;
        if ln.closed {
            continue;
        }
        accrue(env, &mut ln, now)?;
        owed = owed
            .checked_add(ln.principal - ln.principal_paid)
            .and_then(|x| x.checked_add(ln.interest_accrued - ln.interest_paid))
            .ok_or(Error::Overflow)?;
    }
    Ok(owed)
}

fn active_shares(inv: &Investor) -> i128 {
    inv.shares - inv.pending - inv.claimable
}

fn position(env: &Env, union: &Address, member: &Address) -> Result<Position, Error> {
    let m = load_market(env, true, union);
    let inv = load_inv(env, true, union, member);
    let invested = to_underlying(env, active_shares(&inv), m.index).ok_or(Error::Overflow)?;
    let owed = member_owed(env, union, member)?;
    let locked = core::cmp::min(owed, invested);
    let all = to_underlying(env, inv.shares, m.index).ok_or(Error::Overflow)?;
    Ok(Position {
        invested,
        locked,
        free: invested - locked,
        pending_shares: inv.pending,
        claimable_shares: inv.claimable,
        earned: all - inv.cost,
        request_ts: inv.request_ts,
    })
}

// ---------- pools ----------

fn required_reserve(env: &Env, union: &Address) -> Result<(UnionConfig, i128), Error> {
    let uc = union_cfg(env, union)?;
    let claimable = i_get(env, &DataKey::UnionClaimable(union.clone()));
    let bump =
        nila_math::mul_div(env, i128::from(uc.safety_bp), claimable, BPS).ok_or(Error::Overflow)?;
    let req = claimable
        .checked_add(bump)
        .and_then(|x| x.checked_add(uc.safety_floor))
        .ok_or(Error::Overflow)?;
    Ok((uc, req))
}

/// Fees first, the rest to the junior pool: the cash part is junior money only (D5, R42).
fn distribute_interest(
    env: &Env,
    union: &Address,
    uc: &UnionConfig,
    interest: i128,
) -> Result<(), Error> {
    let fee = nila_math::mul_div(env, interest, i128::from(uc.treasury_fee_bp), BPS)
        .ok_or(Error::Overflow)?;
    let rainy = nila_math::mul_div(env, interest, i128::from(uc.rainy_fee_bp), BPS)
        .ok_or(Error::Overflow)?;
    i_add(env, &DataKey::UnionTreasury(union.clone()), fee)?;
    i_add(env, &DataKey::UnionRainy(union.clone()), rainy)?;
    let net = interest - fee - rainy;
    let mut jm = load_market(env, true, union);
    jm.cash += net;
    if jm.total_shares > 0 {
        jm.index = index_bump(env, jm.index, jm.total_shares, net).ok_or(Error::Overflow)?;
    } else {
        // No shareholders to earn it: it stays with the union.
        jm.cash -= net;
        i_add(env, &DataKey::UnionTreasury(union.clone()), net)?;
    }
    save_market(env, true, union, &jm);
    Ok(())
}

fn deposit(
    env: &Env,
    junior: bool,
    union: &Address,
    who: &Address,
    amount: i128,
) -> Result<i128, Error> {
    let fx = require_fx_pool(env)?;
    require_positive(amount)?;
    nin(env).transfer(&fx, env.current_contract_address(), &amount);
    let mut m = load_market(env, junior, union);
    m.exists = true;
    let shares = to_shares(env, amount, m.index).ok_or(Error::Overflow)?;
    m.cash += amount;
    m.total_shares += shares;
    let mut inv = load_inv(env, junior, union, who);
    inv.shares += shares;
    inv.cost += amount;
    save_market(env, junior, union, &m);
    save_inv(env, junior, union, who, &inv);
    Ok(shares)
}

fn request_unbond(
    env: &Env,
    junior: bool,
    union: &Address,
    who: &Address,
    shares: i128,
) -> Result<i128, Error> {
    let mut inv = load_inv(env, junior, union, who);
    let mut m = load_market(env, junior, union);
    let free_shares = if junior {
        // Only the free part: invested minus locked (R27).
        let p = position(env, union, who)?;
        core::cmp::min(
            active_shares(&inv),
            to_shares(env, p.free, m.index).ok_or(Error::Overflow)?,
        )
    } else {
        active_shares(&inv)
    };
    if shares > free_shares {
        return Err(Error::BadShares);
    }
    let snap = to_underlying(env, shares, m.index).ok_or(Error::Overflow)?;
    inv.pending += shares;
    inv.pending_principal_snap += snap;
    inv.request_ts = env.ledger().timestamp();
    m.claimable_principal += snap;
    i_add(env, &DataKey::UnionClaimable(union.clone()), snap)?;
    save_inv(env, junior, union, who, &inv);
    save_market(env, junior, union, &m);
    Ok(snap)
}

/// After 14 days (R28), when the pool's idle cash covers it and the reserve.
fn claim(
    env: &Env,
    junior: bool,
    union: &Address,
    who: &Address,
    max_shares: i128,
) -> Result<i128, Error> {
    if max_shares < 0 {
        return Err(Error::NegativeAmount);
    }
    let mut inv = load_inv(env, junior, union, who);
    let mut m = load_market(env, junior, union);
    if inv.pending > 0 {
        if env.ledger().timestamp() < inv.request_ts + DEFAULT_UNBONDING {
            return Err(Error::NotEligibleMaturity);
        }
        inv.claimable += inv.pending;
        inv.pending = 0;
    }
    if inv.claimable == 0 {
        return Err(Error::NothingToClaim);
    }
    let burn = if max_shares == 0 || max_shares > inv.claimable {
        inv.claimable
    } else {
        max_shares
    };
    let amount = to_underlying(env, burn, m.index).ok_or(Error::Overflow)?;
    let release = core::cmp::min(
        core::cmp::min(amount, m.claimable_principal),
        inv.pending_principal_snap,
    );
    let (uc, req) = required_reserve(env, union)?;
    let req_after = req - release;
    if m.cash < amount || (uc.hard_stop && m.cash < amount + req_after) {
        return Err(Error::ReserveStop);
    }
    let cost_out = nila_math::mul_div(env, inv.cost, burn, inv.shares).ok_or(Error::Overflow)?;
    inv.cost -= cost_out;
    inv.claimable -= burn;
    inv.shares -= burn;
    inv.pending_principal_snap -= release;
    m.total_shares -= burn;
    m.cash -= amount;
    m.claimable_principal -= release;
    i_add(env, &DataKey::UnionClaimable(union.clone()), -release)?;
    save_inv(env, junior, union, who, &inv);
    save_market(env, junior, union, &m);
    // Her trustline is opened for this payout inside fx_pool from C3 (R10).
    nin(env).transfer(&env.current_contract_address(), who, &amount);
    Ok(amount)
}

fn close_hold(env: &Env, union: &Address, ln: &Loan) {
    if ln.overdue {
        on_hold_add(env, union, -1);
    }
}

fn tranche(junior: bool) -> Tranche {
    if junior {
        Tranche::Junior
    } else {
        Tranche::Senior
    }
}

#[contractimpl]
impl FundCore {
    pub fn __constructor(
        env: Env,
        admin: Address,
        roles: Address,
        land_title: Address,
        nin_token: Address,
    ) {
        save_cfg(
            &env,
            &Config {
                admin,
                roles,
                land_title,
                nin: nin_token,
                fx_pool: None,
                upgrade_delay: 0,
            },
        );
        env.storage().instance().set(&DataKey::Paused, &false);
    }

    // ---------- admin (the trust's multisig) ----------

    pub fn pause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        Paused { paused: true }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn unpause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        Paused { paused: false }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_fx_pool(env: Env, fx_pool: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.fx_pool = Some(fx_pool.clone());
        save_cfg(&env, &c);
        FxPoolSet { fx_pool }.publish(&env);
        Ok(())
    }

    /// Lending terms of one union (R26, R34, R37, R42, R76).
    pub fn set_union_config(env: Env, union: Address, uc: UnionConfig) -> Result<(), Error> {
        require_admin(&env)?;
        if uc.max_loan < 0 || uc.gate < 0 || uc.safety_floor < 0 {
            return Err(Error::NegativeAmount);
        }
        if i128::from(uc.treasury_fee_bp) + i128::from(uc.rainy_fee_bp) > BPS
            || i128::from(uc.safety_bp) > BPS
            || i128::from(uc.rate_bp) > BPS
        {
            return Err(Error::BadArg);
        }
        let key = DataKey::UnionCfg(union.clone());
        env.storage().persistent().set(&key, &uc);
        bump_key(&env, &key);
        UnionConfigSet {
            union,
            max_loan: uc.max_loan,
            gate: uc.gate,
            rate_bp: uc.rate_bp,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- union treasury ----------

    /// One leader of the union (R17).
    pub fn withdraw_union_treasury(
        env: Env,
        leader: Address,
        union: Address,
        amount: i128,
        from_rainy: bool,
    ) -> Result<(), Error> {
        require_leader(&env, &union, &leader)?;
        require_positive(amount)?;
        let key = if from_rainy {
            DataKey::UnionRainy(union.clone())
        } else {
            DataKey::UnionTreasury(union.clone())
        };
        if amount > i_get(&env, &key) {
            return Err(Error::InsufficientCash);
        }
        i_add(&env, &key, -amount)?;
        nin(&env).transfer(&env.current_contract_address(), &leader, &amount);
        bump_instance(&env);
        Ok(())
    }

    /// fx_pool only (R15). Escrow accounting is C3's and is checked there.
    pub fn credit_escrow_nin(env: Env, union: Address, amount: i128) -> Result<(), Error> {
        require_positive(amount)?;
        require_fx_pool(&env)?;
        let mut m = load_market(&env, true, &union);
        m.cash = m.cash.checked_add(amount).ok_or(Error::Overflow)?;
        save_market(&env, true, &union, &m);
        Ok(())
    }

    /// fx_pool only: debit junior cash for an expired escrow (R15). Checked again in C3.
    pub fn burn_treasury_for_expiry(env: Env, union: Address, amount: i128) -> Result<(), Error> {
        require_positive(amount)?;
        require_fx_pool(&env)?;
        let mut m = load_market(&env, true, &union);
        m.cash = if m.cash >= amount { m.cash - amount } else { 0 };
        save_market(&env, true, &union, &m);
        Ok(())
    }

    // ---------- junior (members of the union) ----------

    /// Cash-scan INVEST: fx_pool pays the nIN in for her (R13, R25).
    pub fn deposit_junior(
        env: Env,
        union: Address,
        member: Address,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        if !holds_title(&env, &union, &member) {
            return Err(Error::NftRequired);
        }
        let shares = deposit(&env, true, &union, &member, amount)?;
        Deposit {
            investor: member,
            union,
            tranche: Tranche::Junior,
            amount,
            shares_out: shares,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Only the free part: invested minus locked (R27, R28).
    pub fn request_unbond_junior(
        env: Env,
        union: Address,
        member: Address,
        shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        member.require_auth();
        require_positive(shares)?;
        let snap = request_unbond(&env, true, &union, &member, shares)?;
        UnbondRequested {
            investor: member,
            union,
            tranche: Tranche::Junior,
            shares,
            principal_snap: snap,
        }
        .publish(&env);
        Ok(())
    }

    pub fn claim_junior(
        env: Env,
        union: Address,
        member: Address,
        max_shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        member.require_auth();
        let amount = claim(&env, true, &union, &member, max_shares)?;
        Claimed {
            investor: member,
            union,
            tranche: tranche(true),
            amount,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- senior (T2: LPs who are not members) ----------

    /// fx_pool only (mint_senior); rejects title holders (R24, R46).
    pub fn deposit_senior(
        env: Env,
        union: Address,
        lp: Address,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        if holds_title(&env, &union, &lp) {
            return Err(Error::NftForbidden);
        }
        let shares = deposit(&env, false, &union, &lp, amount)?;
        Deposit {
            investor: lp,
            union,
            tranche: Tranche::Senior,
            amount,
            shares_out: shares,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn request_unbond_senior(
        env: Env,
        union: Address,
        lp: Address,
        shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        lp.require_auth();
        require_positive(shares)?;
        let snap = request_unbond(&env, false, &union, &lp, shares)?;
        UnbondRequested {
            investor: lp,
            union,
            tranche: Tranche::Senior,
            shares,
            principal_snap: snap,
        }
        .publish(&env);
        Ok(())
    }

    pub fn claim_senior(
        env: Env,
        union: Address,
        lp: Address,
        max_shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        lp.require_auth();
        let amount = claim(&env, false, &union, &lp, max_shares)?;
        Claimed {
            investor: lp,
            union,
            tranche: tranche(false),
            amount,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- loans ----------

    /// The member signs (N10); no oracle voucher (D-c). The cash part comes from the
    /// junior pool only (R33, C8). The leader hands her the rupees from the union's cash
    /// at hand, so the loan's nIN is burned here and no nIN reaches her wallet (D9);
    /// `escrow_id` names that cash-at-hand escrow in fx_pool, 0 for none.
    pub fn draw_loan(
        env: Env,
        union: Address,
        member: Address,
        loan_id: BytesN<32>,
        cash: i128,
        escrow_id: u64,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        member.require_auth();
        require_positive(cash)?;
        if !holds_title(&env, &union, &member) {
            return Err(Error::NftRequired);
        }
        let (uc, req) = required_reserve(&env, &union)?;
        if on_hold(&env, &union) > 0 {
            return Err(Error::LendingOnHold);
        }
        if cash > uc.max_loan {
            return Err(Error::CapExceeded);
        }
        let key = DataKey::Loan(union.clone(), loan_id.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::LoanExists);
        }
        if position(&env, &union, &member)?.invested < uc.gate {
            return Err(Error::BelowGate);
        }
        let mut ids = loans_of(&env, &union, &member);
        if ids.len() >= MAX_LOANS_PER_MEMBER {
            return Err(Error::TooManyLoans);
        }
        let mut jm = load_market(&env, true, &union);
        if jm.cash < cash {
            return Err(Error::InsufficientCash);
        }
        if jm.cash < cash.checked_add(req).ok_or(Error::Overflow)? {
            return Err(Error::ReserveStop);
        }
        jm.cash -= cash;
        jm.total_borrows += cash;
        save_market(&env, true, &union, &jm);
        i_add(&env, &DataKey::UnionBorrows(union.clone()), cash)?;

        let now = env.ledger().timestamp();
        let ln = Loan {
            borrower: member.clone(),
            principal: cash,
            rate_bp: uc.rate_bp,
            drawdown_ts: now,
            maturity_ts: 0,
            principal_paid: 0,
            interest_paid: 0,
            interest_accrued: 0,
            last_accrual_ts: now,
            escrow_id,
            overdue: false,
            defaulted: false,
            closed: false,
            milestone: 0,
            milestone_digest: BytesN::from_array(&env, &[0u8; 32]),
            digest_ts: 0,
        };
        save_loan(&env, &union, &loan_id, &ln);
        ids.push_back(loan_id.clone());
        let lk = DataKey::LoansOf(union.clone(), member.clone());
        env.storage().persistent().set(&lk, &ids);
        bump_key(&env, &lk);

        nin(&env).burn(&env.current_contract_address(), &cash);
        if escrow_id != 0 {
            let fx = cfg(&env).fx_pool.ok_or(Error::NotFxPool)?;
            FxPoolClient::new(&env, &fx).resolve_escrow_cash(&escrow_id, &cash);
        }

        LoanDrawn {
            union,
            loan_id,
            borrower: member,
            cash,
            rate_bp: uc.rate_bp,
            escrow_id,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Cash-scan REPAY: fx_pool pays the nIN in (R14). Interest first, then principal back
    /// to the junior pool; at most what is owed is taken. Closes inside 0.1 nIN (R40).
    /// Returns true when the loan is closed.
    pub fn repay_loan(
        env: Env,
        union: Address,
        loan_id: BytesN<32>,
        amount: i128,
    ) -> Result<bool, Error> {
        require_not_paused(&env)?;
        let fx = require_fx_pool(&env)?;
        require_positive(amount)?;
        let uc = union_cfg(&env, &union)?;
        let mut ln = load_loan(&env, &union, &loan_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        accrue(&env, &mut ln, env.ledger().timestamp())?;
        let owed_i = ln.interest_accrued - ln.interest_paid;
        let owed_p = ln.principal - ln.principal_paid;
        let pay = core::cmp::min(amount, owed_i + owed_p);
        // The payer signs the amount it sends; interest moves with the ledger clock, so the
        // stated amount comes in and any excess goes straight back.
        let me = env.current_contract_address();
        nin(&env).transfer(&fx, &me, &amount);
        if amount > pay {
            nin(&env).transfer(&me, &fx, &(amount - pay));
        }

        let pay_i = core::cmp::min(pay, owed_i);
        let mut pay_p = pay - pay_i;
        if pay_i > 0 {
            ln.interest_paid += pay_i;
            distribute_interest(&env, &union, &uc, pay_i)?;
        }
        let left = owed_i + owed_p - pay;
        let closed = left <= DUST;
        // Dust principal left at close is written off with the borrow.
        let off_books = if closed { owed_p - pay_p } else { 0 };
        if pay_p > 0 || off_books > 0 {
            let mut jm = load_market(&env, true, &union);
            jm.cash += pay_p;
            jm.total_borrows -= core::cmp::min(jm.total_borrows, pay_p + off_books);
            save_market(&env, true, &union, &jm);
            let ub = DataKey::UnionBorrows(union.clone());
            i_add(
                &env,
                &ub,
                -core::cmp::min(i_get(&env, &ub), pay_p + off_books),
            )?;
            ln.principal_paid += pay_p + off_books;
            pay_p += off_books;
        }
        if closed {
            ln.closed = true;
            close_hold(&env, &union, &ln);
        }
        save_loan(&env, &union, &loan_id, &ln);
        LoanRepaid {
            union,
            loan_id,
            interest_paid: pay_i,
            principal_paid: pay_p,
            closed,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(closed)
    }

    /// The detected end of season (LOAN_RULES: `maturityTs` = actual EOS). Oracle only;
    /// set once; not before the drawdown and not in the future.
    pub fn report_maturity(
        env: Env,
        oracle: Address,
        union: Address,
        loan_id: BytesN<32>,
        maturity_ts: u64,
    ) -> Result<(), Error> {
        require_oracle(&env, &oracle)?;
        let mut ln = load_loan(&env, &union, &loan_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        if ln.maturity_ts != 0 {
            return Err(Error::MaturityAlreadySet);
        }
        if maturity_ts < ln.drawdown_ts || maturity_ts > env.ledger().timestamp() {
            return Err(Error::BadMaturity);
        }
        ln.maturity_ts = maturity_ts;
        save_loan(&env, &union, &loan_id, &ln);
        MaturityReported {
            union,
            loan_id,
            maturity_ts,
        }
        .publish(&env);
        Ok(())
    }

    /// Anyone, once the payback date has passed: lending in the union stops until the
    /// loan is repaid or defaulted (R34, R44). No end of season reported, no flag.
    pub fn flag_overdue(env: Env, union: Address, loan_id: BytesN<32>) -> Result<(), Error> {
        let mut ln = load_loan(&env, &union, &loan_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        if ln.overdue {
            return Err(Error::BadState);
        }
        let due = due_ts(&ln).ok_or(Error::NoMaturity)?;
        if env.ledger().timestamp() <= due {
            return Err(Error::NotDue);
        }
        ln.overdue = true;
        save_loan(&env, &union, &loan_id, &ln);
        on_hold_add(&env, &union, 1);
        LoanOverdue {
            union,
            loan_id,
            due_ts: due,
        }
        .publish(&env);
        Ok(())
    }

    /// Anyone, after the payback date (D-d), which is never earlier than
    /// `maturityTs + 21 days`. Without a reported end of season there is no default.
    /// Loss order (D10): her own junior investment first, then the junior pool, then senior.
    pub fn mark_default(env: Env, union: Address, loan_id: BytesN<32>) -> Result<(), Error> {
        let mut ln = load_loan(&env, &union, &loan_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        let due = due_ts(&ln).ok_or(Error::NoMaturity)?;
        if env.ledger().timestamp() <= due {
            return Err(Error::NotDue);
        }
        let loss = ln.principal - ln.principal_paid;

        // 1. Her investment: burn her active shares up to the loss.
        let mut jm = load_market(&env, true, &union);
        let mut inv = load_inv(&env, true, &union, &ln.borrower);
        let avail = active_shares(&inv);
        let avail_value = to_underlying(&env, avail, jm.index).ok_or(Error::Overflow)?;
        let (member_applied, burn) = if avail_value <= loss {
            (avail_value, avail)
        } else {
            (
                loss,
                core::cmp::min(
                    avail,
                    to_shares(&env, loss, jm.index).ok_or(Error::Overflow)?,
                ),
            )
        };
        if burn > 0 {
            let cost_out =
                nila_math::mul_div(&env, inv.cost, burn, inv.shares).ok_or(Error::Overflow)?;
            inv.cost -= cost_out;
            inv.shares -= burn;
            jm.total_shares -= burn;
            save_inv(&env, true, &union, &ln.borrower, &inv);
        }

        // 2. The junior pool, 3. then senior: index haircuts.
        let rest = loss - member_applied;
        let (junior_applied, idx) = haircut_index(
            &env,
            jm.index,
            to_underlying(&env, jm.total_shares, jm.index).unwrap_or(0),
            rest,
        )
        .ok_or(Error::Overflow)?;
        jm.index = idx;
        jm.total_borrows -= core::cmp::min(jm.total_borrows, loss);
        save_market(&env, true, &union, &jm);
        let mut senior_applied = 0;
        if rest > junior_applied {
            let mut sm = load_market(&env, false, &union);
            let (sa, sidx) = haircut_index(
                &env,
                sm.index,
                to_underlying(&env, sm.total_shares, sm.index).unwrap_or(0),
                rest - junior_applied,
            )
            .ok_or(Error::Overflow)?;
            senior_applied = sa;
            sm.index = sidx;
            save_market(&env, false, &union, &sm);
        }
        let ub = DataKey::UnionBorrows(union.clone());
        i_add(&env, &ub, -core::cmp::min(i_get(&env, &ub), loss))?;

        ln.defaulted = true;
        ln.closed = true;
        close_hold(&env, &union, &ln);
        save_loan(&env, &union, &loan_id, &ln);
        LoanDefaulted {
            union,
            loan_id,
            member_applied,
            junior_applied,
            senior_applied,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Money recovered after a default, paid in through fx_pool's cash scan; it goes
    /// back to the junior pool's shareholders.
    pub fn credit_recovery(
        env: Env,
        union: Address,
        loan_id: BytesN<32>,
        amount: i128,
    ) -> Result<(), Error> {
        let fx = require_fx_pool(&env)?;
        require_positive(amount)?;
        let ln = load_loan(&env, &union, &loan_id)?;
        if !ln.defaulted {
            return Err(Error::NotDefaulted);
        }
        nin(&env).transfer(&fx, env.current_contract_address(), &amount);
        let mut jm = load_market(&env, true, &union);
        if jm.total_shares > 0 {
            jm.cash += amount;
            jm.index =
                index_bump(&env, jm.index, jm.total_shares, amount).ok_or(Error::Overflow)?;
            save_market(&env, true, &union, &jm);
        } else {
            i_add(&env, &DataKey::UnionTreasury(union.clone()), amount)?;
        }
        RecoveryCredited {
            union,
            loan_id,
            recovered: amount,
        }
        .publish(&env);
        Ok(())
    }

    /// Crop milestone fact from the oracle; moves no money (D21).
    pub fn report_milestone(
        env: Env,
        oracle: Address,
        union: Address,
        loan_id: BytesN<32>,
        milestone: u32,
        milestone_digest: BytesN<32>,
    ) -> Result<(), Error> {
        require_oracle(&env, &oracle)?;
        let mut ln = load_loan(&env, &union, &loan_id)?;
        ln.milestone = milestone;
        ln.digest_ts = env.ledger().timestamp();
        ln.milestone_digest = milestone_digest.clone();
        save_loan(&env, &union, &loan_id, &ln);
        MilestoneReported {
            union,
            loan_id,
            milestone,
            milestone_digest,
        }
        .publish(&env);
        Ok(())
    }

    // ---------- upgrade (admin, timelock) ----------

    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.upgrade_delay = delay_seconds;
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        let c = require_admin(&env)?;
        if c.upgrade_delay == 0 {
            env.deployer()
                .update_current_contract_wasm(new_wasm_hash.clone());
            UpgradeApplied {
                wasm_hash: new_wasm_hash,
            }
            .publish(&env);
        } else {
            let eta = env.ledger().timestamp().saturating_add(c.upgrade_delay);
            env.storage().instance().set(
                &DataKey::PendingUpgrade,
                &PendingUpgrade {
                    wasm_hash: new_wasm_hash.clone(),
                    eta,
                },
            );
            UpgradeProposed {
                wasm_hash: new_wasm_hash,
                eta,
            }
            .publish(&env);
        }
        bump_instance(&env);
        Ok(())
    }

    pub fn execute_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        let pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .ok_or(Error::UpgradeNotReady)?;
        if env.ledger().timestamp() < pending.eta {
            return Err(Error::UpgradeNotReady);
        }
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        env.deployer()
            .update_current_contract_wasm(pending.wasm_hash.clone());
        UpgradeApplied {
            wasm_hash: pending.wasm_hash,
        }
        .publish(&env);
        Ok(())
    }

    pub fn cancel_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        Ok(())
    }

    // ---------- views ----------

    pub fn get_config(env: Env) -> Config {
        cfg(&env)
    }

    pub fn get_union_config(env: Env, union: Address) -> Option<UnionConfig> {
        union_cfg(&env, &union).ok()
    }

    pub fn get_junior_market(env: Env, union: Address) -> MarketLite {
        lite(&load_market(&env, true, &union))
    }

    pub fn get_senior_market(env: Env, union: Address) -> MarketLite {
        lite(&load_market(&env, false, &union))
    }

    /// Her junior position: invested, locked, free, pending, earned (R26, R27, R29).
    pub fn position(env: Env, union: Address, member: Address) -> Result<Position, Error> {
        position(&env, &union, &member)
    }

    pub fn get_investor_senior(env: Env, union: Address, lp: Address) -> Investor {
        load_inv(&env, false, &union, &lp)
    }

    pub fn get_loan(env: Env, union: Address, loan_id: BytesN<32>) -> Option<Loan> {
        load_loan(&env, &union, &loan_id).ok()
    }

    /// Loans per union and borrower (R43).
    pub fn loans_of(env: Env, union: Address, member: Address) -> Vec<BytesN<32>> {
        loans_of(&env, &union, &member)
    }

    /// Payback due date, or None while the end of season is not reported (R39).
    pub fn due_date(env: Env, union: Address, loan_id: BytesN<32>) -> Option<u64> {
        load_loan(&env, &union, &loan_id)
            .ok()
            .and_then(|ln| due_ts(&ln))
    }

    /// Loans of the union past payback and not repaid; lending is on hold while > 0 (R34).
    pub fn on_hold(env: Env, union: Address) -> u32 {
        on_hold(&env, &union)
    }

    pub fn union_treasury(env: Env, union: Address) -> i128 {
        i_get(&env, &DataKey::UnionTreasury(union))
    }

    pub fn union_rainy_day(env: Env, union: Address) -> i128 {
        i_get(&env, &DataKey::UnionRainy(union))
    }

    pub fn union_borrows(env: Env, union: Address) -> i128 {
        i_get(&env, &DataKey::UnionBorrows(union))
    }

    pub fn is_paused(env: Env) -> bool {
        require_not_paused(&env).is_err()
    }
}

fn lite(m: &Market) -> MarketLite {
    MarketLite {
        cash: m.cash,
        index: m.index,
        total_shares: m.total_shares,
        total_borrows: m.total_borrows,
        claimable_principal: m.claimable_principal,
    }
}

#[cfg(test)]
mod test;
