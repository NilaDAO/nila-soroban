#![no_std]

mod events;
mod types;

use events::*;
use nila_common::{
    bump_instance, FxPoolClient, InvestorLite, Loan, MarketLite, MembershipClient, ReserveCfg,
    RolesClient, Tranche, ViewerClient, TTL_BUMP, TTL_THRESHOLD,
};
use nila_math::RateParams;
use nila_math::{
    accrued_interest, bucket_ratio_wad, haircut_index, index_bump, to_shares, to_underlying, BPS,
    DEFAULT_PER_LOAN_CAP, DEFAULT_RAINY_FEE_BP, DEFAULT_TREASURY_FEE_BP, DEFAULT_UNBONDING, DUST,
    RAY,
};
use soroban_sdk::{
    contract, contractimpl, contractmeta, contracttype, token::TokenClient, Address, BytesN, Env,
    Vec,
};
use types::{Config, Investor, Market, PendingUpgrade};

pub use types::{DrawParams, Error};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(
    key = "desc",
    val = "Nila fund_core — nIN deposits, tranches, loans, waterfall"
);

const MAX_MATURITY_DATES: u32 = 32;
const ZERO_TYPE: [u8; 32] = [0u8; 32];
const ZERO_DIGEST: [u8; 32] = [0u8; 32];

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    Paused,
    PendingUpgrade,
    Reserve(Address),
    RateParams(Address),
    SeniorMarket(Address),
    JuniorMarket(Address, BytesN<32>),
    SeniorInv(Address, Address),
    JuniorInv(Address, BytesN<32>, Address),
    Loan(Address, BytesN<32>),
    UnionClaimable(Address),
    UnionBorrows(Address),
    UnionTreasury(Address),
    UnionRainy(Address),
    BucketThreshold(Address, BytesN<32>),
    BucketMax(Address, BytesN<32>),
    ScheduledPrincipal(Address, u64),
    MaturedCredit(Address),
    MaturedConsumed(Address),
    LoanScheduled(Address, BytesN<32>),
}

#[contract]
pub struct FundCore;

fn cfg(env: &Env) -> Config {
    env.storage().instance().get(&DataKey::Config).unwrap()
}

fn save_cfg(env: &Env, c: &Config) {
    env.storage().instance().set(&DataKey::Config, c);
    bump_instance(env);
}

fn paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
}

fn require_not_paused(env: &Env) -> Result<(), Error> {
    if paused(env) {
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

fn require_positive(amount: i128) -> Result<(), Error> {
    if amount < 0 {
        Err(Error::NegativeAmount)
    } else if amount == 0 {
        Err(Error::AmountZero)
    } else {
        Ok(())
    }
}

fn require_oracle_or_leader(env: &Env, union: &Address, who: &Address) -> Result<Config, Error> {
    who.require_auth();
    let c = cfg(env);
    if *who == c.admin {
        return Ok(c);
    }
    let roles = RolesClient::new(env, &c.roles);
    if roles.is_oracle(who) || roles.is_leader(union, who) {
        Ok(c)
    } else {
        Err(Error::NotOracleOrLeader)
    }
}

fn require_leader(env: &Env, union: &Address, who: &Address) -> Result<Config, Error> {
    who.require_auth();
    let c = cfg(env);
    if *who == c.admin {
        return Ok(c);
    }
    let roles = RolesClient::new(env, &c.roles);
    if roles.is_leader(union, who) {
        Ok(c)
    } else {
        Err(Error::NotOracleOrLeader)
    }
}

fn bump_key(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, TTL_THRESHOLD, TTL_BUMP);
}

fn load_market(env: &Env, junior: bool, union: &Address, loan_type: &BytesN<32>) -> Market {
    let key = if junior {
        DataKey::JuniorMarket(union.clone(), loan_type.clone())
    } else {
        DataKey::SeniorMarket(union.clone())
    };
    env.storage().persistent().get(&key).unwrap_or_default()
}

fn save_market(env: &Env, junior: bool, union: &Address, loan_type: &BytesN<32>, m: &Market) {
    let key = if junior {
        DataKey::JuniorMarket(union.clone(), loan_type.clone())
    } else {
        DataKey::SeniorMarket(union.clone())
    };
    env.storage().persistent().set(&key, m);
    bump_key(env, &key);
}

fn load_inv(
    env: &Env,
    junior: bool,
    union: &Address,
    loan_type: &BytesN<32>,
    who: &Address,
) -> Investor {
    let key = if junior {
        DataKey::JuniorInv(union.clone(), loan_type.clone(), who.clone())
    } else {
        DataKey::SeniorInv(union.clone(), who.clone())
    };
    env.storage().persistent().get(&key).unwrap_or_default()
}

fn save_inv(
    env: &Env,
    junior: bool,
    union: &Address,
    loan_type: &BytesN<32>,
    who: &Address,
    inv: &Investor,
) {
    let key = if junior {
        DataKey::JuniorInv(union.clone(), loan_type.clone(), who.clone())
    } else {
        DataKey::SeniorInv(union.clone(), who.clone())
    };
    env.storage().persistent().set(&key, inv);
    bump_key(env, &key);
}

fn load_loan(env: &Env, union: &Address, loan_id: &BytesN<32>) -> Option<Loan> {
    env.storage()
        .persistent()
        .get(&DataKey::Loan(union.clone(), loan_id.clone()))
}

fn save_loan(env: &Env, union: &Address, loan_id: &BytesN<32>, ln: &Loan) {
    let key = DataKey::Loan(union.clone(), loan_id.clone());
    env.storage().persistent().set(&key, ln);
    bump_key(env, &key);
}

fn i_get(env: &Env, key: &DataKey) -> i128 {
    env.storage().persistent().get(key).unwrap_or(0)
}

fn i_set(env: &Env, key: &DataKey, v: i128) {
    env.storage().persistent().set(key, &v);
    bump_key(env, key);
}

fn nin(env: &Env) -> TokenClient<'_> {
    TokenClient::new(env, &cfg(env).nin)
}

fn zero_type(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &ZERO_TYPE)
}

fn require_member(env: &Env, union: &Address, who: &Address) -> Result<(), Error> {
    let c = cfg(env);
    let m = MembershipClient::new(env, &c.membership);
    if m.is_member(union, who) {
        Ok(())
    } else {
        Err(Error::NftRequired)
    }
}

