#![no_std]

mod events;
mod types;

use events::*;
use nila_common::{
    bump_instance, upgrade, CropCalendarClient, FxPoolClient, LandTitleClient, Loan,
    MachineryClient, MarketLite, Role, RolesClient, Tranche, TTL_BUMP, TTL_THRESHOLD,
};
use nila_math::{
    accrued_interest, haircut_index, index_bump, to_shares, to_underlying, BPS, DUST, RAY,
};
use soroban_sdk::{
    contract, contractimpl, contractmeta, contracttype, token::TokenClient, vec, Address, BytesN,
    Env, IntoVal, Vec,
};
use types::{
    LoanState, Market, Milestone, UnionBook, FLAG_CLOSED, FLAG_DEFAULTED, FLAG_DRAWN, FLAG_OVERDUE,
};

pub use types::{
    Config, Error, FoodToken, FoodTokenFinal, FoodTokenView, Investor, Position, UnionConfig,
    VoucherBook,
};

contractmeta!(key = "binver", val = "0.2.0");

// Demo builds only (feature `demo-upgrade-delay`): the upgrade timelock is 60 seconds, so a demo deployment can be
// upgraded in place and keep its state. Never for mainnet: the wasm carries this marker, which deploys refuse.
#[cfg(feature = "demo-upgrade-delay")]
soroban_sdk::contractmeta!(
    key = "demo_upgrade_delay",
    val = "demo-upgrade-delay: 60 s, never mainnet"
);
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
/// Share valuation rounds down; this much is forgiven at the gate (stroops).
const GATE_ROUNDING: i128 = 10;
/// Upgrade timelock at deploy: 48 hours (C15); short builds rehearse it in seconds.
#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = 2 * DAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;
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
    /// The oracle's last milestone for a loan (union, token id), written only when reported.
    Milestone(Address, BytesN<32>),
    LoansOf(Address, Address),
    /// The union's counters in one entry (C6): borrows, treasury, rainy day, earmark, on hold.
    Union(Address),
    /// Attested food token (union, Polygon token id) that a loan may be drawn against.
    FoodToken(Address, BytesN<32>),
    /// Its last draw date and lapsed flag (25 Oct), apart so stored `FoodToken` entries keep their layout.
    FoodTokenFinal(Address, BytesN<32>),
    /// A loan's voucher part (C5).
    Vouchers(Address, BytesN<32>),
    /// An owner's settled pending parts, ready to collect (union, owner).
    OwnerBalance(Address, Address),
    /// The crop calendar contract (25 Oct), apart from `Config` so its stored layout stays.
    CropCalendar,
}

#[contract]
pub struct FundCore;

// ---------- storage ----------

/// The crop code (family × 1000 + variety) and planned sowing (unix) packed in a Polygon
/// FoodToken id: bits 204–223 and 172–203 (FoodTokenUpgradeable.packTokenId).
fn crop_and_sos(token_id: &BytesN<32>) -> (u32, u64) {
    let b = token_id.to_array();
    let mut hi = [0u8; 16];
    hi.copy_from_slice(&b[..16]);
    let hi = u128::from_be_bytes(hi);
    (
        ((hi >> 76) & 0xF_FFFF) as u32,
        ((hi >> 44) & 0xFFFF_FFFF) as u64,
    )
}

fn cfg(env: &Env) -> Config {
    env.storage().instance().get(&DataKey::Config).unwrap()
}

fn save_cfg(env: &Env, c: &Config) {
    env.storage().instance().set(&DataKey::Config, c);
    bump_instance(env);
}

fn final_of(env: &Env, key: &DataKey) -> Option<FoodTokenFinal> {
    env.storage().persistent().get(key)
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

fn flag(on: bool, f: u32) -> u32 {
    if on {
        f
    } else {
        0
    }
}

fn to_state(ln: &Loan) -> LoanState {
    LoanState {
        principal: ln.principal,
        rate_bp: ln.rate_bp,
        drawdown_ts: ln.drawdown_ts,
        maturity_ts: ln.maturity_ts,
        principal_paid: ln.principal_paid,
        interest_paid: ln.interest_paid,
        interest_accrued: ln.interest_accrued,
        last_accrual_ts: ln.last_accrual_ts,
        flags: FLAG_DRAWN
            | flag(ln.overdue, FLAG_OVERDUE)
            | flag(ln.defaulted, FLAG_DEFAULTED)
            | flag(ln.closed, FLAG_CLOSED),
    }
}

fn to_view(member: &Address, st: &LoanState) -> Loan {
    Loan {
        borrower: member.clone(),
        principal: st.principal,
        rate_bp: st.rate_bp,
        drawdown_ts: st.drawdown_ts,
        maturity_ts: st.maturity_ts,
        principal_paid: st.principal_paid,
        interest_paid: st.interest_paid,
        interest_accrued: st.interest_accrued,
        last_accrual_ts: st.last_accrual_ts,
        overdue: st.flags & FLAG_OVERDUE != 0,
        defaulted: st.flags & FLAG_DEFAULTED != 0,
        closed: st.flags & FLAG_CLOSED != 0,
    }
}

/// The loan drawn against a food token: read from inside the token's entry (C6).
fn load_loan(env: &Env, union: &Address, token_id: &BytesN<32>) -> Result<Loan, Error> {
    let key = DataKey::FoodToken(union.clone(), token_id.clone());
    let ft: FoodToken = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::LoanNotExist)?;
    if !ft.loan.drawn() {
        return Err(Error::LoanNotExist);
    }
    bump_key(env, &key);
    Ok(to_view(&ft.member, &ft.loan))
}

fn save_loan(env: &Env, union: &Address, token_id: &BytesN<32>, ln: &Loan) -> Result<(), Error> {
    let key = DataKey::FoodToken(union.clone(), token_id.clone());
    let mut ft: FoodToken = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::LoanNotExist)?;
    ft.loan = to_state(ln);
    env.storage().persistent().set(&key, &ft);
    bump_key(env, &key);
    Ok(())
}

