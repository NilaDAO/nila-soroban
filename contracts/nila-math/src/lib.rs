#![no_std]
//! Index, share, interest, and haircut arithmetic for the Nila lending facility.
//!
//! Polygon used 18-decimal nIN and `uint256` products. Stellar tokens are `i128`
//! (SEP-41). nIN is issued at **7 decimals** so typical products fit in `i128`;
//! every `amount × RAY / index` style product still goes through `U256` so a
//! full-book utilisation cannot overflow the host integer.
//!
//! Formulas are the GenericFundMathLib / Maker-style RAY index:
//! - shares = amount × RAY / index
//! - underlying = shares × index / RAY
//! - simple interest = principal × rateBP × elapsed / (10_000 × YEAR)

use soroban_sdk::{contracttype, Env, U256};

/// Maker-style RAY (1e27). Index starts at RAY (= 1.0).
pub const RAY: i128 = 1_000_000_000_000_000_000_000_000_000;

/// Ratio scale used for junior/senior bucket thresholds (1e18).
/// TAD §7.1: "threshold ratio rescaled off WAD" — we keep 18-dec WAD for
/// ratio precision; amounts themselves are 7-dec nIN.
pub const WAD: i128 = 1_000_000_000_000_000_000;

/// Basis-point denominator.
pub const BPS: i128 = 10_000;

/// Seconds in a 365-day year (matches Solidity `365 days`).
pub const YEAR: u64 = 365 * 24 * 60 * 60;

/// Default unbonding window (14 days).
pub const DEFAULT_UNBONDING: u64 = 14 * 24 * 60 * 60;

/// nIN decimals on Stellar (classic asset / SAC).
pub const NIN_DECIMALS: u32 = 7;

/// One whole nIN (₹1) in atomic units.
pub const NIN: i128 = 10_000_000;

/// Dust close threshold: 0.1 nIN at 7 decimals (Solidity used 1e17 at 18 dec).
pub const DUST: i128 = 1_000_000;

/// Tranche-1 per-loan / per-swap cap: ₹10,000 at 7 decimals.
pub const DEFAULT_PER_LOAN_CAP: i128 = 10_000 * NIN;

/// Default protocol fees (basis points of interest).
pub const DEFAULT_TREASURY_FEE_BP: u32 = 100; // 1%
pub const DEFAULT_RAINY_FEE_BP: u32 = 200; // 2%

/// Kinked utilisation rate curve (per union).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateParams {
    pub base_rate_bp: u32,
    pub kink_util_bp: u32,
    pub slope1_bp: u32,
    pub slope2_bp: u32,
    pub max_rate_bp: u32,
}

/// `floor(a * b / d)` with U256 intermediates. Returns `None` on sign/zero/overflow.
pub fn mul_div(env: &Env, a: i128, b: i128, d: i128) -> Option<i128> {
    if a < 0 || b < 0 || d <= 0 {
        return None;
    }
    let prod = U256::from_u128(env, a as u128).checked_mul(&U256::from_u128(env, b as u128))?;
    let quot = prod.checked_div(&U256::from_u128(env, d as u128))?;
    quot.to_u128().and_then(|x| i128::try_from(x).ok())
}

/// Convert an nIN amount into shares at `index` (RAY-scaled). Index 0 is a pool written off
/// in full, not "unset": it has no share price, so this returns `None`.
pub fn to_shares(env: &Env, amount: i128, index: i128) -> Option<i128> {
    mul_div(env, amount, RAY, index)
}

/// Convert shares back to nIN at `index` (RAY-scaled). At index 0 shares are worth nothing.
pub fn to_underlying(env: &Env, shares: i128, index: i128) -> Option<i128> {
    mul_div(env, shares, index, RAY)
}

/// Simple interest on leftover principal over `elapsed` seconds.
pub fn accrued_interest(env: &Env, principal: i128, rate_bp: u32, elapsed: u64) -> Option<i128> {
    if principal <= 0 || rate_bp == 0 || elapsed == 0 {
        return Some(0);
    }
    let denom = BPS.checked_mul(i128::from(YEAR))?;
    let num_left = mul_div(env, principal, i128::from(rate_bp), 1)?;
    mul_div(env, num_left, i128::from(elapsed), denom)
}

/// Junior-first (then senior) index haircut. Returns `(applied, new_index)`.
pub fn haircut_index(env: &Env, index: i128, underlying: i128, loss: i128) -> Option<(i128, i128)> {
    if loss <= 0 || index == 0 || underlying <= 0 {
        return Some((0, index));
    }
    let applied = if loss < underlying { loss } else { underlying };
    let remaining = underlying.checked_sub(applied)?;
    let new_index = mul_div(env, index, remaining, underlying)?;
    Some((applied, new_index))
}

/// Bump a RAY index by distributing `cash_in` across `total_shares`.
pub fn index_bump(env: &Env, index: i128, total_shares: i128, cash_in: i128) -> Option<i128> {
    if cash_in <= 0 || total_shares <= 0 {
        return Some(index);
    }
    let delta = mul_div(env, cash_in, RAY, total_shares)?;
    index.checked_add(delta)
}

