use soroban_sdk::{contracterror, contracttype, Address, BytesN};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotOracleOrLeader = 1,
    AmountZero = 2,
    ReserveStop = 3,
    InsufficientCash = 4,
    NothingToClaim = 5,
    NotEligibleMaturity = 6,
    NftRequired = 7,
    NftForbidden = 8,
    NotExists = 10,
    BadShares = 12,
    BadState = 13,
    BadArg = 14,
    LoanNotExist = 15,
    LoanClosed = 16,
    MaturityAlreadySet = 20,
    BadMaturity = 21,
    NotDefaulted = 23,
    Paused = 27,
    NotAdmin = 28,
    Overflow = 29,
    NotFxPool = 30,
    UpgradeNotReady = 31,
    CapExceeded = 32,
    NegativeAmount = 33,
    /// Less than the gate (600 nIN) invested in the union's junior pool (R26).
    BelowGate = 34,
    /// A loan of the union is past its payback date and not repaid (R34).
    LendingOnHold = 35,
    /// The oracle has not reported the end of season: no overdue, no default.
    NoMaturity = 36,
    /// Payback date not reached yet.
    NotDue = 37,
    LoanExists = 38,
    TooManyLoans = 39,
    NotOracle = 40,
    NotLeader = 41,
    /// The pool was written off in full; it takes no new money.
    PoolWrittenOff = 42,
    NotAttester = 43,
    /// No attested food token of hers under that id.
    NoFoodToken = 44,
    /// A loan was already drawn against this food token.
    FoodTokenUsed = 45,
    /// Before the food token's from-date.
    NotOpenYet = 46,
    /// More cash than the food token's attested amount.
    AboveFoodToken = 47,
    /// The treasury would fall below the union's cash at hand in fx_pool (R15).
    CashAtHand = 48,
    /// fx_pool is set once; only a timelocked upgrade can repoint it.
    FxPoolAlreadySet = 49,
    /// Above the 30-day maximum.
    BadDelay = 50,
    /// Above the maximum loan rate in code (C14).
    AboveMaxRate = 51,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub admin: Address,
    pub roles: Address,
    pub land_title: Address,
    pub nin: Address,
    /// fx_pool: the cash-scan entry point for all money coming in, and the nIN SAC admin.
    pub fx_pool: Option<Address>,
}

/// Per-union lending terms, set by the admin (R26, R34, R37, R42, R76).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionConfig {
    /// Largest cash part per loan: ₹10,000 at launch (R34).
    pub max_loan: i128,
    /// Junior investment needed before a draw: 600 nIN (R26).
    pub gate: i128,
    /// Simple interest on the cash part, 12.5% (R37).
    pub rate_bp: u32,
    /// Share of the interest to the union treasury, 1% (R42).
    pub treasury_fee_bp: u32,
    /// Share of the interest to the rainy-day reserve, 0.77% (R42).
    pub rainy_fee_bp: u32,
    pub safety_bp: u32,
    pub safety_floor: i128,
    pub hard_stop: bool,
    /// Cap each draw at the food token's attested amount. Off during testing: the union may
    /// give more than attested, within `max_loan`.
    pub enforce_token_amount: bool,
}

/// Food token fact written by the attester (F2): copied from the Polygon FoodToken of a crop
/// plan. Until T3's plan engine, the attestation is how a loan is approved.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoodToken {
    pub member: Address,
    /// Most cash that may be drawn against it.
    pub amount: i128,
    /// Loans accepted from this date.
    pub from_date: u64,
    /// The loan drawn against it; one loan per token.
    pub loan_id: Option<BytesN<32>>,
}

/// A member's junior position (R26, R27, R29), in nIN.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    /// Value of her shares that are not on their way out.
    pub invested: i128,
    /// What she owes on open loans, principal plus interest, up to `invested` (R27).
    pub locked: i128,
    /// `invested - locked`: what she may ask to withdraw.
    pub free: i128,
    pub pending_shares: i128,
    pub claimable_shares: i128,
    /// Value of all her shares minus what she paid in and has not taken out (R29).
    pub earned: i128,
    /// Unbond request time; claim opens 14 days later.
    pub request_ts: u64,
}

#[contracttype]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Market {
    pub cash: i128,
    pub index: i128,
    pub total_shares: i128,
    pub total_borrows: i128,
    pub claimable_principal: i128,
    pub exists: bool,
}

#[contracttype]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Investor {
    pub shares: i128,
    pub pending: i128,
    pub claimable: i128,
    pub request_ts: u64,
    /// nIN paid in and not yet taken out, for "earned" (R29). Yield is never paid
    /// separately: it is in the share value and leaves with the shares.
    pub cost: i128,
    pub pending_principal_snap: i128,
}
