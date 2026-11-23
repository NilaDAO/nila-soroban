#![no_std]
//! Senior (executive decision 23 Nov): a guarantee pool only, from day 0. Senior never lends.
//!
//! - Investors the trust accepted deposit USDC and hold shares, valued by one USDC index (ray math, as
//!   fund_core's pools): value = shares × index. One pool, one index for every investor.
//! - The USDC stays in this contract. It grows by the guarantee fee: fund_core sets a share of every
//!   voucher's interest aside (in rupees); a member LP of the union takes those rupees at the office and pays
//!   the USDC in here (`pay_fee`, with a leader): the index rises.
//! - It leaves only to cover a loss (`propose_cover`, 48 hours, then `execute_cover`: the index falls) or to
//!   an investor who exits (`request_redeem`, 14 days, then `redeem`: shares × index at that moment, within the
//!   daily cap). There is no other way out and no admin withdrawal; but a 48-hour upgrade (trust,
//!   `nila_common::upgrade`) can change anything, so every `UpgradeProposed` here is watched.
//! - The machine prepay and the cash of a crop agreement come from junior money (members, sold agreements, and
//!   during the development period Nila itself as a junior investor), never from here.
//! - Unaudited: deposits stop at the trust's cap (0 until set).
//! - Error codes start at 101, apart from fund_core's and fx_pool's; removed codes are not reused.

use nila_common::{bump_instance, upgrade, FundCoreClient, RolesClient, TTL_BUMP, TTL_THRESHOLD};
use nila_math::{haircut_index, index_bump, to_shares, to_underlying, RAY};
use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contractmeta,
    contracttype, token::TokenClient, Address, BytesN, Env,
};

contractmeta!(key = "binver", val = "0.3.0");

// Demo builds only (feature `demo-upgrade-delay`): the upgrade timelock is 60 seconds, so a demo deployment can be
// upgraded in place and keep its state. Never for mainnet: the wasm carries this marker, which deploys refuse.
#[cfg(feature = "demo-upgrade-delay")]
soroban_sdk::contractmeta!(
    key = "demo_upgrade_delay",
    val = "demo-upgrade-delay: 60 s, never mainnet"
);
contractmeta!(
    key = "desc",
    val = "Nila senior: a guarantee pool only; USDC grows by the vouchers' guarantee fee, pays only loss cover"
);

#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = upgrade::DEFAULT_DELAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;

/// nIN per USDC at 7 decimals, as fx_pool's rate.
pub const RATE_ONE: i128 = 10_000_000;
/// Senior USDC out a day (UTC) at launch: 2,500 USDC (executive decision 19 Nov); `set_daily_cap` changes it.
pub const DEFAULT_DAILY_CAP: i128 = 2_500 * 10_000_000;
const DAY: u64 = 86_400;
/// An exit waits 14 days, so no investor leaves just ahead of a loss it knows of.
pub const UNBOND: u64 = 14 * DAY;
/// A loss cover waits 48 hours after the trust proposes it, so it can be seen and stopped.
pub const COVER_DELAY: u64 = 2 * DAY;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 101,
    Paused = 102,
    AmountZero = 103,
    /// Not an investor the trust accepted.
    NotAccepted = 104,
    /// The deposit would take the senior USDC above the trust's cap.
    AboveCap = 105,
    /// No rate in force (fx_pool), or it ran out.
    StaleRate = 106,
    /// The LP pays less than the rate less its fee.
    BelowFix = 107,
    NotLeader = 108,
    /// Not an accepted LP of the union, or its identity is not checked (K1).
    NotLp = 109,
    // 110 (VaultAlreadySet), 111 (NoVault), 116 (UnknownUnion), 117 (AboveGuaranteeCap) removed 23 Nov.
    /// An investor's residency is set once.
    TagFixed = 112,
    UpgradeNotReady = 113,
    BadDelay = 114,
    Overflow = 115,
    /// The LP's account did not pay the USDC it signed for (re-review, 3 Nov).
    NotPaid = 118,
    /// Today's senior USDC out would pass the daily cap: the rest waits for the next day (UTC).
    AboveDailyCap = 119,
    /// No shares yet: a guarantee fee has no one to grow for and stays in rupees in fund_core.
    NoShares = 120,
    /// The exit's 14 days, or the cover's 48 hours, have not passed.
    NotReady = 121,
    /// No cover proposed, or the pool does not hold it.
    NoCover = 122,
    /// More shares than the investor holds free.
    AboveShares = 123,
}