/// Junior / senior share ratio in WAD. `None` if senior underlying is zero
/// (caller treats that as "ratio check disabled / passes").
pub fn bucket_ratio_wad(
    env: &Env,
    junior_underlying: i128,
    senior_underlying: i128,
) -> Option<i128> {
    if senior_underlying <= 0 {
        return None;
    }
    mul_div(env, junior_underlying, WAD, senior_underlying)
}

/// Kinked utilisation curve → borrow rate in basis points.
pub fn preview_rate_bp(params: &RateParams, util_bp: u32) -> u32 {
    if params.kink_util_bp == 0 {
        return params.base_rate_bp.min(params.max_rate_bp);
    }
    let rate = if util_bp <= params.kink_util_bp {
        params.base_rate_bp + params.slope1_bp.saturating_mul(util_bp) / params.kink_util_bp
    } else {
        let excess = util_bp - params.kink_util_bp;
        let denom = BPS as u32 - params.kink_util_bp;
        let extra = if denom == 0 {
            0
        } else {
            params.slope2_bp.saturating_mul(excess) / denom
        };
        params
            .base_rate_bp
            .saturating_add(params.slope1_bp)
            .saturating_add(extra)
    };
    rate.min(params.max_rate_bp)
}

/// Utilisation in basis points: borrows / (idle cash + borrows).
pub fn utilisation_bp(env: &Env, borrows: i128, idle_cash: i128) -> Option<u32> {
    let denom = borrows.checked_add(idle_cash)?;
    if denom <= 0 {
        return Some(0);
    }
    let ratio = mul_div(env, borrows, BPS, denom)?;
    u32::try_from(ratio).ok()
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn shares_round_trip_at_par() {
        let env = Env::default();
        let amount = 1_000 * NIN;
        let shares = to_shares(&env, amount, RAY).unwrap();
        assert_eq!(shares, amount);
        assert_eq!(to_underlying(&env, shares, RAY).unwrap(), amount);
    }

    #[test]
    fn index_growth_dilutes_later_deposits() {
        let env = Env::default();
        // First depositor: 100 nIN at RAY → 100 nIN shares.
        let s1 = to_shares(&env, 100 * NIN, RAY).unwrap();
        // Interest of 100 nIN doubles the index.
        let idx2 = index_bump(&env, RAY, s1, 100 * NIN).unwrap();
        assert_eq!(idx2, 2 * RAY);
        // Second depositor of 100 nIN gets half the shares.
        let s2 = to_shares(&env, 100 * NIN, idx2).unwrap();
        assert_eq!(s2, 50 * NIN);
        assert_eq!(to_underlying(&env, s1, idx2).unwrap(), 200 * NIN);
        assert_eq!(to_underlying(&env, s2, idx2).unwrap(), 100 * NIN);
    }

    #[test]
    fn haircut_hits_junior_index() {
        let env = Env::default();
        let shares = 1_000 * NIN;
        let underlying = to_underlying(&env, shares, RAY).unwrap();
        let (applied, new_idx) = haircut_index(&env, RAY, underlying, 100 * NIN).unwrap();
        assert_eq!(applied, 100 * NIN);
        assert_eq!(to_underlying(&env, shares, new_idx).unwrap(), 900 * NIN);
    }

    #[test]
    fn interest_12_percent_one_year() {
        let env = Env::default();
        let principal = 10_000 * NIN;
        let interest = accrued_interest(&env, principal, 1_200, YEAR).unwrap();
        assert_eq!(interest, 1_200 * NIN); // 12% of 10,000
    }

    #[test]
    fn mul_div_rejects_negatives() {
        let env = Env::default();
        assert!(mul_div(&env, -1, 2, 3).is_none());
        assert!(mul_div(&env, 1, 2, 0).is_none());
    }

    #[test]
    fn rate_curve_kink() {
        let p = RateParams {
            base_rate_bp: 200,
            kink_util_bp: 8_000,
            slope1_bp: 400,
            slope2_bp: 2_000,
            max_rate_bp: 5_000,
        };
        assert_eq!(preview_rate_bp(&p, 0), 200);
        assert_eq!(preview_rate_bp(&p, 8_000), 600);
        assert!(preview_rate_bp(&p, 10_000) > preview_rate_bp(&p, 8_000));
        assert_eq!(preview_rate_bp(&p, 10_000), 2_600);
    }

    #[test]
    fn large_product_fits_via_u256() {
        let env = Env::default();
        // ₹8.2M book at 7 decimals × RAY would overflow i128 if multiplied naively.
        let book = 8_200_000 * NIN;
        let shares = to_shares(&env, book, RAY).unwrap();
        assert_eq!(shares, book);
    }

    #[test]
    fn index_zero_is_a_written_off_pool() {
        let env = Env::default();
        assert_eq!(to_underlying(&env, 1_000, 0), Some(0));
        assert_eq!(to_shares(&env, 1_000, 0), None);
    }
}
