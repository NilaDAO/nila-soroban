#![no_std]

mod events;
mod types;

use events::*;
use nila_common::{
    bump_instance, upgrade, FxPoolClient, LandTitleClient, Loan, MarketLite, Role, RolesClient,
    Tranche, TTL_BUMP, TTL_THRESHOLD,
};
use nila_math::{
    accrued_interest, haircut_index, index_bump, to_shares, to_underlying, BPS, DUST, RAY,
};
use soroban_sdk::{
    contract, contractimpl, contractmeta, contracttype, token::TokenClient, vec, Address, BytesN,
    Env, Vec,
};
use types::Market;

pub use types::{Config, Error, FoodToken, Investor, Position, UnionConfig};

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
/// Highest loan rate any union may be given (C14, executive decision 10 Oct): 12.5% simple. The legal
/// maximum notified under TN Money-Lenders Act s.7 is not yet sourced; once counsel confirms
/// it, this is raised by a timelocked upgrade, never by a parameter.
pub const MAX_RATE_BP: u32 = 1250;
/// Upgrade timelock at deploy: 48 hours (C15); short builds rehearse it in seconds.
pub const UPGRADE_DELAY: u64 = 2 * DAY;
/// Unbond request to claim (R28).
pub const UNBONDING: u64 = 14 * DAY;
/// Repay window after the end of season (LOAN_RULES §1).
pub const REPAY_WINDOW: u64 = 21 * DAY;
/// Open loans per member, for `loans_of` and the lock (R27, R43).
pub const MAX_LOANS_PER_MEMBER: u32 = 32;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    Paused,
    UnionCfg(Address),
    SeniorMarket(Address),
    JuniorMarket(Address),
    SeniorInv(Address, Address),
    JuniorInv(Address, Address),
    Loan(Address, BytesN<32>),
    LoansOf(Address, Address),
    UnionBorrows(Address),
    UnionTreasury(Address),
    UnionRainy(Address),
    /// Loans of the union flagged overdue and not yet repaid or defaulted (R34).
    OnHold(Address),
    /// Attested food token (union, Polygon token id) that a loan may be drawn against.
    FoodToken(Address, BytesN<32>),
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
    if !m.exists {
        m.index = RAY;
    }
    m
}