/// Residency (R24), fixed at acceptance.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Tag {
    Offshore = 0,
    Domestic = 1,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Investor {
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
    pub tag: Tag,
}

/// An investor's shares: free, and asked out (`pending`, since `request_ts`).
#[contracttype]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Holding {
    pub shares: i128,
    pub pending: i128,
    pub request_ts: u64,
}

/// A loss cover the trust proposed: `usdc` to `to` (an LP who brings the rupees, or the union) for `union`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cover {
    pub union: Address,
    pub to: Address,
    pub usdc: i128,
    pub eta: u64,
}

/// fx_pool's rate in force (INR per USD, 7 decimals).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fix {
    pub inr_per_usd: i128,
    pub valid_until: u64,
    pub set_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpOf {
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
}

#[contractclient(name = "FxClient")]
pub trait FxTrait {
    fn rate() -> Option<Fix>;
    fn lp_member(union: Address, lp: Address) -> Option<LpOf>;
    fn id_checked(account: Address) -> bool;
    fn lp_fee() -> u32;
}

#[contractclient(name = "LpAccountClient")]
pub trait LpAccountTrait {
    fn send_usdc(to: Address, amount: i128);
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Usdc,
    FundCore,
    FxPool,
    Roles,
    Paused,
    /// The most senior USDC this contract takes in, in all (the trust; 0 until set).
    Cap,
    /// USDC the pool holds for its investors: deposits and fees, less covers and exits.
    Total,
    Investor(Address),
    /// Senior USDC out a day (19 Nov); unset = `DEFAULT_DAILY_CAP`. Instance.
    DailyCap,
    /// USDC `redeem` paid out on a day (day index). Temporary.
    DayOut(u64),
    /// The pool's index (ray); unset = `RAY`. Instance.
    Index,
    /// All shares, free and pending. Instance.
    TotalShares,
    Holding(Address),
    /// The one loss cover waiting (instance).
    PendingCover,
}

#[contractevent]
pub struct AdminSet {
    pub admin: Address,
}

#[contractevent]
pub struct PausedSet {
    pub paused: bool,
}

#[contractevent]
pub struct CapSet {
    pub usdc: i128,
}

#[contractevent]
pub struct DailyCapSet {
    pub usdc: i128,
}

#[contractevent]
pub struct InvestorSet {
    #[topic]
    pub investor: Address,
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
    pub tag: Tag,
}

#[contractevent]
pub struct Deposited {
    #[topic]
    pub investor: Address,
    pub tag: Tag,
    pub usdc: i128,
    pub shares: i128,
}

#[contractevent]
pub struct RedeemRequested {
    #[topic]
    pub investor: Address,
    pub shares: i128,
}

#[contractevent]
pub struct Redeemed {
    #[topic]
    pub investor: Address,
    pub shares: i128,
    pub usdc: i128,
}

#[contractevent]
pub struct FeePaid {
    #[topic]
    pub union: Address,
    pub leader: Address,
    pub lp: Address,
    pub inr: i128,
    pub usdc: i128,
    pub cash_settled: i128,
    pub index: i128,
}

#[contractevent]
pub struct CoverProposed {
    #[topic]
    pub union: Address,
    pub to: Address,
    pub usdc: i128,
    pub eta: u64,
}

#[contractevent]
pub struct CoverPaid {
    #[topic]
    pub union: Address,
    pub to: Address,
    pub usdc: i128,
    pub index: i128,
}

#[contractevent]
pub struct CoverCancelled {
    #[topic]
    pub union: Address,
    pub usdc: i128,
}

fn get(env: &Env, k: &DataKey) -> Address {
    env.storage().instance().get(k).unwrap()
}

fn i_get(env: &Env, k: &DataKey) -> i128 {
    env.storage().instance().get(k).unwrap_or(0)
}

fn i_set(env: &Env, k: &DataKey, v: i128) {
    env.storage().instance().set(k, &v);
}

fn index(env: &Env) -> i128 {
    env.storage().instance().get(&DataKey::Index).unwrap_or(RAY)
}

fn bump(env: &Env, k: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(k, TTL_THRESHOLD, TTL_BUMP);
}

fn require_admin(env: &Env) -> Result<Address, Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(admin)
}

fn require_not_paused(env: &Env) -> Result<(), Error> {
    if env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        return Err(Error::Paused);
    }
    Ok(())
}

fn load_investor(env: &Env, who: &Address) -> Option<Investor> {
    env.storage()
        .persistent()
        .get(&DataKey::Investor(who.clone()))
}