fn loans_of(env: &Env, union: &Address, member: &Address) -> Vec<BytesN<32>> {
    env.storage()
        .persistent()
        .get(&DataKey::LoansOf(union.clone(), member.clone()))
        .unwrap_or(vec![env])
}

/// A field of the union's one counters entry (`UnionBook`, C6: one entry, not five).
enum Counter {
    Borrows(Address),
    Treasury(Address),
    Rainy(Address),
    Earmark(Address),
}

fn load_union(env: &Env, union: &Address) -> UnionBook {
    env.storage()
        .persistent()
        .get(&DataKey::Union(union.clone()))
        .unwrap_or(UnionBook {
            borrows: 0,
            treasury: 0,
            rainy: 0,
            earmark: 0,
            on_hold: 0,
        })
}

fn save_union(env: &Env, union: &Address, b: &UnionBook) {
    let key = DataKey::Union(union.clone());
    env.storage().persistent().set(&key, b);
    bump_key(env, &key);
}

fn i_get(env: &Env, c: &Counter) -> i128 {
    match c {
        Counter::Borrows(u) => load_union(env, u).borrows,
        Counter::Treasury(u) => load_union(env, u).treasury,
        Counter::Rainy(u) => load_union(env, u).rainy,
        Counter::Earmark(u) => load_union(env, u).earmark,
    }
}

fn i_add(env: &Env, c: &Counter, delta: i128) -> Result<(), Error> {
    let (u, field) = match c {
        Counter::Borrows(u) => (u, 0),
        Counter::Treasury(u) => (u, 1),
        Counter::Rainy(u) => (u, 2),
        Counter::Earmark(u) => (u, 3),
    };
    let mut b = load_union(env, u);
    let slot = match field {
        0 => &mut b.borrows,
        1 => &mut b.treasury,
        2 => &mut b.rainy,
        _ => &mut b.earmark,
    };
    let next = slot.checked_add(delta).ok_or(Error::Overflow)?;
    if next < 0 {
        return Err(Error::Overflow);
    }
    *slot = next;
    save_union(env, u, &b);
    Ok(())
}

fn p_get(env: &Env, key: &DataKey) -> i128 {
    env.storage().persistent().get(key).unwrap_or(0)
}

fn p_add(env: &Env, key: &DataKey, delta: i128) -> Result<(), Error> {
    let next = p_get(env, key).checked_add(delta).ok_or(Error::Overflow)?;
    if next < 0 {
        return Err(Error::Overflow);
    }
    env.storage().persistent().set(key, &next);
    bump_key(env, key);
    Ok(())
}

fn on_hold_add(env: &Env, union: &Address, delta: i32) {
    let mut b = load_union(env, union);
    b.on_hold = if delta > 0 {
        b.on_hold + 1
    } else {
        b.on_hold.saturating_sub(1)
    };
    save_union(env, union, &b);
}

