#![no_std]
//! Shared types, TTL policy, and cross-contract client traits.
//!
//! Storage policy (TAD §4.3):
//! - instance: facility config, admin, caps, lock terms
//! - persistent: LP positions, loans, union markets
//! - temporary: cash-escrow entries (fx-pool only)

use soroban_sdk::{contractclient, contracttype, Address, BytesN, Env, Vec};

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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvestorLite {
    pub unbond_period: u64,
    pub shares: i128,
    pub locked: i128,
    pub pending: i128,
    pub pending_principal_snap: i128,
    pub unclaimed: i128,
    pub entry_index: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Loan {
    pub borrower: Address,
    pub loan_type: BytesN<32>,
    pub principal: i128,
    pub rate_bp: u32,
    pub create_ts: u64,
    pub drawdown_ts: u64,
    pub maturity_ts: u64,
    pub principal_paid: i128,
    pub interest_paid: i128,
    pub funded_from_junior: i128,
    pub funded_from_senior: i128,
    pub defaulted: bool,
    pub liquidated: bool,
    pub milestone: u32,
    pub milestone_digest: BytesN<32>,
    pub digest_ts: u64,
    pub last_accrual_ts: u64,
    pub interest_accrued: i128,
    pub lower_rate: bool,
    pub sos_date: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReserveCfg {
    pub safety_bp: u32,
    pub safety_floor: i128,
    pub hard_stop: bool,
    pub exists: bool,
    pub escrow_duration: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberRecord {
    pub union: Address,
    pub member: Address,
    pub polygon_address: BytesN<20>,
    pub plot_ids: Vec<u64>,
    pub cycle: u32,
    pub confirmed_ts: u64,
    pub active: bool,
}

/// Membership gate the fund, anchor, and nIN trustline share (TAD §4.2).
#[contractclient(name = "MembershipClient")]
pub trait MembershipTrait {
    fn is_member(union: Address, account: Address) -> bool;
    fn plots_of(union: Address, account: Address) -> Vec<u64>;
}

#[contractclient(name = "RolesClient")]
pub trait RolesTrait {
    fn is_oracle(account: Address) -> bool;
    fn is_leader(union: Address, account: Address) -> bool;
}

#[contractclient(name = "ViewerClient")]
pub trait ViewerTrait {
    fn on_loan_created(union: Address, loan_id: BytesN<32>, borrower: Address);
    fn on_loan_closed(union: Address, loan_id: BytesN<32>, borrower: Address);
}

/// Flattened FX callback: Core passes amount; FX does not read back into Core.
#[contractclient(name = "FxPoolClient")]
pub trait FxPoolTrait {
    fn resolve_escrow_cash(escrow_id: u64, amount: i128);
}