fn load_holding(env: &Env, who: &Address) -> Holding {
    env.storage()
        .persistent()
        .get(&DataKey::Holding(who.clone()))
        .unwrap_or_default()
}

fn save_holding(env: &Env, who: &Address, h: &Holding) {
    let k = DataKey::Holding(who.clone());
    if h.shares == 0 && h.pending == 0 {
        env.storage().persistent().remove(&k);
    } else {
        env.storage().persistent().set(&k, h);
        bump(env, &k);
    }
}

fn fx(env: &Env) -> FxClient<'_> {
    FxClient::new(env, &get(env, &DataKey::FxPool))
}

fn usdc_token(env: &Env) -> TokenClient<'_> {
    TokenClient::new(env, &get(env, &DataKey::Usdc))
}

fn fresh_fix(env: &Env) -> Result<Fix, Error> {
    let f = fx(env).rate().ok_or(Error::StaleRate)?;
    if env.ledger().timestamp() > f.valid_until || f.inr_per_usd <= 0 {
        return Err(Error::StaleRate);
    }
    Ok(f)
}

fn daily_cap(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::DailyCap)
        .unwrap_or(DEFAULT_DAILY_CAP)
}

/// Senior USDC out today (UTC) stays within the daily cap (19 Nov).
fn take_day_out(env: &Env, usdc: i128) -> Result<(), Error> {
    let k = DataKey::DayOut(env.ledger().timestamp() / DAY);
    let t = env.storage().temporary();
    let used: i128 = t.get(&k).unwrap_or(0);
    if used + usdc > daily_cap(env) {
        return Err(Error::AboveDailyCap);
    }
    t.set(&k, &(used + usdc));
    t.extend_ttl(&k, 2 * 17_280, 2 * 17_280);
    Ok(())
}

#[contract]
pub struct SeniorContract;

#[contractimpl]
impl SeniorContract {
    pub fn __constructor(
        env: Env,
        admin: Address,
        usdc: Address,
        fund_core: Address,
        fx_pool: Address,
        roles: Address,
    ) {
        let s = env.storage().instance();
        s.set(&DataKey::Admin, &admin);
        s.set(&DataKey::Usdc, &usdc);
        s.set(&DataKey::FundCore, &fund_core);
        s.set(&DataKey::FxPool, &fx_pool);
        s.set(&DataKey::Roles, &roles);
        upgrade::init(&env, UPGRADE_DELAY);
        bump_instance(&env);
    }