fn require_not_member(env: &Env, union: &Address, who: &Address) -> Result<(), Error> {
    let c = cfg(env);
    let m = MembershipClient::new(env, &c.membership);
    if m.is_member(union, who) {
        Err(Error::NftForbidden)
    } else {
        Ok(())
    }
}

fn ensure_junior(env: &Env, union: &Address, loan_type: &BytesN<32>) -> Market {
    let mut m = load_market(env, true, union, loan_type);
    if !m.exists {
        m.exists = true;
        m.index = RAY;
        save_market(env, true, union, loan_type, &m);
        JuniorMarketCreated {
            union: union.clone(),
            loan_type: loan_type.clone(),
        }
        .publish(env);
    }
    m
}

fn ensure_senior(env: &Env, union: &Address) -> Market {
    let zt = zero_type(env);
    let mut m = load_market(env, false, union, &zt);
    if m.index == 0 {
        m.index = RAY;
        m.exists = true;
        save_market(env, false, union, &zt, &m);
    }
    m
}

fn settle_yield(env: &Env, inv: &mut Investor, current_index: i128) -> Result<(), Error> {
    let idx = if current_index == 0 {
        RAY
    } else {
        current_index
    };
    if inv.entry_index == 0 {
        inv.entry_index = idx;
        return Ok(());
    }
    if idx > inv.entry_index && inv.shares > 0 {
        let delta = idx - inv.entry_index;
        let add = nila_math::mul_div(env, inv.shares, delta, RAY).ok_or(Error::Overflow)?;
        inv.unclaimed = inv.unclaimed.checked_add(add).ok_or(Error::Overflow)?;
    }
    inv.entry_index = idx;
    Ok(())
}

fn required_reserve(env: &Env, union: &Address) -> Result<(ReserveCfg, i128), Error> {
    let cfg_r: ReserveCfg = env
        .storage()
        .persistent()
        .get(&DataKey::Reserve(union.clone()))
        .ok_or(Error::NotExists)?;
    if !cfg_r.exists {
        return Err(Error::NotExists);
    }
    let claimable = i_get(env, &DataKey::UnionClaimable(union.clone()));
    let bump = nila_math::mul_div(env, i128::from(cfg_r.safety_bp), claimable, BPS)
        .ok_or(Error::Overflow)?;
    let req = claimable
        .checked_add(bump)
        .and_then(|x| x.checked_add(cfg_r.safety_floor))
        .ok_or(Error::Overflow)?;
    Ok((cfg_r, req))
}

fn check_reserve_before_funding(
    env: &Env,
    union: &Address,
    loan_type: &BytesN<32>,
    amount: i128,
) -> Result<(), Error> {
    let (_, req) = required_reserve(env, union)?;
    let jm = load_market(env, true, union, loan_type);
    let sm = load_market(env, false, union, &zero_type(env));
    let idle = jm.cash.checked_add(sm.cash).ok_or(Error::Overflow)?;
    if idle < amount.checked_add(req).ok_or(Error::Overflow)? {
        Err(Error::ReserveStop)
    } else {
        Ok(())
    }
}

fn verify_bucket_ratio(env: &Env, union: &Address, loan_type: &BytesN<32>) -> Result<(), Error> {
    let threshold: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::BucketThreshold(union.clone(), loan_type.clone()))
        .unwrap_or(0);
    if threshold == 0 {
        return Ok(());
    }
    let jm = load_market(env, true, union, loan_type);
    let sm = load_market(env, false, union, &zero_type(env));
    let total_j = to_underlying(env, jm.total_shares, jm.index).unwrap_or(0);
    let total_s = to_underlying(env, sm.total_shares, sm.index).unwrap_or(0);
    if let Some(ratio) = bucket_ratio_wad(env, total_j, total_s) {
        if ratio < threshold {
            return Err(Error::BadRatio);
        }
    }
    Ok(())
}

fn distribute_interest(
    env: &Env,
    union: &Address,
    loan_type: &BytesN<32>,
    interest: i128,
    funded_j: i128,
    funded_s: i128,
    skip_fees: bool,
) -> Result<(), Error> {
    let c = cfg(env);
    let fee = if skip_fees {
        0
    } else {
        nila_math::mul_div(env, interest, i128::from(c.treasury_fee_bp), BPS)
            .ok_or(Error::Overflow)?
    };
    if fee > 0 {
        let key = DataKey::UnionTreasury(union.clone());
        i_set(env, &key, i_get(env, &key) + fee);
    }
    let rainy = if skip_fees || c.rainy_fee_bp == 0 {
        0
    } else {
        nila_math::mul_div(env, interest, i128::from(c.rainy_fee_bp), BPS).ok_or(Error::Overflow)?
    };
    if rainy > 0 {
        let key = DataKey::UnionRainy(union.clone());
        i_set(env, &key, i_get(env, &key) + rainy);
    }
    let net = interest - fee - rainy;
    let mut jm = load_market(env, true, union, loan_type);
    let mut sm = load_market(env, false, union, &zero_type(env));
    let total_funded = funded_j + funded_s;
    let (to_j, to_s) = if total_funded == 0 {
        (net / 2, net - net / 2)
    } else {
        let to_j = nila_math::mul_div(env, net, funded_j, total_funded).ok_or(Error::Overflow)?;
        (to_j, net - to_j)
    };
    jm.cash += to_j;
    sm.cash += to_s;
    jm.index = index_bump(env, jm.index, jm.total_shares, to_j).ok_or(Error::Overflow)?;
    sm.index = index_bump(env, sm.index, sm.total_shares, to_s).ok_or(Error::Overflow)?;
    save_market(env, true, union, loan_type, &jm);
    save_market(env, false, union, &zero_type(env), &sm);
    Ok(())
}

fn bump_union_claimable(env: &Env, union: &Address, delta: i128) -> Result<(), Error> {
    let key = DataKey::UnionClaimable(union.clone());
    let cur = i_get(env, &key);
    let next = cur.checked_add(delta).ok_or(Error::Overflow)?;
    if next < 0 {
        return Err(Error::Overflow);
    }
    i_set(env, &key, next);
    Ok(())
}

