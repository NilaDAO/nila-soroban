#![no_std]
//! Shared types, TTL policy, and cross-contract client traits.
//!
//! Storage policy (TAD §4.3):
//! - instance: facility config, admin, caps, lock terms
//! - persistent: LP positions, loans, union markets
//! - persistent: cash-escrow and cash-out entries (fx-pool; kept until resolved)

use soroban_sdk::{contractclient, contracttype, Address, BytesN, Env};

pub mod upgrade;

/// ~5s ledgers → 17,280 ledgers / day (network-configured; documented assumption).
pub const DAY_IN_LEDGERS: u32 = 17_280;
/// Extend instance/persistent when TTL falls below 30 days.
pub const TTL_THRESHOLD: u32 = 30 * DAY_IN_LEDGERS;
/// Extend to 120 days (TAD: instance extended on every interaction).
pub const TTL_BUMP: u32 = 120 * DAY_IN_LEDGERS;

pub fn bump_instance(env: &Env) {
    env.storage().instance().extend_ttl(TTL_THRESHOLD, TTL_BUMP);
}

/// Tranche discriminant. Matches `ICore.Tranche`.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tranche {
    Junior = 0,
    Senior = 1,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketLite {
    pub cash: i128,
    pub index: i128,
    pub total_shares: i128,
    pub total_borrows: i128,
    pub claimable_principal: i128,
}

/// One crop-share cash loan (T1: cash part only, junior money only, D5).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Loan {
    pub borrower: Address,
    /// The attested food token the loan was drawn against.
    pub token_id: BytesN<32>,
    /// Cash part handed out by the leader (D9).
    pub principal: i128,
    pub rate_bp: u32,
    pub drawdown_ts: u64,
    /// Detected end of season reported by the oracle; 0 until reported (no EOS, no default).
    pub maturity_ts: u64,
    pub principal_paid: i128,
    pub interest_paid: i128,
    pub interest_accrued: i128,
    pub last_accrual_ts: u64,
    pub overdue: bool,
    pub defaulted: bool,
    pub closed: bool,
    pub milestone: u32,
    pub milestone_digest: BytesN<32>,
    pub digest_ts: u64,
}

/// Membership is holding a land title the union minted (land_title contract).
#[contractclient(name = "LandTitleClient")]
pub trait LandTitleTrait {
    fn holds_title(union: Address, account: Address) -> bool;
}

/// Trust-wide roles in the roles contract, set by the admin (the trust's multisig, R75).
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Role {
    Oracle = 0,
    Attester = 1,
    Core = 2,
}

/// Officer roles inside one union, set by the union account (3 of 5, D12).
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum UnionRole {
    Treasurer = 0,
    Secretary = 1,
}

#[contractclient(name = "RolesClient")]
pub trait RolesTrait {
    fn is_oracle(account: Address) -> bool;
    fn is_leader(union: Address, account: Address) -> bool;
    fn has_role(account: Address, role: Role) -> bool;
}

/// What fund_core asks of fx_pool (C3). fx_pool never reads back into fund_core inside
/// these calls (TAD §7.1).
#[contractclient(name = "FxPoolClient")]
pub trait FxPoolTrait {
    /// The cash part of a draw was handed out of the union's cash at hand (D9, R35).
    fn settle_cash(union: Address, amount: i128) -> i128;
    /// R10: authorise or deauthorise her nIN trustline around a payout.
    fn set_member_auth(union: Address, account: Address, authorized: bool);
    /// Cash at hand: the union's open escrows (R15).
    fn cash_at_hand(union: Address) -> i128;
}

/// What fx_pool's cash scan calls on fund_core (R13–R15, R17). fx_pool signs as the payer.
#[contractclient(name = "FundCoreClient")]
pub trait FundCoreTrait {
    fn deposit_junior(union: Address, member: Address, amount: i128);
    /// mint_senior: an LP's USDC, as nIN, into the union's senior pool (R46).
    fn deposit_senior(union: Address, lp: Address, amount: i128);
    fn repay_loan(union: Address, loan_id: BytesN<32>, amount: i128) -> bool;
    fn credit_recovery(union: Address, loan_id: BytesN<32>, amount: i128);
    fn deposit_union_treasury(union: Address, amount: i128);
    /// An escrow expired unsettled: the union keeps the rupees, so its treasury nIN burns.
    fn cover_expired_escrow(union: Address, amount: i128) -> i128;
    fn union_treasury(union: Address) -> i128;
}