    // ---------- the trust ----------

    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        AdminSet { admin: new_admin }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Stops deposits, exits, fees and covers.
    pub fn set_paused(env: Env, paused: bool) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &paused);
        PausedSet { paused }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The most senior USDC in all (0 until the contract is audited).
    pub fn set_cap(env: Env, usdc: i128) -> Result<(), Error> {
        require_admin(&env)?;
        if usdc < 0 {
            return Err(Error::AmountZero);
        }
        i_set(&env, &DataKey::Cap, usdc);
        CapSet { usdc }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Senior USDC out a day (executive decision 19 Nov), evented.
    pub fn set_daily_cap(env: Env, usdc: i128) -> Result<(), Error> {
        require_admin(&env)?;
        if usdc < 0 {
            return Err(Error::AmountZero);
        }
        i_set(&env, &DataKey::DailyCap, usdc);
        DailyCapSet { usdc }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The trust accepts (or stops) an investor after KYC at the anchor; the residency tag is fixed once.
    pub fn set_investor(
        env: Env,
        investor: Address,
        accepted: bool,
        kyc_ref: BytesN<32>,
        tag: Tag,
    ) -> Result<(), Error> {
        require_admin(&env)?;
        if load_investor(&env, &investor).is_some_and(|i| i.tag != tag) {
            return Err(Error::TagFixed);
        }
        let k = DataKey::Investor(investor.clone());
        env.storage().persistent().set(
            &k,
            &Investor {
                accepted,
                kyc_ref: kyc_ref.clone(),
                tag,
            },
        );
        bump(&env, &k);
        InvestorSet {
            investor,
            accepted,
            kyc_ref,
            tag,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- investors ----------

    /// USDC in; shares out at today's index. Returns the shares.
    pub fn deposit(env: Env, investor: Address, usdc: i128) -> Result<i128, Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        if usdc <= 0 {
            return Err(Error::AmountZero);
        }
        let inv = load_investor(&env, &investor)
            .filter(|i| i.accepted)
            .ok_or(Error::NotAccepted)?;
        let total = i_get(&env, &DataKey::Total)
            .checked_add(usdc)
            .ok_or(Error::Overflow)?;
        if total > i_get(&env, &DataKey::Cap) {
            return Err(Error::AboveCap);
        }
        let shares = to_shares(&env, usdc, index(&env)).ok_or(Error::Overflow)?;
        if shares <= 0 {
            return Err(Error::AmountZero);
        }
        usdc_token(&env).transfer(&investor, env.current_contract_address(), &usdc);
        i_set(&env, &DataKey::Total, total);
        i_set(
            &env,
            &DataKey::TotalShares,
            i_get(&env, &DataKey::TotalShares) + shares,
        );
        let mut h = load_holding(&env, &investor);
        h.shares += shares;
        save_holding(&env, &investor, &h);
        Deposited {
            investor,
            tag: inv.tag,
            usdc,
            shares,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(shares)
    }

    /// Ask `shares` out: they keep their share of fees and losses for 14 days, then `redeem` pays them.
    pub fn request_redeem(env: Env, investor: Address, shares: i128) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        let mut h = load_holding(&env, &investor);
        if shares <= 0 {
            return Err(Error::AmountZero);
        }
        if shares > h.shares {
            return Err(Error::AboveShares);
        }
        h.shares -= shares;
        h.pending += shares;
        h.request_ts = env.ledger().timestamp();
        save_holding(&env, &investor, &h);
        RedeemRequested { investor, shares }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// After the 14 days: the pending shares at the index of now, in USDC, within today's cap. Returns the USDC.
    pub fn redeem(env: Env, investor: Address) -> Result<i128, Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        let mut h = load_holding(&env, &investor);
        if h.pending <= 0 {
            return Err(Error::AmountZero);
        }
        if env.ledger().timestamp() < h.request_ts + UNBOND {
            return Err(Error::NotReady);
        }
        let out = core::cmp::min(
            to_underlying(&env, h.pending, index(&env)).ok_or(Error::Overflow)?,
            i_get(&env, &DataKey::Total),
        );
        take_day_out(&env, out)?;
        let shares = h.pending;
        i_set(
            &env,
            &DataKey::TotalShares,
            i_get(&env, &DataKey::TotalShares) - shares,
        );
        i_set(&env, &DataKey::Total, i_get(&env, &DataKey::Total) - out);
        h.pending = 0;
        save_holding(&env, &investor, &h);
        if out > 0 {
            usdc_token(&env).transfer(&env.current_contract_address(), &investor, &out);
        }
        Redeemed {
            investor,
            shares,
            usdc: out,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(out)
    }

    // ---------- the guarantee fee ----------

    /// A leader of the union and an accepted member LP turn `inr` of the union's guarantee fee into USDC at the
    /// office: the LP takes the rupees and pays `usdc_in` (at least the rate less its fee) into this contract;
    /// the index rises for every share. Returns what cash at hand covered.
    pub fn pay_fee(
        env: Env,
        union: Address,
        leader: Address,
        lp: Address,
        inr: i128,
        usdc_in: i128,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        leader.require_auth();
        if !RolesClient::new(&env, &get(&env, &DataKey::Roles)).is_leader(&union, &leader) {
            return Err(Error::NotLeader);
        }
        lp.require_auth();
        let fx = fx(&env);
        if !fx.lp_member(&union, &lp).is_some_and(|m| m.accepted) || !fx.id_checked(&lp) {
            return Err(Error::NotLp);
        }
        if inr <= 0 || usdc_in <= 0 {
            return Err(Error::AmountZero);
        }
        let shares = i_get(&env, &DataKey::TotalShares);
        if shares <= 0 {
            return Err(Error::NoShares);
        }
        let f = fresh_fix(&env)?;
        let fair = inr.checked_mul(RATE_ONE).ok_or(Error::Overflow)? / f.inr_per_usd;
        if usdc_in < fair * i128::from(10_000 - fx.lp_fee().min(10_000)) / 10_000 {
            return Err(Error::BelowFix);
        }
        let settled = FundCoreClient::new(&env, &get(&env, &DataKey::FundCore))
            .take_guarantee_fee(&union, &inr);
        // The payment goes through the LP's own account code, so it is checked on this contract's balance, not
        // trusted (re-review, 3 Nov): a fake account that pays nothing is refused.
        let me = env.current_contract_address();
        let token = usdc_token(&env);
        let before = token.balance(&me);
        LpAccountClient::new(&env, &lp).send_usdc(&me, &usdc_in);
        if token.balance(&me) - before < usdc_in {
            return Err(Error::NotPaid);
        }
        let idx = index_bump(&env, index(&env), shares, usdc_in).ok_or(Error::Overflow)?;
        env.storage().instance().set(&DataKey::Index, &idx);
        i_set(
            &env,
            &DataKey::Total,
            i_get(&env, &DataKey::Total) + usdc_in,
        );
        FeePaid {
            union,
            leader,
            lp,
            inr,
            usdc: usdc_in,
            cash_settled: settled,
            index: idx,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(settled)
    }

    // ---------- loss cover ----------

    /// The trust proposes to cover a loss in `union` (a default beyond the junior pool, fund_core's
    /// `LoanDefaulted.senior_applied`): `usdc` to `to` after 48 hours. One at a time.
    pub fn propose_cover(env: Env, union: Address, to: Address, usdc: i128) -> Result<u64, Error> {
        require_admin(&env)?;
        if usdc <= 0 || usdc > i_get(&env, &DataKey::Total) {
            return Err(Error::NoCover);
        }
        let eta = env.ledger().timestamp() + COVER_DELAY;
        env.storage().instance().set(
            &DataKey::PendingCover,
            &Cover {
                union: union.clone(),
                to: to.clone(),
                usdc,
                eta,
            },
        );
        CoverProposed {
            union,
            to,
            usdc,
            eta,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(eta)
    }

    /// After the 48 hours: the cover is paid and the index falls by it.
    pub fn execute_cover(env: Env) -> Result<i128, Error> {
        require_not_paused(&env)?;
        require_admin(&env)?;
        let c: Cover = env
            .storage()
            .instance()
            .get(&DataKey::PendingCover)
            .ok_or(Error::NoCover)?;
        if env.ledger().timestamp() < c.eta {
            return Err(Error::NotReady);
        }
        let total = i_get(&env, &DataKey::Total);
        if c.usdc > total {
            return Err(Error::NoCover);
        }
        let shares = i_get(&env, &DataKey::TotalShares);
        let mut idx = index(&env);
        if shares > 0 {
            let under = to_underlying(&env, shares, idx).ok_or(Error::Overflow)?;
            idx = haircut_index(&env, idx, under, c.usdc)
                .ok_or(Error::Overflow)?
                .1;
            env.storage().instance().set(&DataKey::Index, &idx);
        }
        i_set(&env, &DataKey::Total, total - c.usdc);
        env.storage().instance().remove(&DataKey::PendingCover);
        usdc_token(&env).transfer(&env.current_contract_address(), &c.to, &c.usdc);
        CoverPaid {
            union: c.union,
            to: c.to,
            usdc: c.usdc,
            index: idx,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(c.usdc)
    }

    pub fn cancel_cover(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        let c: Cover = env
            .storage()
            .instance()
            .get(&DataKey::PendingCover)
            .ok_or(Error::NoCover)?;
        env.storage().instance().remove(&DataKey::PendingCover);
        CoverCancelled {
            union: c.union,
            usdc: c.usdc,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- views ----------

    /// An investor's holding and the USDC its shares (free and pending) are worth now.
    pub fn holding(env: Env, investor: Address) -> (Holding, i128) {
        let h = load_holding(&env, &investor);
        let value = to_underlying(&env, h.shares + h.pending, index(&env)).unwrap_or(0);
        (h, value)
    }

    pub fn investor(env: Env, investor: Address) -> Option<Investor> {
        load_investor(&env, &investor)
    }

    /// The pool's index (ray): USDC per share × 10^27.
    pub fn index(env: Env) -> i128 {
        index(&env)
    }

    pub fn total(env: Env) -> i128 {
        i_get(&env, &DataKey::Total)
    }

    pub fn total_shares(env: Env) -> i128 {
        i_get(&env, &DataKey::TotalShares)
    }

    pub fn pending_cover(env: Env) -> Option<Cover> {
        env.storage().instance().get(&DataKey::PendingCover)
    }

    pub fn cap(env: Env) -> i128 {
        i_get(&env, &DataKey::Cap)
    }

    pub fn daily_cap(env: Env) -> i128 {
        daily_cap(&env)
    }

    /// Senior USDC paid out today (UTC).
    pub fn paid_today(env: Env) -> i128 {
        env.storage()
            .temporary()
            .get(&DataKey::DayOut(env.ledger().timestamp() / DAY))
            .unwrap_or(0)
    }

    pub fn admin(env: Env) -> Address {
        get(&env, &DataKey::Admin)
    }

    pub fn paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    // ---------- upgrade (48 h, the trust) ----------

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
}

#[cfg(test)]
mod test;