fn redeem_claimable(
    env: &Env,
    is_junior: bool,
    union: &Address,
    loan_type: &BytesN<32>,
    investor: &Address,
    max_shares: i128,
) -> Result<(), Error> {
    let mut inv = load_inv(env, is_junior, union, loan_type, investor);
    if inv.claimable == 0 {
        return Err(Error::NothingToClaim);
    }
    let burn = if max_shares == 0 || max_shares > inv.claimable {
        inv.claimable
    } else {
        max_shares
    };
    if burn <= 0 {
        return Err(Error::BadShares);
    }
    let mut m = load_market(env, is_junior, union, loan_type);
    let amount = to_underlying(env, burn, m.index).ok_or(Error::Overflow)?;
    let (rcfg, req) = required_reserve(env, union)?;
    let sm = load_market(env, false, union, &zero_type(env));
    let idle = if is_junior {
        m.cash.checked_add(sm.cash).ok_or(Error::Overflow)?
    } else {
        m.cash
    };
    if rcfg.hard_stop && idle < amount.checked_add(req).ok_or(Error::Overflow)? {
        return Err(Error::ReserveStop);
    }
    if m.cash < amount {
        return Err(Error::InsufficientCash);
    }
    inv.claimable -= burn;
    inv.shares -= burn;
    m.total_shares -= burn;
    m.cash -= amount;
    let mut release = amount;
    if release > m.claimable_principal {
        release = m.claimable_principal;
    }
    m.claimable_principal -= release;
    bump_union_claimable(env, union, -release)?;
    save_inv(env, is_junior, union, loan_type, investor, &inv);
    save_market(env, is_junior, union, loan_type, &m);
    nin(env).transfer(&env.current_contract_address(), investor, &amount);
    let tranche = if is_junior {
        Tranche::Junior
    } else {
        Tranche::Senior
    };
    Claimed {
        investor: investor.clone(),
        union: union.clone(),
        tranche,
        loan_type: loan_type.clone(),
        amount,
    }
    .publish(env);
    Ok(())
}

fn resolve_escrow(env: &Env, escrow_id: u64, amount: i128) {
    if escrow_id == 0 {
        return;
    }
    let c = cfg(env);
    if let Some(fx) = c.fx_pool {
        FxPoolClient::new(env, &fx).resolve_escrow_cash(&escrow_id, &amount);
    }
}

#[contractimpl]
impl FundCore {
    pub fn __constructor(
        env: Env,
        admin: Address,
        roles: Address,
        membership: Address,
        viewer: Address,
        nin_token: Address,
    ) {
        let c = Config {
            admin,
            roles,
            membership,
            viewer,
            nin: nin_token,
            fx_pool: None,
            treasury_fee_bp: DEFAULT_TREASURY_FEE_BP,
            rainy_fee_bp: DEFAULT_RAINY_FEE_BP,
            per_loan_cap: DEFAULT_PER_LOAN_CAP,
            upgrade_delay: 0,
        };
        save_cfg(&env, &c);
        env.storage().instance().set(&DataKey::Paused, &false);
    }