fn on_hold(env: &Env, union: &Address) -> u32 {
    load_union(env, union).on_hold
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
        // Her voucher part counts too: its value, its interest and any fee (26 Oct).
        if let Some(mut b) = load_book(env, union, &id) {
            accrue_voucher(env, &mut b, ln.rate_bp, now)?;
            owed = owed.checked_add(voucher_owed(&b)).ok_or(Error::Overflow)?;
        }
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
    i_add(env, &Counter::Treasury(union.clone()), fee)?;
    i_add(env, &Counter::Rainy(union.clone()), rainy)?;
    let net = interest - fee - rainy;
    let mut jm = load_market(env, true, union);
    jm.cash += net;
    if jm.total_shares > 0 {
        jm.index = index_bump(env, jm.index, jm.total_shares, net).ok_or(Error::Overflow)?;
    } else {
        // No shareholders to earn it: it stays with the union.
        jm.cash -= net;
        i_add(env, &Counter::Treasury(union.clone()), net)?;
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

/// After 14 days (R28), when the pool's idle cash covers it and the reserve. `to_wallet` false (the desk,
/// executive decision 22 Oct): nothing is paid to her wallet; the caller books the amount as cash.
fn claim(
    env: &Env,
    junior: bool,
    union: &Address,
    who: &Address,
    max_shares: i128,
    to_wallet: bool,
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
    // The voucher reserve is outside senior cash (26 Oct): nothing else is held back.
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
    // Over the desk (`to_wallet` false) the caller burns it and settles it against cash at hand.
    if to_wallet {
        if junior {
            pay_member(env, union, who, amount)?;
        } else {
            // Senior payouts go out through fx_pool's USDC redeem (R47, X15): T2.
            nin(env).transfer(&env.current_contract_address(), who, &amount);
        }
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
fn close_loan(env: &Env, union: &Address, token_id: &BytesN<32>, ln: &Loan) {
    if ln.overdue {
        on_hold_add(env, union, -1);
    }
    let mut ids = loans_of(env, union, &ln.borrower);
    if let Some(i) = ids.first_index_of(token_id) {
        ids.remove(i);
        let key = DataKey::LoansOf(union.clone(), ln.borrower.clone());
        env.storage().persistent().set(&key, &ids);
        bump_key(env, &key);
    }
}

fn load_book(env: &Env, union: &Address, token_id: &BytesN<32>) -> Option<VoucherBook> {
    env.storage()
        .persistent()
        .get(&DataKey::Vouchers(union.clone(), token_id.clone()))
}

fn save_book(env: &Env, union: &Address, token_id: &BytesN<32>, b: &VoucherBook) {
    let k = DataKey::Vouchers(union.clone(), token_id.clone());
    env.storage().persistent().set(&k, b);
    bump_key(env, &k);
}

/// The voucher's interest to `now` (senior's, 26 Oct): simple interest at the loan's rate on what
/// is still lent.
fn accrue_voucher(env: &Env, b: &mut VoucherBook, rate_bp: u32, now: u64) -> Result<(), Error> {
    let i = accrued_interest(
        env,
        b.value - b.repaid,
        rate_bp,
        now.saturating_sub(b.last_accrual_ts),
    )
    .ok_or(Error::Overflow)?;
    b.interest_accrued = b.interest_accrued.checked_add(i).ok_or(Error::Overflow)?;
    b.last_accrual_ts = now;
    Ok(())
}

/// What she owes on the voucher part: its value, its interest and the treasury's fee.
fn voucher_owed(b: &VoucherBook) -> i128 {
    b.value - b.repaid + b.interest_accrued - b.interest_paid + b.fee_due
}

/// Senior is repaid `pay` of the voucher's value (cash in, the loan's borrow out).
fn senior_repaid(env: &Env, union: &Address, pay: i128) {
    let mut sm = load_market(env, false, union);
    sm.cash += pay;
    sm.total_borrows -= core::cmp::min(sm.total_borrows, pay);
    save_market(env, false, union, &sm);
}

/// The voucher's interest is senior's income.
fn senior_interest(env: &Env, union: &Address, interest: i128) -> Result<(), Error> {
    if interest <= 0 {
        return Ok(());
    }
    let mut sm = load_market(env, false, union);
    sm.cash += interest;
    if sm.total_shares > 0 {
        sm.index = index_bump(env, sm.index, sm.total_shares, interest).ok_or(Error::Overflow)?;
    } else {
        // No senior shareholders to earn it: it stays with the union.
        sm.cash -= interest;
        i_add(env, &Counter::Treasury(union.clone()), interest)?;
    }
    save_market(env, false, union, &sm);
    Ok(())
}

/// At a default: her unclaimed hours lapse and their reserve goes back to senior cash. Returns it.
fn void_vouchers(
    env: &Env,
    union: &Address,
    token_id: &BytesN<32>,
    b: &mut VoucherBook,
) -> Result<i128, Error> {
    let back = b.reserve;
    if back > 0 {
        i_add(env, &Counter::Earmark(union.clone()), -back)?;
        let mut sm = load_market(env, false, union);
        sm.cash += back;
        save_market(env, false, union, &sm);
        b.reserve = 0;
    }
    if let Some(m) = cfg(env).machinery {
        MachineryClient::new(env, &m).void(union, token_id);
    }
    Ok(back)
}

/// The voucher part of a crop share, at the draw or added later (27 Oct): `hours` (centi tractor hours) at the
/// tractor hour's value, lent by senior (which must back it), set aside in the reserve and owed from `now` with
/// interest; the hours go onto her voucher. Returns the value.
fn lend_vouchers(
    env: &Env,
    union: &Address,
    member: &Address,
    token_id: &BytesN<32>,
    hours: u32,
    rate_bp: u32,
    now: u64,
) -> Result<i128, Error> {
    let mc = MachineryClient::new(env, &cfg(env).machinery.ok_or(Error::NotMachinery)?);
    let per_hour = mc.value_per_tractor_acre(union);
    let value = per_hour
        .checked_mul(i128::from(hours))
        .ok_or(Error::Overflow)?
        .checked_add(99)
        .ok_or(Error::Overflow)?
        / 100;
    let mut sm = load_market(env, false, union);
    if sm.cash < value {
        return Err(Error::NoSeniorBacking);
    }
    sm.cash -= value;
    sm.total_borrows += value;
    save_market(env, false, union, &sm);
    i_add(env, &Counter::Earmark(union.clone()), value)?;
    let mut b = load_book(env, union, token_id).unwrap_or(VoucherBook {
        hours: 0,
        per_hour,
        value: 0,
        repaid: 0,
        interest_accrued: 0,
        interest_paid: 0,
        last_accrual_ts: now,
        reserve: 0,
        fee_due: 0,
    });
    accrue_voucher(env, &mut b, rate_bp, now)?;
    // One rate per voucher for a returned hour: the hours' average.
    let all = b.hours.checked_add(hours).ok_or(Error::Overflow)?;
    b.per_hour =
        (b.per_hour * i128::from(b.hours) + per_hour * i128::from(hours)) / i128::from(all);
    b.hours = all;
    b.value += value;
    b.reserve += value;
    save_book(env, union, token_id, &b);
    mc.grant(union, member, token_id, &hours);
    Ok(value)
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
                machinery: None,
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

    /// Set once, like the fx_pool link.
    pub fn set_machinery(env: Env, machinery: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        if c.machinery.is_some() {
            return Err(Error::MachineryAlreadySet);
        }
        c.machinery = Some(machinery.clone());
        save_cfg(&env, &c);
        MachinerySet { machinery }.publish(&env);
        Ok(())
    }

    /// The crop calendar that sets a food token's window at attest (25 Oct). Admin; may be moved.
    pub fn set_crop_calendar(env: Env, calendar: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage()
            .instance()
            .set(&DataKey::CropCalendar, &calendar);
        bump_instance(&env);
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
            Counter::Rainy(union.clone())
        } else {
            Counter::Treasury(union.clone())
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

    /// C10 (executive decision, 20 Oct): a leader puts her own nIN into the union treasury, as on Polygon
    /// (`withdrawUnionTreasury(.., deposit=true)`). Not a cash scan: the rupees behind her nIN were counted
    /// when it was minted. Her trustline is authorised for this transfer only (R10).
    pub fn leader_deposit_treasury(
        env: Env,
        leader: Address,
        union: Address,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &leader)?;
        require_positive(amount)?;
        union_cfg(&env, &union)?;
        let fx = FxPoolClient::new(&env, &cfg(&env).fx_pool.ok_or(Error::NotFxPool)?);
        fx.set_member_auth(&union, &leader, &true);
        nin(&env).transfer(&leader, env.current_contract_address(), &amount);
        fx.set_member_auth(&union, &leader, &false);
        i_add(&env, &Counter::Treasury(union.clone()), amount)?;
        TreasuryMoved {
            union,
            amount,
            rainy: false,
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
        i_add(&env, &Counter::Treasury(union.clone()), amount)?;
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
            Counter::Treasury(union.clone()),
            Counter::Rainy(union.clone()),
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

    /// A leader's own junior investment, paid as nIN to her wallet (for treasury deposits). Members never
    /// hold nIN (executive decision 22 Oct, "No nIN in a member's wallet (desk only)"): they use
    /// `claim_junior_cash`.
    pub fn claim_junior(
        env: Env,
        union: Address,
        member: Address,
        max_shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &member)?;
        let amount = claim(&env, true, &union, &member, max_shares, true)?;
        Claimed {
            investor: member,
            union,
            tranche: tranche(true),
            amount,
            cash: false,
            cash_settled: 0,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Her junior investment collected over the desk (executive decision 22 Oct): she and a leader of her
    /// union both sign; the claim's nIN is burned in fund_core and booked against the union's cash at hand,
    /// as `draw_loan` does for the cash part. No nIN reaches her wallet. The leader hands the notes.
    /// Her signed QR (24 Oct): she authorises (union, max_shares) only, so any leader of her union may scan
    /// and submit it once the cash is handed over; one use by its Soroban nonce, expiring at its ledger.
    pub fn claim_junior_cash(
        env: Env,
        union: Address,
        member: Address,
        leader: Address,
        max_shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        member.require_auth_for_args((union.clone(), max_shares).into_val(&env));
        require_leader(&env, &union, &leader)?;
        let amount = claim(&env, true, &union, &member, max_shares, false)?;
        nin(&env).burn(&env.current_contract_address(), &amount);
        let fx = cfg(&env).fx_pool.ok_or(Error::NotFxPool)?;
        let settled = FxPoolClient::new(&env, &fx).settle_cash(&union, &amount);
        Claimed {
            investor: member,
            union,
            tranche: tranche(true),
            amount,
            cash: true,
            cash_settled: settled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- senior (T2: LPs who are not members) ----------

    /// fx_pool only (mint_senior): a senior investor holds no land title in any union (executive decision,
    /// 12 Oct: junior vs senior is simply holding a title or not; R46).
    pub fn deposit_senior(
        env: Env,
        union: Address,
        investor: Address,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        if LandTitleClient::new(&env, &cfg(&env).land_title).holds_any_title(&investor) {
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
        let amount = claim(&env, false, &union, &investor, max_shares, true)?;
        Claimed {
            investor,
            union,
            tranche: tranche(false),
            amount,
            cash: false,
            cash_settled: 0,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- loans ----------

    /// Food token fact (F2): the attester copies a Polygon FoodToken from the crop plan with
    /// the amount that may be drawn against it. Its window comes from the crop calendar by the
    /// token id's crop and planned sowing (25 Oct): from a week before that sowing window up to
    /// its last day. One loan per token; it can be corrected until a loan is drawn against it.
    #[allow(clippy::too_many_arguments)]
    pub fn attest_food_token(
        env: Env,
        attester: Address,
        union: Address,
        token_id: BytesN<32>,
        member: Address,
        amount: i128,
        hours: u32,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        require_attester(&env, &attester)?;
        if amount < 0 || (amount == 0 && hours == 0) {
            return Err(Error::AmountZero);
        }
        // Its window from the calendar: no calendar, or a sowing outside every window, is refused.
        let (from_date, final_date) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::CropCalendar)
            .and_then(|c| {
                let (crop, sos) = crop_and_sos(&token_id);
                CropCalendarClient::new(&env, &c).window(&crop, &sos)
            })
            .ok_or(Error::BadDates)?;
        let key = DataKey::FoodToken(union.clone(), token_id.clone());
        if let Some(old) = env.storage().persistent().get::<_, FoodToken>(&key) {
            if old.loan.drawn() {
                return Err(Error::FoodTokenUsed);
            }
        }
        let fk = DataKey::FoodTokenFinal(union.clone(), token_id.clone());
        if final_of(&env, &fk).is_some_and(|f| f.lapsed) {
            return Err(Error::FoodTokenLapsed);
        }
        // Usable in its window (25 Oct): from a week before its sowing window up to its last day.
        let ft = FoodToken {
            member: member.clone(),
            amount,
            hours,
            from_date,
            loan: LoanState::none(),
        };
        env.storage().persistent().set(&key, &ft);
        bump_key(&env, &key);
        env.storage().persistent().set(
            &fk,
            &FoodTokenFinal {
                final_date,
                lapsed: false,
            },
        );
        bump_key(&env, &fk);
        FoodTokenAttested {
            union,
            token_id,
            member,
            amount,
            hours,
            final_date,
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
        cash: i128,
        hours: u32, // centi tractor hours (executive decision 23 Oct)
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        member.require_auth();
        if cash < 0 || (cash == 0 && hours == 0) {
            return Err(Error::AmountZero);
        }
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
        // A share already taken up takes more machine hours only (executive decision 27 Oct: the union may have more
        // machines by then): cash 0, its loan open, up to its last draw date below.
        let more = ft.loan.drawn();
        if more && (cash != 0 || hours == 0 || ft.loan.flags & (FLAG_CLOSED | FLAG_DEFAULTED) != 0)
        {
            return Err(Error::FoodTokenUsed);
        }
        // Its window (25 Oct): from its opening date (a week before its sowing window, attested) up to the last date. A token
        // attested before 25 Oct keeps its season's SOS as the opening and has no last date.
        if env.ledger().timestamp() < ft.from_date {
            return Err(Error::NotOpenYet);
        }
        if let Some(f) = final_of(
            &env,
            &DataKey::FoodTokenFinal(union.clone(), token_id.clone()),
        ) {
            if f.lapsed {
                return Err(Error::FoodTokenLapsed);
            }
            if env.ledger().timestamp() > f.final_date {
                return Err(Error::TooLate);
            }
        }

        let uc = union_cfg(&env, &union)?;
        if on_hold(&env, &union) > 0 {
            return Err(Error::LendingOnHold);
        }
        if more {
            // The added hours show in machinery's VoucherGranted; their value in voucher_book.
            lend_vouchers(
                &env,
                &union,
                &member,
                &token_id,
                hours,
                ft.loan.rate_bp,
                env.ledger().timestamp(),
            )?;
            bump_instance(&env);
            return Ok(());
        }
        if cash > uc.max_loan {
            return Err(Error::CapExceeded);
        }
        // During testing the union may give more than the attested amount (executive decision, 9 Oct);
        // the ₹10,000 cap above still holds.
        if uc.enforce_token_amount && (cash > ft.amount || hours > ft.hours) {
            return Err(Error::AboveFoodToken);
        }
        // Her shares are valued down to the stroop: allow that rounding, so exactly the gate
        // invested passes (R26; found by the oracle lane's O1 rehearsal).
        if position(&env, &union, &member)?.invested + GATE_ROUNDING < uc.gate {
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
        if cash > 0 && jm.cash < cash.checked_add(req).ok_or(Error::Overflow)? {
            return Err(Error::ReserveStop);
        }
        // The voucher part, in centi tractor hours (executive decision 26 Oct): its value (the
        // tractor hour's whole rate) is lent by senior and owed from now, with interest; senior
        // sets the money aside in the reserve, which pays the owners whenever she uses the hours.
        let mut voucher = 0;
        if hours > 0 {
            voucher = lend_vouchers(
                &env,
                &union,
                &member,
                &token_id,
                hours,
                uc.rate_bp,
                env.ledger().timestamp(),
            )?;
        }
        jm.cash -= cash;
        jm.total_borrows += cash;
        save_market(&env, true, &union, &jm);
        i_add(&env, &Counter::Borrows(union.clone()), cash)?;

        let now = env.ledger().timestamp();
        // The loan lives inside its food token entry: one write, no copies (C6).
        ft.loan = LoanState {
            principal: cash,
            rate_bp: uc.rate_bp,
            drawdown_ts: now,
            maturity_ts: 0,
            principal_paid: 0,
            interest_paid: 0,
            interest_accrued: 0,
            last_accrual_ts: now,
            flags: FLAG_DRAWN,
        };
        env.storage().persistent().set(&ft_key, &ft);
        bump_key(&env, &ft_key);
        ids.push_back(token_id.clone());
        let lk = DataKey::LoansOf(union.clone(), member.clone());
        env.storage().persistent().set(&lk, &ids);
        bump_key(&env, &lk);

        let mut settled = 0;
        if cash > 0 {
            nin(&env).burn(&env.current_contract_address(), &cash);
            let fx = cfg(&env).fx_pool.ok_or(Error::NotFxPool)?;
            settled = FxPoolClient::new(&env, &fx).settle_cash(&union, &cash);
        }

        LoanDrawn {
            union,
            token_id,
            borrower: member,
            cash,
            hours,
            voucher,
            rate_bp: uc.rate_bp,
            cash_settled: settled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Cash-scan REPAY: fx_pool pays the nIN in (R14). The order (executive decision 26 Oct): the
    /// voucher part to senior (its interest, then its value), then junior (interest, then principal),
    /// then the union treasury's fee for handed-back hours. At most what is owed is taken. Closes
    /// inside 0.1 nIN (R40); her voucher hours stay hers. Returns true when closed.
    pub fn repay_loan(
        env: Env,
        union: Address,
        token_id: BytesN<32>,
        amount: i128,
    ) -> Result<bool, Error> {
        require_not_paused(&env)?;
        let fx = require_fx_pool(&env)?;
        require_positive(amount)?;
        let uc = union_cfg(&env, &union)?;
        let mut ln = load_loan(&env, &union, &token_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        let now = env.ledger().timestamp();
        accrue(&env, &mut ln, now)?;
        let mut book = load_book(&env, &union, &token_id);
        if let Some(b) = book.as_mut() {
            accrue_voucher(&env, b, ln.rate_bp, now)?;
        }
        let owed_vi = book
            .as_ref()
            .map(|b| b.interest_accrued - b.interest_paid)
            .unwrap_or(0);
        let owed_v = book.as_ref().map(|b| b.value - b.repaid).unwrap_or(0);
        let owed_f = book.as_ref().map(|b| b.fee_due).unwrap_or(0);
        let owed_i = ln.interest_accrued - ln.interest_paid;
        let owed_p = ln.principal - ln.principal_paid;
        let total = owed_vi + owed_v + owed_i + owed_p + owed_f;
        if total <= 0 {
            return Err(Error::NothingOwed);
        }
        let pay = core::cmp::min(amount, total);
        // The payer signs the amount it sends; interest moves with the ledger clock, so the
        // stated amount comes in and any excess goes straight back.
        let me = env.current_contract_address();
        nin(&env).transfer(&fx, &me, &amount);
        if amount > pay {
            nin(&env).transfer(&me, &fx, &(amount - pay));
        }

        let mut left_pay = pay;
        let pay_vi = core::cmp::min(left_pay, owed_vi);
        left_pay -= pay_vi;
        let pay_v = core::cmp::min(left_pay, owed_v);
        left_pay -= pay_v;
        let pay_i = core::cmp::min(left_pay, owed_i);
        left_pay -= pay_i;
        let mut pay_p = core::cmp::min(left_pay, owed_p);
        left_pay -= pay_p;
        let pay_f = core::cmp::min(left_pay, owed_f);
        // Always run, even for zero interest: interest moves with the ledger clock, and the
        // simulated footprint must hold the treasury and rainy-day keys either way.
        ln.interest_paid += pay_i;
        distribute_interest(&env, &union, &uc, pay_i)?;
        let left = total - pay;
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
            let ub = Counter::Borrows(union.clone());
            i_add(
                &env,
                &ub,
                -core::cmp::min(i_get(&env, &ub), pay_p + off_books),
            )?;
            ln.principal_paid += pay_p + off_books;
            pay_p += off_books;
        }
        let mut pay_s = pay_v;
        if let Some(b) = book.as_mut() {
            b.interest_paid += pay_vi;
            senior_interest(&env, &union, pay_vi)?;
            // Closing inside dust: the voucher value's last fraction is written off by senior.
            let off_v = if closed { owed_v - pay_v } else { 0 };
            senior_repaid(&env, &union, pay_v);
            if off_v > 0 {
                let mut sm = load_market(&env, false, &union);
                sm.total_borrows -= core::cmp::min(sm.total_borrows, off_v);
                let under = to_underlying(&env, sm.total_shares, sm.index).unwrap_or(0);
                sm.index = haircut_index(&env, sm.index, under, off_v)
                    .ok_or(Error::Overflow)?
                    .1;
                save_market(&env, false, &union, &sm);
            }
            b.repaid += pay_v + off_v;
            pay_s += off_v;
            b.fee_due -= pay_f;
            if closed {
                b.fee_due = 0;
            }
            if pay_f > 0 {
                i_add(&env, &Counter::Treasury(union.clone()), pay_f)?;
            }
            save_book(&env, &union, &token_id, b);
        }
        if closed {
            ln.closed = true;
            close_loan(&env, &union, &token_id, &ln);
        }
        save_loan(&env, &union, &token_id, &ln)?;
        LoanRepaid {
            union,
            token_id,
            interest_paid: pay_i + pay_vi,
            principal_paid: pay_p,
            fee_paid: pay_f,
            senior_paid: pay_s,
            closed,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(closed)
    }

    /// She hands back `units` unclaimed centi tractor hours (executive decision 26 Oct): their
    /// whole value comes off what she owes senior and their reserve goes back to senior cash; 5%
    /// of that value is added as owed to the union treasury. Only while the voucher's value is
    /// still owed (with nothing owed, unused hours simply stay hers).
    pub fn return_voucher(
        env: Env,
        member: Address,
        union: Address,
        token_id: BytesN<32>,
        units: u32,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        member.require_auth();
        let ln = load_loan(&env, &union, &token_id)?;
        if ln.borrower != member {
            return Err(Error::NotBorrower);
        }
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        let mut b = load_book(&env, &union, &token_id).ok_or(Error::NotExists)?;
        accrue_voucher(&env, &mut b, ln.rate_bp, env.ledger().timestamp())?;
        let value = core::cmp::min(
            b.per_hour
                .checked_mul(i128::from(units))
                .ok_or(Error::Overflow)?
                / 100,
            b.reserve,
        );
        if units == 0 || value <= 0 || value > b.value - b.repaid {
            return Err(Error::BadState);
        }
        MachineryClient::new(&env, &cfg(&env).machinery.ok_or(Error::NotMachinery)?)
            .take_back(&union, &token_id, &units);
        b.reserve -= value;
        i_add(&env, &Counter::Earmark(union.clone()), -value)?;
        senior_repaid(&env, &union, value);
        b.repaid += value;
        let fee = value * 5 / 100;
        b.fee_due += fee;
        save_book(&env, &union, &token_id, &b);
        VoucherReturned {
            union,
            token_id,
            units,
            value,
            fee,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(value - fee)
    }

    /// A13: the owner collects her settled pending parts to her wallet (R85); cash at the
    /// desk through a cash-out (A7).
    pub fn collect_dues(env: Env, owner: Address, union: Address) -> Result<i128, Error> {
        require_not_paused(&env)?;
        owner.require_auth();
        let k = DataKey::OwnerBalance(union.clone(), owner.clone());
        let amount = p_get(&env, &k);
        if amount <= 0 {
            return Err(Error::NothingToClaim);
        }
        p_add(&env, &k, -amount)?;
        pay_member(&env, &union, &owner, amount)?;
        DuesCollected {
            union,
            owner,
            amount,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(amount)
    }

    /// machinery only: an owner claimed hours on a voucher (D19). The owner is paid `amount` in
    /// full from the voucher's reserve at once, also after the loan is repaid (executive decision
    /// 26 Oct: the hours are hers, the debt is separate).
    pub fn pay_voucher(
        env: Env,
        union: Address,
        token_id: BytesN<32>,
        owner: Address,
        units: u32,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        cfg(&env)
            .machinery
            .ok_or(Error::NotMachinery)?
            .require_auth();
        if amount < 0 {
            return Err(Error::NegativeAmount);
        }
        let mut b = load_book(&env, &union, &token_id).ok_or(Error::NotExists)?;
        // Never more than this voucher set aside (a rate may have risen since the draw).
        let pay = core::cmp::min(amount, b.reserve);
        b.reserve -= pay;
        i_add(&env, &Counter::Earmark(union.clone()), -pay)?;
        save_book(&env, &union, &token_id, &b);
        if pay > 0 {
            pay_member(&env, &union, &owner, pay)?;
        }
        VoucherPaid {
            union,
            token_id,
            owner,
            hours: units,
            prepay: pay,
            pending: 0,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The detected end of season (LOAN_RULES: `maturityTs` = actual EOS). Oracle only;
    /// set once; not before the drawdown and not in the future.
    pub fn report_maturity(
        env: Env,
        oracle: Address,
        union: Address,
        token_id: BytesN<32>,
        maturity_ts: u64,
    ) -> Result<(), Error> {
        require_oracle(&env, &oracle)?;
        let mut ln = load_loan(&env, &union, &token_id)?;
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
        save_loan(&env, &union, &token_id, &ln)?;
        MaturityReported {
            union,
            token_id,
            maturity_ts,
        }
        .publish(&env);
        Ok(())
    }

    /// Anyone, once the payback date has passed: lending in the union stops until the
    /// loan is repaid or defaulted (R34, R44). No end of season reported, no flag.
    pub fn flag_overdue(env: Env, union: Address, token_id: BytesN<32>) -> Result<(), Error> {
        // A freeze stops repayment, so it stops the clock's consequences too.
        require_not_paused(&env)?;
        let mut ln = load_loan(&env, &union, &token_id)?;
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
        save_loan(&env, &union, &token_id, &ln)?;
        on_hold_add(&env, &union, 1);
        LoanOverdue {
            union,
            token_id,
            due_ts: due,
        }
        .publish(&env);
        Ok(())
    }

    /// Anyone, after the payback date (D-d), which is never earlier than
    /// `maturityTs + 21 days`. Without a reported end of season there is no default.
    /// Writes off all she owes, principal and interest. Loss order (D10): her own junior
    /// investment first (principal and interest), then the junior pool, then senior.
    pub fn mark_default(env: Env, union: Address, token_id: BytesN<32>) -> Result<(), Error> {
        require_not_paused(&env)?;
        let mut ln = load_loan(&env, &union, &token_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        let due = due_ts(&ln).ok_or(Error::NoMaturity)?;
        if env.ledger().timestamp() <= due {
            return Err(Error::NotDue);
        }
        // Everything she owes is written off: principal and the interest accrued to date, plus
        // her voucher part. Her unclaimed hours lapse first and their reserve goes back to senior,
        // which takes it off the voucher's value she owes (26 Oct).
        let now = env.ledger().timestamp();
        accrue(&env, &mut ln, now)?;
        let mut book = load_book(&env, &union, &token_id);
        let mut jm = load_market(&env, true, &union);
        let mut sm = load_market(&env, false, &union);
        let (mut v_rest, mut v_int) = (0, 0);
        if let Some(b) = book.as_mut() {
            accrue_voucher(&env, b, ln.rate_bp, now)?;
            save_market(&env, true, &union, &jm);
            let back = void_vouchers(&env, &union, &token_id, b)?;
            sm = load_market(&env, false, &union);
            let v_out = b.value - b.repaid;
            let cover = core::cmp::min(back, v_out);
            sm.total_borrows -= core::cmp::min(sm.total_borrows, cover);
            b.repaid += cover;
            // Hours she had paid for and never used: their reserve pays her junior principal.
            let surplus = back - cover;
            if surplus > 0 {
                let to_p = core::cmp::min(surplus, ln.principal - ln.principal_paid);
                sm.cash -= to_p;
                jm.cash += to_p;
                jm.total_borrows -= core::cmp::min(jm.total_borrows, to_p);
                ln.principal_paid += to_p;
                let ub = Counter::Borrows(union.clone());
                i_add(&env, &ub, -core::cmp::min(i_get(&env, &ub), to_p))?;
            }
            v_rest = b.value - b.repaid;
            v_int = b.interest_accrued - b.interest_paid;
        }
        let loss = ln.principal - ln.principal_paid;
        let interest = ln.interest_accrued - ln.interest_paid;

        // 1. Her investment pays first (R27, D10): the voucher's value to senior (in cash, so capped
        //    by the junior pool's idle cash), then junior principal and interest, then the voucher's
        //    interest. What cannot move stays her investment.
        let mut inv = load_inv(&env, true, &union, &ln.borrower);
        let avail = active_shares(&inv);
        let avail_value = to_underlying(&env, avail, jm.index).ok_or(Error::Overflow)?;
        let m_s = core::cmp::min(core::cmp::min(avail_value, v_rest), jm.cash);
        let m_p = core::cmp::min(avail_value - m_s, loss);
        let m_i = core::cmp::min(avail_value - m_s - m_p, interest);
        let m_vi = core::cmp::min(
            core::cmp::min(avail_value - m_s - m_p - m_i, v_int),
            jm.cash - m_s,
        );
        let member_applied = m_s + m_p + m_i + m_vi;
        let burn = if member_applied >= avail_value {
            avail
        } else {
            core::cmp::min(
                avail,
                to_shares(&env, member_applied, jm.index).ok_or(Error::Overflow)?,
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
        // What her shares cover of the interest is pool income.
        if m_i > 0 {
            if jm.total_shares > 0 {
                jm.index =
                    index_bump(&env, jm.index, jm.total_shares, m_i).ok_or(Error::Overflow)?;
            } else {
                jm.cash -= m_i;
                i_add(&env, &Counter::Treasury(union.clone()), m_i)?;
            }
        }
        // Her investment's cash to senior for the voucher value and interest she owed.
        jm.cash -= m_s + m_vi;
        sm.cash += m_s + m_vi;
        sm.total_borrows -= core::cmp::min(sm.total_borrows, v_rest);
        if m_vi > 0 && sm.total_shares > 0 {
            sm.index = index_bump(&env, sm.index, sm.total_shares, m_vi).ok_or(Error::Overflow)?;
        }

        // 2. Unpaid principal: junior pool, then senior (index haircuts). Unpaid interest
        //    beyond her investment was never booked and is written off.
        let rest = loss - m_p;
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
            let (sa, sidx) = haircut_index(
                &env,
                sm.index,
                to_underlying(&env, sm.total_shares, sm.index).unwrap_or(0),
                rest - junior_applied,
            )
            .ok_or(Error::Overflow)?;
            senior_applied = sa;
            sm.index = sidx;
        }
        // 3. The voucher value she could not cover is senior's loss (senior guaranteed it).
        let senior_prepay_lost = v_rest - m_s;
        if senior_prepay_lost > 0 {
            let sunder = to_underlying(&env, sm.total_shares, sm.index).unwrap_or(0);
            sm.index = haircut_index(&env, sm.index, sunder, senior_prepay_lost)
                .ok_or(Error::Overflow)?
                .1;
        }
        save_market(&env, false, &union, &sm);
        if let Some(b) = book.as_mut() {
            b.repaid = b.value;
            b.interest_paid = b.interest_accrued;
            b.fee_due = 0;
            save_book(&env, &union, &token_id, b);
        }
        let ub = Counter::Borrows(union.clone());
        i_add(&env, &ub, -core::cmp::min(i_get(&env, &ub), loss))?;
        let dues_paid = 0;
        let dues = 0;

        ln.defaulted = true;
        ln.closed = true;
        close_loan(&env, &union, &token_id, &ln);
        save_loan(&env, &union, &token_id, &ln)?;
        LoanDefaulted {
            union,
            token_id,
            member_applied,
            junior_applied,
            senior_applied,
            interest_written_off: interest,
            dues_paid,
            dues_unpaid: dues - dues_paid,
            senior_prepay_covered: m_s,
            senior_prepay_lost,
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
        token_id: BytesN<32>,
        amount: i128,
    ) -> Result<(), Error> {
        let fx = require_fx_pool(&env)?;
        require_positive(amount)?;
        let mut ln = load_loan(&env, &union, &token_id)?;
        if !ln.defaulted {
            return Err(Error::NotDefaulted);
        }
        // At most the principal that was written off; `principal_paid` counts recoveries too.
        if amount > ln.principal - ln.principal_paid {
            return Err(Error::CapExceeded);
        }
        ln.principal_paid += amount;
        save_loan(&env, &union, &token_id, &ln)?;
        nin(&env).transfer(&fx, env.current_contract_address(), &amount);
        let mut jm = load_market(&env, true, &union);
        if jm.total_shares > 0 {
            jm.cash += amount;
            jm.index =
                index_bump(&env, jm.index, jm.total_shares, amount).ok_or(Error::Overflow)?;
            save_market(&env, true, &union, &jm);
        } else {
            i_add(&env, &Counter::Treasury(union.clone()), amount)?;
        }
        RecoveryCredited {
            union,
            token_id,
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
        token_id: BytesN<32>,
        milestone: u32,
        milestone_digest: BytesN<32>,
    ) -> Result<(), Error> {
        require_oracle(&env, &oracle)?;
        let ln = load_loan(&env, &union, &token_id)?;
        if ln.closed {
            return Err(Error::LoanClosed);
        }
        let mk = DataKey::Milestone(union.clone(), token_id.clone());
        env.storage().persistent().set(
            &mk,
            &Milestone {
                milestone,
                digest: milestone_digest.clone(),
                ts: env.ledger().timestamp(),
            },
        );
        bump_key(&env, &mk);
        MilestoneReported {
            union,
            token_id,
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

    pub fn get_loan(env: Env, union: Address, token_id: BytesN<32>) -> Option<Loan> {
        load_loan(&env, &union, &token_id).ok()
    }

    /// The food token with its last draw date (0: none) and whether it lapsed (25 Oct).
    pub fn food_token(env: Env, union: Address, token_id: BytesN<32>) -> Option<FoodTokenView> {
        let ft: FoodToken = env
            .storage()
            .persistent()
            .get(&DataKey::FoodToken(union.clone(), token_id.clone()))?;
        let f = final_of(&env, &DataKey::FoodTokenFinal(union, token_id));
        Some(FoodTokenView {
            member: ft.member,
            amount: ft.amount,
            hours: ft.hours,
            from_date: ft.from_date,
            final_date: f.as_ref().map_or(0, |f| f.final_date),
            lapsed: f.is_some_and(|f| f.lapsed),
            loan: ft.loan,
        })
    }

    /// Anyone, after the last draw date (25 Oct): an undrawn food token lapses. The entry stays, so the id is never
    /// attested or drawn again; no funds move. The oracle relays it to Polygon (burnToken) after its own checks.
    pub fn lapse_food_token(env: Env, union: Address, token_id: BytesN<32>) -> Result<(), Error> {
        require_not_paused(&env)?;
        let ft: FoodToken = env
            .storage()
            .persistent()
            .get(&DataKey::FoodToken(union.clone(), token_id.clone()))
            .ok_or(Error::NoFoodToken)?;
        if ft.loan.drawn() {
            return Err(Error::FoodTokenUsed);
        }
        let fk = DataKey::FoodTokenFinal(union.clone(), token_id.clone());
        let mut f = final_of(&env, &fk).ok_or(Error::NotLapsedYet)?;
        if f.lapsed {
            return Err(Error::FoodTokenLapsed);
        }
        if env.ledger().timestamp() <= f.final_date {
            return Err(Error::NotLapsedYet);
        }
        f.lapsed = true;
        env.storage().persistent().set(&fk, &f);
        bump_key(&env, &fk);
        FoodTokenLapsed {
            union,
            token_id,
            member: ft.member,
            final_date: f.final_date,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Open loans per union and borrower (R43); closed ones are in the events.
    pub fn loans_of(env: Env, union: Address, member: Address) -> Vec<BytesN<32>> {
        loans_of(&env, &union, &member)
    }

    /// Payback due date, or None while the end of season is not reported (R39).
    pub fn due_date(env: Env, union: Address, token_id: BytesN<32>) -> Option<u64> {
        load_loan(&env, &union, &token_id)
            .ok()
            .and_then(|ln| due_ts(&ln))
    }

    /// Loans of the union past payback and not repaid; lending is on hold while > 0 (R34).
    pub fn on_hold(env: Env, union: Address) -> u32 {
        on_hold(&env, &union)
    }

    pub fn union_treasury(env: Env, union: Address) -> i128 {
        i_get(&env, &Counter::Treasury(union))
    }

    pub fn union_rainy_day(env: Env, union: Address) -> i128 {
        i_get(&env, &Counter::Rainy(union))
    }

    /// An owner's settled pending parts, ready to collect.
    pub fn owner_balance(env: Env, union: Address, owner: Address) -> i128 {
        p_get(&env, &DataKey::OwnerBalance(union, owner))
    }

    /// Her loan's voucher part: hours, earmark, senior prepay out, owners' pending parts.
    /// The oracle's last crop milestone for a loan (a fact; it moves no money, D21).
    pub fn milestone(env: Env, union: Address, token_id: BytesN<32>) -> Option<Milestone> {
        env.storage()
            .persistent()
            .get(&DataKey::Milestone(union, token_id))
    }

    pub fn voucher_book(env: Env, union: Address, token_id: BytesN<32>) -> Option<VoucherBook> {
        load_book(&env, &union, &token_id)
    }

    /// Senior cash held back for unclaimed voucher hours.
    pub fn senior_earmark(env: Env, union: Address) -> i128 {
        i_get(&env, &Counter::Earmark(union))
    }

    pub fn union_borrows(env: Env, union: Address) -> i128 {
        i_get(&env, &Counter::Borrows(union))
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