/// A pool written off in full keeps index 0: its old shares are worth nothing, and it
/// takes no new money (a fresh deposit would buy shares at par next to them).
fn require_live(m: &Market) -> Result<(), Error> {
    if m.exists && m.index == 0 {
        Err(Error::PoolWrittenOff)
    } else {
        Ok(())
    }
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

/// Reads extend live entries: a passive investor or a loan waiting for its end of season
/// may not be written for longer than the TTL.
fn load_inv(env: &Env, junior: bool, union: &Address, who: &Address) -> Investor {
    let key = inv_key(junior, union, who);
    let inv: Option<Investor> = env.storage().persistent().get(&key);
    if inv.is_some() {
        bump_key(env, &key);
    }
    inv.unwrap_or_default()
}

fn save_inv(env: &Env, junior: bool, union: &Address, who: &Address, inv: &Investor) {
    let key = inv_key(junior, union, who);
    env.storage().persistent().set(&key, inv);
    bump_key(env, &key);
}

fn load_loan(env: &Env, union: &Address, loan_id: &BytesN<32>) -> Result<Loan, Error> {
    let key = DataKey::Loan(union.clone(), loan_id.clone());
    let ln: Loan = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::LoanNotExist)?;
    bump_key(env, &key);
    Ok(ln)
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

/// The attester writes facts only (D21), from the one role list in `roles`.
fn require_attester(env: &Env, who: &Address) -> Result<(), Error> {
    who.require_auth();
    if RolesClient::new(env, &cfg(env).roles).has_role(who, &Role::Attester) {
        Ok(())
    } else {
        Err(Error::NotAttester)
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

/// Reserve held for one tranche's own unbond queue; the other tranche is never read (I3).
fn reserve_for(env: &Env, uc: &UnionConfig, claimable: i128) -> Result<i128, Error> {
    let bump =
        nila_math::mul_div(env, i128::from(uc.safety_bp), claimable, BPS).ok_or(Error::Overflow)?;
    let req = claimable
        .checked_add(bump)
        .and_then(|x| x.checked_add(uc.safety_floor))
        .ok_or(Error::Overflow)?;
    Ok(req)
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
    require_live(&m)?;
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
        if env.ledger().timestamp() < inv.request_ts + UNBONDING {
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
    if m.cash < amount {
        return Err(Error::InsufficientCash);
    }
    let uc = union_cfg(env, union)?;
    let req_after = reserve_for(env, &uc, m.claimable_principal - release)?;
    if uc.hard_stop && m.cash < amount + req_after {
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
    save_inv(env, junior, union, who, &inv);
    save_market(env, junior, union, &m);
    if junior {
        pay_member(env, union, who, amount)?;
    } else {
        // Senior payouts go out through fx_pool's USDC redeem (R47, X15): T2.
        nin(env).transfer(&env.current_contract_address(), who, &amount);
    }
    Ok(amount)
}

/// R10: fx_pool (the nIN SAC admin) authorises her trustline for this payout only.
fn pay_member(env: &Env, union: &Address, who: &Address, amount: i128) -> Result<(), Error> {
    let fx = FxPoolClient::new(env, &cfg(env).fx_pool.ok_or(Error::NotFxPool)?);
    fx.set_member_auth(union, who, &true);
    nin(env).transfer(&env.current_contract_address(), who, &amount);
    fx.set_member_auth(union, who, &false);
    Ok(())
}

/// A closed loan lifts its hold and leaves her open-loan list (the events keep the history).
fn close_loan(env: &Env, union: &Address, loan_id: &BytesN<32>, ln: &Loan) {
    if ln.overdue {
        on_hold_add(env, union, -1);
    }
    let mut ids = loans_of(env, union, &ln.borrower);
    if let Some(i) = ids.first_index_of(loan_id) {
        ids.remove(i);
        let key = DataKey::LoansOf(union.clone(), ln.borrower.clone());
        env.storage().persistent().set(&key, &ids);
        bump_key(env, &key);
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
            },
        );
        env.storage().instance().set(&DataKey::Paused, &false);
        upgrade::init(&env, UPGRADE_DELAY);
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

    /// Set once: fx_pool can burn treasury cover and gates every payout, so only a
    /// timelocked upgrade may repoint it.
    pub fn set_fx_pool(env: Env, fx_pool: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        if c.fx_pool.is_some() {
            return Err(Error::FxPoolAlreadySet);
        }
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
        {
            return Err(Error::BadArg);
        }
        if uc.rate_bp > MAX_RATE_BP {
            return Err(Error::AboveMaxRate);
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
        require_not_paused(&env)?;
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
        let fx = cfg(&env).fx_pool.ok_or(Error::NotFxPool)?;
        // Cash at hand never exceeds the treasury (R15).
        if !from_rainy
            && i_get(&env, &key) - amount < FxPoolClient::new(&env, &fx).cash_at_hand(&union)
        {
            return Err(Error::CashAtHand);
        }
        i_add(&env, &key, -amount)?;
        pay_member(&env, &union, &leader, amount)?;
        TreasuryMoved {
            union,
            amount: -amount,
            rainy: from_rainy,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Cash-scan TREASURY: the union's own rupees into its treasury (R17), via fx_pool.
    pub fn deposit_union_treasury(env: Env, union: Address, amount: i128) -> Result<(), Error> {
        require_not_paused(&env)?;
        let fx = require_fx_pool(&env)?;
        require_positive(amount)?;
        union_cfg(&env, &union)?;
        nin(&env).transfer(&fx, env.current_contract_address(), &amount);
        i_add(&env, &DataKey::UnionTreasury(union.clone()), amount)?;
        TreasuryMoved {
            union,
            amount,
            rainy: false,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// fx_pool only: an escrow expired unsettled (R15). The union kept the rupees, so the
    /// same nIN burns from its treasury, then its rainy-day fund. Returns what was covered.
    pub fn cover_expired_escrow(env: Env, union: Address, amount: i128) -> Result<i128, Error> {
        // While paused no draw can settle cash at hand, so no expiry may burn the treasury.
        require_not_paused(&env)?;
        require_fx_pool(&env)?;
        if amount < 0 {
            return Err(Error::NegativeAmount);
        }
        let mut left = amount;
        for key in [
            DataKey::UnionTreasury(union.clone()),
            DataKey::UnionRainy(union.clone()),
        ] {
            let take = core::cmp::min(left, i_get(&env, &key));
            if take > 0 {
                i_add(&env, &key, -take)?;
                left -= take;
            }
        }
        let covered = amount - left;
        if covered > 0 {
            nin(&env).burn(&env.current_contract_address(), &covered);
        }
        EscrowCovered {
            union,
            amount,
            covered,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(covered)
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

    /// fx_pool only (mint_senior): a senior investor, never a member; title holders refused (R46).
    pub fn deposit_senior(
        env: Env,
        union: Address,
        investor: Address,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        if holds_title(&env, &union, &investor) {
            return Err(Error::NftForbidden);
        }
        let shares = deposit(&env, false, &union, &investor, amount)?;
        Deposit {
            investor,
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
        investor: Address,
        shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        require_positive(shares)?;
        let snap = request_unbond(&env, false, &union, &investor, shares)?;
        UnbondRequested {
            investor,
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
        investor: Address,
        max_shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        let amount = claim(&env, false, &union, &investor, max_shares)?;
        Claimed {
            investor,
            union,
            tranche: tranche(false),
            amount,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- loans ----------

    /// Food token fact (F2): the attester copies a Polygon FoodToken from the crop plan with
    /// the amount that may be drawn against it and the date loans open. One loan per token;
    /// it can be corrected until a loan is drawn against it.
    pub fn attest_food_token(
        env: Env,
        attester: Address,
        union: Address,
        token_id: BytesN<32>,
        member: Address,
        amount: i128,
        from_date: u64,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        require_attester(&env, &attester)?;
        require_positive(amount)?;
        let key = DataKey::FoodToken(union.clone(), token_id.clone());
        if let Some(old) = env.storage().persistent().get::<_, FoodToken>(&key) {
            if old.loan_id.is_some() {
                return Err(Error::FoodTokenUsed);
            }
        }
        let ft = FoodToken {
            member: member.clone(),
            amount,
            from_date,
            loan_id: None,
        };
        env.storage().persistent().set(&key, &ft);
        bump_key(&env, &key);
        FoodTokenAttested {
            union,
            token_id,
            member,
            amount,
            from_date,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The member signs (N10); no oracle voucher (D-c). The loan is drawn against an attested
    /// food token of hers: from its date, once, and at most its amount when the union's
    /// `enforce_token_amount` is on. The cash part comes from the
    /// junior pool only (R33, C8). The leader hands her the rupees from the union's cash
    /// at hand, so the loan's nIN is burned here, the cash at hand in fx_pool is settled
    /// oldest first, and no nIN reaches her wallet (D9, R35).
    pub fn draw_loan(
        env: Env,
        union: Address,
        member: Address,
        token_id: BytesN<32>,
        loan_id: BytesN<32>,
        cash: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        member.require_auth();
        require_positive(cash)?;
        if !holds_title(&env, &union, &member) {
            return Err(Error::NftRequired);
        }
        let ft_key = DataKey::FoodToken(union.clone(), token_id.clone());
        let mut ft: FoodToken = env
            .storage()
            .persistent()
            .get(&ft_key)
            .ok_or(Error::NoFoodToken)?;
        if ft.member != member {
            return Err(Error::NoFoodToken);
        }
        if ft.loan_id.is_some() {
            return Err(Error::FoodTokenUsed);
        }
        if env.ledger().timestamp() < ft.from_date {
            return Err(Error::NotOpenYet);
        }

        let uc = union_cfg(&env, &union)?;
        if on_hold(&env, &union) > 0 {
            return Err(Error::LendingOnHold);
        }
        if cash > uc.max_loan {
            return Err(Error::CapExceeded);
        }
        // During testing the union may give more than the attested amount (executive decision, 9 Oct);
        // the ₹10,000 cap above still holds.
        if uc.enforce_token_amount && cash > ft.amount {
            return Err(Error::AboveFoodToken);
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
        require_live(&jm)?;
        let req = reserve_for(&env, &uc, jm.claimable_principal)?;
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
            token_id: token_id.clone(),
            principal: cash,
            rate_bp: uc.rate_bp,
            drawdown_ts: now,
            maturity_ts: 0,
            principal_paid: 0,
            interest_paid: 0,
            interest_accrued: 0,
            last_accrual_ts: now,
            overdue: false,
            defaulted: false,
            closed: false,
            milestone: 0,
            milestone_digest: BytesN::from_array(&env, &[0u8; 32]),
            digest_ts: 0,
        };
        save_loan(&env, &union, &loan_id, &ln);
        ft.loan_id = Some(loan_id.clone());
        env.storage().persistent().set(&ft_key, &ft);
        bump_key(&env, &ft_key);
        ids.push_back(loan_id.clone());
        let lk = DataKey::LoansOf(union.clone(), member.clone());
        env.storage().persistent().set(&lk, &ids);
        bump_key(&env, &lk);

        nin(&env).burn(&env.current_contract_address(), &cash);
        let fx = cfg(&env).fx_pool.ok_or(Error::NotFxPool)?;
        let settled = FxPoolClient::new(&env, &fx).settle_cash(&union, &cash);

        LoanDrawn {
            union,
            loan_id,
            borrower: member,
            token_id,
            cash,
            rate_bp: uc.rate_bp,
            cash_settled: settled,
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
        // Always run, even for zero interest: interest moves with the ledger clock, and the
        // simulated footprint must hold the treasury and rainy-day keys either way.
        ln.interest_paid += pay_i;
        distribute_interest(&env, &union, &uc, pay_i)?;
        let left = owed_i + owed_p - pay;
        let closed = left <= DUST;
        // Dust principal left at close is written off with the borrow.
        let off_books = if closed { owed_p - pay_p } else { 0 };
        if pay_p > 0 || off_books > 0 {
            let mut jm = load_market(&env, true, &union);
            jm.cash += pay_p;
            jm.total_borrows -= core::cmp::min(jm.total_borrows, pay_p + off_books);
            if off_books > 0 {
                let under = to_underlying(&env, jm.total_shares, jm.index).unwrap_or(0);
                jm.index = haircut_index(&env, jm.index, under, off_books)
                    .ok_or(Error::Overflow)?
                    .1;
            }
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
            close_loan(&env, &union, &loan_id, &ln);
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
        // A freeze stops repayment, so it stops the clock's consequences too.
        require_not_paused(&env)?;
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
    /// Writes off all she owes, principal and interest. Loss order (D10): her own junior
    /// investment first (principal and interest), then the junior pool, then senior.
    pub fn mark_default(env: Env, union: Address, loan_id: BytesN<32>) -> Result<(), Error> {
        require_not_paused(&env)?;
        let mut ln = load_loan(&env, &union, &loan_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        let due = due_ts(&ln).ok_or(Error::NoMaturity)?;
        if env.ledger().timestamp() <= due {
            return Err(Error::NotDue);
        }
        // Everything she owes is written off: principal and the interest accrued to date.
        accrue(&env, &mut ln, env.ledger().timestamp())?;
        let loss = ln.principal - ln.principal_paid;
        let interest = ln.interest_accrued - ln.interest_paid;

        // 1. Her investment covers what she owes, principal and interest (R27, D10).
        let mut jm = load_market(&env, true, &union);
        let mut inv = load_inv(&env, true, &union, &ln.borrower);
        let owed = loss + interest;
        let avail = active_shares(&inv);
        let avail_value = to_underlying(&env, avail, jm.index).ok_or(Error::Overflow)?;
        let (member_applied, burn) = if avail_value <= owed {
            (avail_value, avail)
        } else {
            (
                owed,
                core::cmp::min(
                    avail,
                    to_shares(&env, owed, jm.index).ok_or(Error::Overflow)?,
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
        jm.total_borrows -= core::cmp::min(jm.total_borrows, loss);
        // Her shares pay principal first; what they cover of the interest goes to the pool.
        let member_interest = member_applied - core::cmp::min(member_applied, loss);
        if member_interest > 0 {
            if jm.total_shares > 0 {
                jm.index = index_bump(&env, jm.index, jm.total_shares, member_interest)
                    .ok_or(Error::Overflow)?;
            } else {
                jm.cash -= member_interest;
                i_add(
                    &env,
                    &DataKey::UnionTreasury(union.clone()),
                    member_interest,
                )?;
            }
        }

        // 2. Then the junior pool, 3. then senior: index haircuts on the unpaid principal.
        // Unpaid interest beyond her investment was never booked and is written off.
        let rest = loss - core::cmp::min(member_applied, loss);
        let (junior_applied, idx) = haircut_index(
            &env,
            jm.index,
            to_underlying(&env, jm.total_shares, jm.index).unwrap_or(0),
            rest,
        )
        .ok_or(Error::Overflow)?;
        jm.index = idx;
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
        close_loan(&env, &union, &loan_id, &ln);
        save_loan(&env, &union, &loan_id, &ln);
        LoanDefaulted {
            union,
            loan_id,
            member_applied,
            junior_applied,
            senior_applied,
            interest_written_off: interest,
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
        let mut ln = load_loan(&env, &union, &loan_id)?;
        if !ln.defaulted {
            return Err(Error::NotDefaulted);
        }
        // At most the principal that was written off; `principal_paid` counts recoveries too.
        if amount > ln.principal - ln.principal_paid {
            return Err(Error::CapExceeded);
        }
        ln.principal_paid += amount;
        save_loan(&env, &union, &loan_id, &ln);
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
        if ln.closed {
            return Err(Error::LoanClosed);
        }
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

    // ---------- upgrade (admin, timelock, nila_common::upgrade) ----------

    /// Raising applies at once and voids a filed lowering; lowering applies only when set again
    /// after the current delay, within 7 days; at most 30 days.
    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        require_admin(&env)?;
        if !upgrade::set_delay(&env, delay_seconds) {
            return Err(Error::BadDelay);
        }
        bump_instance(&env);
        Ok(())
    }

    pub fn upgrade_delay(env: Env) -> u64 {
        upgrade::delay(&env)
    }

    pub fn pending_upgrade(env: Env) -> Option<upgrade::PendingUpgrade> {
        upgrade::pending(&env)
    }

    /// A filed lowering of the delay, if any.
    pub fn pending_delay(env: Env) -> Option<upgrade::PendingDelay> {
        upgrade::pending_delay(&env)
    }

    /// Files the new wasm; `execute_upgrade` applies it once the delay has passed.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        require_admin(&env)?;
        upgrade::propose(&env, new_wasm_hash);
        bump_instance(&env);
        Ok(())
    }

    pub fn execute_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        if !upgrade::execute(&env) {
            return Err(Error::UpgradeNotReady);
        }
        Ok(())
    }

    pub fn cancel_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        upgrade::cancel(&env);
        bump_instance(&env);
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

    pub fn get_investor_senior(env: Env, union: Address, investor: Address) -> Investor {
        load_inv(&env, false, &union, &investor)
    }

    pub fn get_loan(env: Env, union: Address, loan_id: BytesN<32>) -> Option<Loan> {
        load_loan(&env, &union, &loan_id).ok()
    }

    pub fn food_token(env: Env, union: Address, token_id: BytesN<32>) -> Option<FoodToken> {
        env.storage()
            .persistent()
            .get(&DataKey::FoodToken(union, token_id))
    }

    /// Open loans per union and borrower (R43); closed ones are in the events.
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