    pub fn pause(env: Env, who: Address) -> Result<(), Error> {
        who.require_auth();
        if who != cfg(&env).admin {
            return Err(Error::NotAdmin);
        }
        env.storage().instance().set(&DataKey::Paused, &true);
        Paused { paused: true }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn unpause(env: Env, who: Address) -> Result<(), Error> {
        who.require_auth();
        if who != cfg(&env).admin {
            return Err(Error::NotAdmin);
        }
        env.storage().instance().set(&DataKey::Paused, &false);
        Paused { paused: false }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_viewer(env: Env, v: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.viewer = v;
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn set_roles(env: Env, v: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.roles = v;
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn set_membership(env: Env, v: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.membership = v;
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn set_fx_pool(env: Env, v: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.fx_pool = Some(v);
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn set_nin(env: Env, v: Address) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.nin = v;
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn set_fee_bps(env: Env, treasury_bp: u32, rainy_bp: u32) -> Result<(), Error> {
        if treasury_bp as i128 + rainy_bp as i128 > BPS {
            return Err(Error::BadArg);
        }
        let mut c = require_admin(&env)?;
        c.treasury_fee_bp = treasury_bp;
        c.rainy_fee_bp = rainy_bp;
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn set_per_loan_cap(env: Env, cap: i128) -> Result<(), Error> {
        if cap < 0 {
            return Err(Error::NegativeAmount);
        }
        let mut c = require_admin(&env)?;
        c.per_loan_cap = cap;
        save_cfg(&env, &c);
        CapUpdated { per_loan_cap: cap }.publish(&env);
        Ok(())
    }

    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        let mut c = require_admin(&env)?;
        c.upgrade_delay = delay_seconds;
        save_cfg(&env, &c);
        Ok(())
    }

    pub fn set_reserve_config(
        env: Env,
        who: Address,
        union: Address,
        safety_bp: u32,
        safety_floor: i128,
        hard_stop: bool,
        escrow_duration: u32,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        if safety_floor < 0 || safety_bp as i128 > BPS {
            return Err(Error::BadArg);
        }
        let rc = ReserveCfg {
            safety_bp,
            safety_floor,
            hard_stop,
            exists: true,
            escrow_duration,
        };
        let key = DataKey::Reserve(union.clone());
        env.storage().persistent().set(&key, &rc);
        bump_key(&env, &key);
        UnionReserveUpdated {
            union,
            safety_bp,
            safety_floor,
            hard_stop,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_bucket_thresholds(
        env: Env,
        union: Address,
        loan_type: BytesN<32>,
        threshold_wad: i128,
        max_loan_amount: i128,
    ) -> Result<(), Error> {
        require_admin(&env)?;
        if threshold_wad < 0 || max_loan_amount < 0 {
            return Err(Error::NegativeAmount);
        }
        i_set(
            &env,
            &DataKey::BucketThreshold(union.clone(), loan_type.clone()),
            threshold_wad,
        );
        i_set(&env, &DataKey::BucketMax(union, loan_type), max_loan_amount);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_rate_params(
        env: Env,
        who: Address,
        union: Address,
        params: RateParams,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        if params.kink_util_bp == 0 || params.kink_util_bp > 10_000 {
            return Err(Error::BadKink);
        }
        let key = DataKey::RateParams(union.clone());
        env.storage().persistent().set(&key, &params);
        bump_key(&env, &key);
        RateParamsUpdated {
            union,
            base_rate_bp: params.base_rate_bp,
            kink_util_bp: params.kink_util_bp,
            slope1_bp: params.slope1_bp,
            slope2_bp: params.slope2_bp,
            max_rate_bp: params.max_rate_bp,
        }
        .publish(&env);
        Ok(())
    }

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
        let bal = i_get(&env, &key);
        if amount > bal {
            return Err(Error::InsufficientCash);
        }
        i_set(&env, &key, bal - amount);
        nin(&env).transfer(&env.current_contract_address(), &leader, &amount);
        bump_instance(&env);
        Ok(())
    }

    pub fn credit_escrow_nin(
        env: Env,
        union: Address,
        loan_type: BytesN<32>,
        amount: i128,
    ) -> Result<(), Error> {
        require_positive(amount)?;
        let c = cfg(&env);
        let fx = c.fx_pool.ok_or(Error::NotFxPool)?;
        fx.require_auth();
        let mut m = ensure_junior(&env, &union, &loan_type);
        m.cash = m.cash.checked_add(amount).ok_or(Error::Overflow)?;
        save_market(&env, true, &union, &loan_type, &m);
        Ok(())
    }

    /// FX-only: debit junior cash for expired escrow (TAD burnTreasuryForExpiry).
    pub fn burn_treasury_for_expiry(
        env: Env,
        union: Address,
        loan_type: BytesN<32>,
        amount: i128,
    ) -> Result<(), Error> {
        require_positive(amount)?;
        let c = cfg(&env);
        let fx = c.fx_pool.ok_or(Error::NotFxPool)?;
        fx.require_auth();
        let mut m = load_market(&env, true, &union, &loan_type);
        m.cash = if m.cash >= amount { m.cash - amount } else { 0 };
        save_market(&env, true, &union, &loan_type, &m);
        Ok(())
    }

    pub fn add_junior_cash(
        env: Env,
        who: Address,
        union: Address,
        loan_type: BytesN<32>,
        amount: i128,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        require_positive(amount)?;
        nin(&env).transfer(&who, env.current_contract_address(), &amount);
        let mut m = ensure_junior(&env, &union, &loan_type);
        m.cash += amount;
        save_market(&env, true, &union, &loan_type, &m);
        Ok(())
    }

    pub fn get_union_escrow_duration(env: Env, union: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Reserve(union))
            .map(|r: ReserveCfg| r.escrow_duration)
            .unwrap_or(0)
    }

    // ---------- Senior ----------
    pub fn deposit_senior(
        env: Env,
        investor: Address,
        union: Address,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        require_positive(amount)?;
        require_not_member(&env, &union, &investor)?;
        let zt = zero_type(&env);
        let mut m = ensure_senior(&env, &union);
        let mut inv = load_inv(&env, false, &union, &zt, &investor);
        settle_yield(&env, &mut inv, m.index)?;
        nin(&env).transfer(&investor, env.current_contract_address(), &amount);
        m.cash += amount;
        let shares = to_shares(&env, amount, m.index).ok_or(Error::Overflow)?;
        m.total_shares += shares;
        inv.shares += shares;
        save_market(&env, false, &union, &zt, &m);
        save_inv(&env, false, &union, &zt, &investor, &inv);
        Deposit {
            investor,
            union,
            tranche: Tranche::Senior,
            loan_type: zt,
            amount,
            shares_out: shares,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn request_unbond_senior(
        env: Env,
        investor: Address,
        union: Address,
        shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        require_positive(shares)?;
        let zt = zero_type(&env);
        let mut inv = load_inv(&env, false, &union, &zt, &investor);
        let free = inv.shares - inv.pending - inv.claimable;
        if shares > free {
            return Err(Error::BadShares);
        }
        inv.pending += shares;
        inv.request_ts = env.ledger().timestamp();
        let mut m = load_market(&env, false, &union, &zt);
        let snap = to_underlying(&env, shares, m.index).ok_or(Error::Overflow)?;
        m.claimable_principal += snap;
        bump_union_claimable(&env, &union, snap)?;
        save_inv(&env, false, &union, &zt, &investor, &inv);
        save_market(&env, false, &union, &zt, &m);
        UnbondRequested {
            investor,
            union,
            tranche: Tranche::Senior,
            loan_type: zt,
            shares,
            principal_snap: snap,
        }
        .publish(&env);
        Ok(())
    }

    pub fn claim_senior(
        env: Env,
        investor: Address,
        union: Address,
        max_shares: i128,
    ) -> Result<(), Error> {
        investor.require_auth();
        if max_shares < 0 {
            return Err(Error::NegativeAmount);
        }
        let zt = zero_type(&env);
        let m = load_market(&env, false, &union, &zt);
        let mut inv = load_inv(&env, false, &union, &zt, &investor);
        settle_yield(&env, &mut inv, m.index)?;
        if inv.claimable == 0
            && inv.pending > 0
            && env.ledger().timestamp() >= inv.request_ts + DEFAULT_UNBONDING
        {
            let shares_auto = inv.pending;
            inv.pending = 0;
            inv.claimable += shares_auto;
            UnbondPromoted {
                investor: investor.clone(),
                union: union.clone(),
                tranche: Tranche::Senior,
                loan_type: zt.clone(),
                shares: shares_auto,
            }
            .publish(&env);
        }
        save_inv(&env, false, &union, &zt, &investor, &inv);
        redeem_claimable(&env, false, &union, &zt, &investor, max_shares)?;
        bump_instance(&env);
        Ok(())
    }

    // ---------- Junior ----------
    pub fn deposit_junior(
        env: Env,
        investor: Address,
        union: Address,
        loan_type: BytesN<32>,
        amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        require_positive(amount)?;
        require_member(&env, &union, &investor)?;
        let mut m = ensure_junior(&env, &union, &loan_type);
        let mut inv = load_inv(&env, true, &union, &loan_type, &investor);
        settle_yield(&env, &mut inv, m.index)?;
        nin(&env).transfer(&investor, env.current_contract_address(), &amount);
        m.cash += amount;
        let shares = to_shares(&env, amount, m.index).ok_or(Error::Overflow)?;
        m.total_shares += shares;
        inv.shares += shares;
        save_market(&env, true, &union, &loan_type, &m);
        save_inv(&env, true, &union, &loan_type, &investor, &inv);
        Deposit {
            investor,
            union,
            tranche: Tranche::Junior,
            loan_type,
            amount,
            shares_out: shares,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn request_unbond_junior(
        env: Env,
        investor: Address,
        union: Address,
        loan_type: BytesN<32>,
        shares: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        require_positive(shares)?;
        ensure_junior(&env, &union, &loan_type);
        let mut inv = load_inv(&env, true, &union, &loan_type, &investor);
        let free = inv.shares - inv.pending - inv.claimable;
        if shares > free {
            return Err(Error::BadShares);
        }
        inv.pending += shares;
        inv.request_ts = env.ledger().timestamp();
        let mut m = load_market(&env, true, &union, &loan_type);
        let snap = to_underlying(&env, shares, m.index).ok_or(Error::Overflow)?;
        inv.pending_principal_snap += snap;
        m.claimable_principal += snap;
        bump_union_claimable(&env, &union, snap)?;
        save_inv(&env, true, &union, &loan_type, &investor, &inv);
        save_market(&env, true, &union, &loan_type, &m);
        UnbondRequested {
            investor,
            union,
            tranche: Tranche::Junior,
            loan_type,
            shares,
            principal_snap: snap,
        }
        .publish(&env);
        Ok(())
    }

    pub fn claim_junior(
        env: Env,
        investor: Address,
        union: Address,
        loan_type: BytesN<32>,
        max_shares: i128,
    ) -> Result<(), Error> {
        investor.require_auth();
        if max_shares < 0 {
            return Err(Error::NegativeAmount);
        }
        ensure_junior(&env, &union, &loan_type);
        let mut inv = load_inv(&env, true, &union, &loan_type, &investor);
        if inv.claimable == 0 && inv.pending > 0 {
            let past_min = env.ledger().timestamp() >= inv.request_ts + DEFAULT_UNBONDING;
            let available_budget = i_get(&env, &DataKey::MaturedCredit(union.clone()))
                - i_get(&env, &DataKey::MaturedConsumed(union.clone()));
            let covered_by_maturities =
                inv.pending_principal_snap > 0 && available_budget >= inv.pending_principal_snap;

            let (rcfg, _) = required_reserve(&env, &union)?;
            let claimable = i_get(&env, &DataKey::UnionClaimable(union.clone()));
            let claimable_after = if claimable > inv.pending_principal_snap {
                claimable - inv.pending_principal_snap
            } else {
                0
            };
            let bump_after =
                nila_math::mul_div(&env, i128::from(rcfg.safety_bp), claimable_after, BPS)
                    .ok_or(Error::Overflow)?;
            let req_after = claimable_after + bump_after + rcfg.safety_floor;
            let jm = load_market(&env, true, &union, &loan_type);
            let sm = load_market(&env, false, &union, &zero_type(&env));
            let idle = jm.cash + sm.cash;
            let covered_by_idle = idle >= inv.pending_principal_snap + req_after;

            if !(past_min && (covered_by_maturities || covered_by_idle)) {
                return Err(Error::NotEligibleMaturity);
            }

            settle_yield(&env, &mut inv, jm.index)?;
            let to_promote = inv.pending;
            inv.pending = 0;
            inv.claimable += to_promote;
            if covered_by_maturities {
                let ck = DataKey::MaturedConsumed(union.clone());
                i_set(&env, &ck, i_get(&env, &ck) + inv.pending_principal_snap);
            }
            inv.pending_principal_snap = 0;
            UnbondPromoted {
                investor: investor.clone(),
                union: union.clone(),
                tranche: Tranche::Junior,
                loan_type: loan_type.clone(),
                shares: to_promote,
            }
            .publish(&env);
        }
        save_inv(&env, true, &union, &loan_type, &investor, &inv);
        if load_inv(&env, true, &union, &loan_type, &investor).claimable == 0 {
            return Err(Error::NothingToClaim);
        }
        redeem_claimable(&env, true, &union, &loan_type, &investor, max_shares)?;
        bump_instance(&env);
        Ok(())
    }

    pub fn claim_yield(
        env: Env,
        investor: Address,
        tranche: Tranche,
        union: Address,
        loan_type: BytesN<32>,
        max_amount: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        if max_amount < 0 {
            return Err(Error::NegativeAmount);
        }
        let is_junior = matches!(tranche, Tranche::Junior);
        if is_junior {
            ensure_junior(&env, &union, &loan_type);
        } else {
            ensure_senior(&env, &union);
        }
        let zt = if is_junior {
            loan_type.clone()
        } else {
            zero_type(&env)
        };
        let mut m = load_market(&env, is_junior, &union, &zt);
        let mut inv = load_inv(&env, is_junior, &union, &zt, &investor);
        settle_yield(&env, &mut inv, m.index)?;
        let available = inv.unclaimed;
        if available == 0 {
            return Err(Error::NoYield);
        }
        let (rcfg, req) = required_reserve(&env, &union)?;
        let sm = load_market(&env, false, &union, &zero_type(&env));
        let idle = if is_junior { m.cash + sm.cash } else { m.cash };
        let mut payout = if max_amount == 0 || max_amount > available {
            available
        } else {
            max_amount
        };
        let headroom = if idle > req { idle - req } else { 0 };
        if rcfg.hard_stop {
            if idle < req + payout {
                return Err(Error::ReserveStop);
            }
        } else if payout > headroom {
            payout = headroom;
        }
        if payout == 0 || payout > m.cash {
            return Err(Error::InsufficientCash);
        }
        inv.unclaimed -= payout;
        m.cash -= payout;
        save_inv(&env, is_junior, &union, &zt, &investor, &inv);
        save_market(&env, is_junior, &union, &zt, &m);
        nin(&env).transfer(&env.current_contract_address(), &investor, &payout);
        YieldClaimed {
            investor,
            union,
            tranche,
            loan_type: zt,
            amount: payout,
        }
        .publish(&env);
        Ok(())
    }

    // ---------- Loans ----------
    /// Oracle voucher is Stellar `require_auth` on `oracle` (host nonces), not EIP-712.
    pub fn draw_loan(
        env: Env,
        borrower: Address,
        oracle: Address,
        union: Address,
        p: DrawParams,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        borrower.require_auth();
        oracle.require_auth();
        require_positive(p.amount)?;
        require_member(&env, &union, &borrower)?;

        let c = cfg(&env);
        let roles = RolesClient::new(&env, &c.roles);
        if !roles.is_oracle(&oracle) {
            return Err(Error::SignatureInvalid);
        }
        if p.amount > p.max_amount {
            return Err(Error::VoucherAmountTooHigh);
        }
        if c.per_loan_cap > 0 && p.amount > c.per_loan_cap {
            return Err(Error::CapExceeded);
        }
        let bucket_cap = i_get(
            &env,
            &DataKey::BucketMax(union.clone(), p.loan_type.clone()),
        );
        if bucket_cap > 0 && p.amount > bucket_cap {
            return Err(Error::MaxLoanAmount);
        }
        if load_loan(&env, &union, &p.loan_id).is_some() {
            return Err(Error::LoanNotExist);
        }

        check_reserve_before_funding(&env, &union, &p.loan_type, p.amount)?;
        let mut jm = ensure_junior(&env, &union, &p.loan_type);
        let mut sm = ensure_senior(&env, &union);
        let take_j = if p.amount <= jm.cash {
            p.amount
        } else {
            jm.cash
        };
        let take_s = p.amount - take_j;
        if take_s > sm.cash {
            return Err(Error::InsufficientCash);
        }
        verify_bucket_ratio(&env, &union, &p.loan_type)?;
        jm.cash -= take_j;
        sm.cash -= take_s;

        let now = env.ledger().timestamp();
        let mut stored = Loan {
            borrower: borrower.clone(),
            loan_type: p.loan_type.clone(),
            principal: p.amount,
            rate_bp: p.rate_bp,
            create_ts: now,
            drawdown_ts: 0,
            maturity_ts: p.maturity_ts,
            principal_paid: 0,
            interest_paid: 0,
            funded_from_junior: take_j,
            funded_from_senior: take_s,
            defaulted: false,
            liquidated: false,
            milestone: 0,
            milestone_digest: BytesN::from_array(&env, &ZERO_DIGEST),
            digest_ts: 0,
            last_accrual_ts: 0,
            interest_accrued: 0,
            lower_rate: p.rate_bp >= p.min_rate_bp,
            sos_date: p.sos_date,
        };

        let bk = DataKey::UnionBorrows(union.clone());
        i_set(&env, &bk, i_get(&env, &bk) + p.amount);
        jm.total_borrows += p.amount;
        save_market(&env, true, &union, &p.loan_type, &jm);
        save_market(&env, false, &union, &zero_type(&env), &sm);

        if p.rate_bp >= p.min_rate_bp && p.fast_draw {
            stored.drawdown_ts = now;
            stored.last_accrual_ts = now;
            nin(&env).transfer(&env.current_contract_address(), &borrower, &p.amount);
            resolve_escrow(&env, p.escrow_id, p.amount);
        }
        save_loan(&env, &union, &p.loan_id, &stored);
        ViewerClient::new(&env, &c.viewer).on_loan_created(&union, &p.loan_id, &borrower);

        LoanClaimed {
            union,
            loan_id: p.loan_id,
            borrower,
            loan_type: p.loan_type,
            amount: p.amount,
            rate_bp: p.rate_bp,
            sos_date: p.sos_date,
            drawdown_ts: stored.drawdown_ts,
            fast_draw: p.fast_draw,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn accept_loan(
        env: Env,
        leader: Address,
        union: Address,
        loan_id: BytesN<32>,
        escrow_id: u64,
    ) -> Result<(), Error> {
        require_leader(&env, &union, &leader)?;
        let mut ln = load_loan(&env, &union, &loan_id).ok_or(Error::LoanNotExist)?;
        if ln.create_ts == 0 {
            return Err(Error::LoanNotExist);
        }
        if ln.drawdown_ts != 0 {
            return Err(Error::LoanHasBeenAccepted);
        }
        let now = env.ledger().timestamp();
        ln.drawdown_ts = now;
        ln.last_accrual_ts = now;
        let amount = ln.principal;
        let borrower = ln.borrower.clone();
        save_loan(&env, &union, &loan_id, &ln);
        nin(&env).transfer(&env.current_contract_address(), &borrower, &amount);
        resolve_escrow(&env, escrow_id, amount);
        LoanAccepted {
            union,
            loan_id,
            amount,
            borrower,
        }
        .publish(&env);
        Ok(())
    }

    pub fn repay_loan(
        env: Env,
        payer: Address,
        union: Address,
        loan_id: BytesN<32>,
        amount: i128,
    ) -> Result<bool, Error> {
        require_not_paused(&env)?;
        payer.require_auth();
        require_positive(amount)?;
        let mut ln = load_loan(&env, &union, &loan_id).ok_or(Error::LoanNotExist)?;
        if ln.defaulted || ln.liquidated {
            return Err(Error::BadState);
        }
        nin(&env).transfer(&payer, env.current_contract_address(), &amount);

        let principal_outstanding = if ln.principal > ln.principal_paid {
            ln.principal - ln.principal_paid
        } else {
            0
        };
        let last_ts = if ln.last_accrual_ts == 0 {
            ln.create_ts
        } else {
            ln.last_accrual_ts
        };
        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(last_ts);
        let interest_for_period =
            accrued_interest(&env, principal_outstanding, ln.rate_bp, elapsed)
                .ok_or(Error::Overflow)?;
        let total_accrued = ln.interest_accrued + interest_for_period;
        let interest_owed = if total_accrued > ln.interest_paid {
            total_accrued - ln.interest_paid
        } else {
            0
        };
        let pay_interest = if amount < interest_owed {
            amount
        } else {
            interest_owed
        };
        let leftover = amount - pay_interest;
        if interest_for_period > 0 || ln.interest_accrued > 0 {
            ln.interest_accrued = total_accrued;
        }
        if pay_interest > 0 {
            ln.interest_paid += pay_interest;
            distribute_interest(
                &env,
                &union,
                &ln.loan_type,
                pay_interest,
                ln.funded_from_junior,
                ln.funded_from_senior,
                ln.lower_rate,
            )?;
        }
        ln.last_accrual_ts = now;

        let mut pay_principal = if leftover > principal_outstanding {
            principal_outstanding
        } else {
            leftover
        };
        let principal_for_util = pay_principal;
        if pay_principal > 0 {
            let rem_j = ln.funded_from_junior;
            if rem_j > 0 {
                let to_j = if pay_principal <= rem_j {
                    pay_principal
                } else {
                    rem_j
                };
                ln.funded_from_junior = rem_j - to_j;
                let mut jm = load_market(&env, true, &union, &ln.loan_type);
                jm.cash += to_j;
                save_market(&env, true, &union, &ln.loan_type, &jm);
                pay_principal -= to_j;
            }
            if pay_principal > 0 {
                let rem_s = ln.funded_from_senior;
                let to_s = if pay_principal <= rem_s {
                    pay_principal
                } else {
                    rem_s
                };
                ln.funded_from_senior = rem_s - to_s;
                let mut sm = load_market(&env, false, &union, &zero_type(&env));
                sm.cash += to_s;
                save_market(&env, false, &union, &zero_type(&env), &sm);
                pay_principal -= to_s;
            }
            if pay_principal > 0 {
                let mut sm = load_market(&env, false, &union, &zero_type(&env));
                sm.cash += pay_principal;
                save_market(&env, false, &union, &zero_type(&env), &sm);
            }
            let ubk = DataKey::UnionBorrows(union.clone());
            let ub = i_get(&env, &ubk);
            i_set(
                &env,
                &ubk,
                if principal_for_util >= ub {
                    0
                } else {
                    ub - principal_for_util
                },
            );
            let mut jm = load_market(&env, true, &union, &ln.loan_type);
            jm.total_borrows = if principal_for_util >= jm.total_borrows {
                0
            } else {
                jm.total_borrows - principal_for_util
            };
            save_market(&env, true, &union, &ln.loan_type, &jm);
            ln.principal_paid += principal_for_util;
            if ln.maturity_ts != 0 && now < ln.maturity_ts {
                let sk = DataKey::LoanScheduled(union.clone(), loan_id.clone());
                let sched = i_get(&env, &sk);
                if sched > 0 {
                    let cut = if principal_for_util <= sched {
                        principal_for_util
                    } else {
                        sched
                    };
                    i_set(&env, &sk, sched - cut);
                    let dk = DataKey::ScheduledPrincipal(union.clone(), ln.maturity_ts);
                    i_set(&env, &dk, i_get(&env, &dk) - cut);
                }
            }
        }

        let remaining_principal = if ln.principal > ln.principal_paid {
            ln.principal - ln.principal_paid
        } else {
            0
        };
        let unpaid_interest = if total_accrued > ln.interest_paid {
            total_accrued - ln.interest_paid
        } else {
            0
        };
        let fully = remaining_principal + unpaid_interest <= DUST;
        if fully {
            ViewerClient::new(&env, &cfg(&env).viewer).on_loan_closed(
                &union,
                &loan_id,
                &ln.borrower,
            );
            ln.principal_paid = ln.principal;
            ln.liquidated = true;
            LoanRepaid {
                union: union.clone(),
                loan_id: loan_id.clone(),
                interest_paid: pay_interest,
                principal_paid: principal_for_util,
            }
            .publish(&env);
        }
        save_loan(&env, &union, &loan_id, &ln);
        bump_instance(&env);
        Ok(fully)
    }

    pub fn mark_default(
        env: Env,
        who: Address,
        union: Address,
        loan_id: BytesN<32>,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        let mut ln = load_loan(&env, &union, &loan_id).ok_or(Error::LoanNotExist)?;
        if ln.create_ts == 0 {
            return Err(Error::LoanNotExist);
        }
        if ln.defaulted || ln.liquidated {
            return Err(Error::LoanClosed);
        }
        let mut jm = load_market(&env, true, &union, &ln.loan_type);
        let principal_remaining = if ln.principal > ln.principal_paid {
            ln.principal - ln.principal_paid
        } else {
            0
        };
        if principal_remaining > 0 {
            let ubk = DataKey::UnionBorrows(union.clone());
            let ub = i_get(&env, &ubk);
            i_set(
                &env,
                &ubk,
                if principal_remaining >= ub {
                    0
                } else {
                    ub - principal_remaining
                },
            );
            jm.total_borrows = if principal_remaining >= jm.total_borrows {
                0
            } else {
                jm.total_borrows - principal_remaining
            };
        }
        let (junior_applied, new_idx_j) = haircut_index(
            &env,
            jm.index,
            to_underlying(&env, jm.total_shares, jm.index).unwrap_or(0),
            principal_remaining,
        )
        .ok_or(Error::Overflow)?;
        jm.index = new_idx_j;
        save_market(&env, true, &union, &ln.loan_type, &jm);
        let remaining = if principal_remaining > junior_applied {
            principal_remaining - junior_applied
        } else {
            0
        };
        let mut senior_applied = 0;
        if remaining > 0 {
            let mut sm = load_market(&env, false, &union, &zero_type(&env));
            let (sa, new_idx_s) = haircut_index(
                &env,
                sm.index,
                to_underlying(&env, sm.total_shares, sm.index).unwrap_or(0),
                remaining,
            )
            .ok_or(Error::Overflow)?;
            senior_applied = sa;
            sm.index = new_idx_s;
            save_market(&env, false, &union, &zero_type(&env), &sm);
        }
        ln.defaulted = true;
        ln.liquidated = true;
        if ln.maturity_ts != 0 {
            let sk = DataKey::LoanScheduled(union.clone(), loan_id.clone());
            let sched = i_get(&env, &sk);
            if sched > 0 {
                i_set(&env, &sk, 0);
                let dk = DataKey::ScheduledPrincipal(union.clone(), ln.maturity_ts);
                i_set(&env, &dk, i_get(&env, &dk) - sched);
            }
        }
        save_loan(&env, &union, &loan_id, &ln);
        LoanDefaulted {
            union,
            loan_id,
            junior_applied,
            senior_applied,
        }
        .publish(&env);
        Ok(())
    }

    pub fn remove_loan(
        env: Env,
        who: Address,
        union: Address,
        loan_id: BytesN<32>,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        let ln = load_loan(&env, &union, &loan_id).ok_or(Error::LoanNotExist)?;
        if ln.drawdown_ts != 0 || ln.liquidated {
            return Err(Error::BadState);
        }
        let mut jm = load_market(&env, true, &union, &ln.loan_type);
        jm.cash += ln.funded_from_junior;
        jm.total_borrows = if ln.principal >= jm.total_borrows {
            0
        } else {
            jm.total_borrows - ln.principal
        };
        save_market(&env, true, &union, &ln.loan_type, &jm);
        let mut sm = load_market(&env, false, &union, &zero_type(&env));
        sm.cash += ln.funded_from_senior;
        save_market(&env, false, &union, &zero_type(&env), &sm);
        let ubk = DataKey::UnionBorrows(union.clone());
        let ub = i_get(&env, &ubk);
        i_set(
            &env,
            &ubk,
            if ln.principal >= ub {
                0
            } else {
                ub - ln.principal
            },
        );
        env.storage()
            .persistent()
            .remove(&DataKey::Loan(union.clone(), loan_id.clone()));
        ViewerClient::new(&env, &cfg(&env).viewer).on_loan_closed(&union, &loan_id, &ln.borrower);
        LoanRemoved { union, loan_id }.publish(&env);
        Ok(())
    }

    pub fn report_maturity(
        env: Env,
        who: Address,
        union: Address,
        loan_id: BytesN<32>,
        maturity_ts: u64,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        let mut ln = load_loan(&env, &union, &loan_id).ok_or(Error::LoanNotExist)?;
        if ln.maturity_ts != 0 {
            return Err(Error::MaturityAlreadySet);
        }
        if maturity_ts < ln.create_ts {
            return Err(Error::BadMaturity);
        }
        ln.maturity_ts = maturity_ts;
        ln.milestone = 10;
        let remaining = if ln.principal > ln.principal_paid {
            ln.principal - ln.principal_paid
        } else {
            0
        };
        if remaining > 0 {
            let dk = DataKey::ScheduledPrincipal(union.clone(), maturity_ts);
            i_set(&env, &dk, i_get(&env, &dk) + remaining);
            i_set(
                &env,
                &DataKey::LoanScheduled(union.clone(), loan_id.clone()),
                remaining,
            );
        }
        save_loan(&env, &union, &loan_id, &ln);
        MaturityReported {
            union,
            loan_id,
            maturity_ts,
        }
        .publish(&env);
        Ok(())
    }

    pub fn credit_matured_repayments(
        env: Env,
        who: Address,
        union: Address,
        dates: Vec<u64>,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        if dates.len() > MAX_MATURITY_DATES {
            return Err(Error::CapExceeded);
        }
        let now = env.ledger().timestamp();
        for i in 0..dates.len() {
            let d = dates.get(i).unwrap();
            if d > now {
                return Err(Error::DateInFuture);
            }
            let dk = DataKey::ScheduledPrincipal(union.clone(), d);
            let amt = i_get(&env, &dk);
            if amt == 0 {
                continue;
            }
            i_set(&env, &dk, 0);
            let ck = DataKey::MaturedCredit(union.clone());
            i_set(&env, &ck, i_get(&env, &ck) + amt);
            MaturitiesCredited {
                union: union.clone(),
                date: d,
                amount: amt,
            }
            .publish(&env);
        }
        Ok(())
    }

    pub fn credit_recovery(
        env: Env,
        who: Address,
        union: Address,
        loan_id: BytesN<32>,
        recovered: i128,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        require_positive(recovered)?;
        let ln = load_loan(&env, &union, &loan_id).ok_or(Error::LoanNotExist)?;
        if !ln.defaulted {
            return Err(Error::NotDefaulted);
        }
        let mut jm = load_market(&env, true, &union, &ln.loan_type);
        jm.cash += recovered;
        save_market(&env, true, &union, &ln.loan_type, &jm);
        RecoveryCredited {
            union,
            loan_id,
            recovered,
        }
        .publish(&env);
        Ok(())
    }

    pub fn report_milestone(
        env: Env,
        who: Address,
        union: Address,
        loan_id: BytesN<32>,
        milestone: u32,
        milestone_digest: BytesN<32>,
    ) -> Result<(), Error> {
        require_oracle_or_leader(&env, &union, &who)?;
        let mut ln = load_loan(&env, &union, &loan_id).ok_or(Error::LoanNotExist)?;
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
            let eta = env.ledger().timestamp() + c.upgrade_delay;
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

    // ---------- Views ----------
    pub fn get_config(env: Env) -> Config {
        cfg(&env)
    }

    pub fn get_senior_market(env: Env, union: Address) -> MarketLite {
        let m = load_market(&env, false, &union, &zero_type(&env));
        MarketLite {
            cash: m.cash,
            index: if m.index == 0 { RAY } else { m.index },
            total_shares: m.total_shares,
            total_borrows: m.total_borrows,
            claimable_principal: m.claimable_principal,
        }
    }

    pub fn get_junior_market(env: Env, union: Address, loan_type: BytesN<32>) -> MarketLite {
        let m = load_market(&env, true, &union, &loan_type);
        MarketLite {
            cash: m.cash,
            index: if m.index == 0 { RAY } else { m.index },
            total_shares: m.total_shares,
            total_borrows: m.total_borrows,
            claimable_principal: m.claimable_principal,
        }
    }

    pub fn get_investor_senior(env: Env, union: Address, investor: Address) -> InvestorLite {
        let inv = load_inv(&env, false, &union, &zero_type(&env), &investor);
        InvestorLite {
            unbond_period: inv.request_ts,
            shares: inv.shares,
            locked: inv.pending + inv.claimable,
            pending: inv.pending,
            pending_principal_snap: inv.pending_principal_snap,
            unclaimed: inv.unclaimed,
            entry_index: inv.entry_index,
        }
    }

    pub fn get_investor_junior(
        env: Env,
        union: Address,
        loan_type: BytesN<32>,
        investor: Address,
    ) -> InvestorLite {
        let inv = load_inv(&env, true, &union, &loan_type, &investor);
        InvestorLite {
            unbond_period: inv.request_ts,
            shares: inv.shares,
            locked: inv.pending + inv.claimable,
            pending: inv.pending,
            pending_principal_snap: inv.pending_principal_snap,
            unclaimed: inv.unclaimed,
            entry_index: inv.entry_index,
        }
    }

    pub fn get_loan(env: Env, union: Address, loan_id: BytesN<32>) -> Option<Loan> {
        load_loan(&env, &union, &loan_id)
    }

    pub fn get_reserve(env: Env, union: Address) -> Option<ReserveCfg> {
        env.storage().persistent().get(&DataKey::Reserve(union))
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
        paused(&env)
    }
}

#[cfg(test)]
mod test;
