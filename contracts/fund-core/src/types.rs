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
    NotMachinery = 52,
    /// Senior money not free to back the voucher prepay (R36, C8).
    NoSeniorBacking = 53,
    /// More machine owners on one loan than the book holds.
    TooManyOwners = 54,
    MachineryAlreadySet = 55,
    /// Nothing is owed yet (a voucher-only loan before any claim).
    NothingOwed = 56,
    NotBorrower = 57,
    /// No crop calendar, or the planned sowing is in none of its crop's windows (25 Oct).
    BadDates = 58,
    /// The food token lapsed unused after its last draw date (25 Oct): it can no longer be drawn or attested.
    FoodTokenLapsed = 59,
    /// After the food token's last draw date: too late for this cycle (25 Oct).
    TooLate = 60,
    /// The food token's last draw date has not passed yet (25 Oct).
    NotLapsedYet = 61,
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
    /// machinery: machine registry, vouchers and owner claims (C5).
    pub machinery: Option<Address>,
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

pub const FLAG_OVERDUE: u32 = 1;
pub const FLAG_DEFAULTED: u32 = 2;
pub const FLAG_CLOSED: u32 = 4;
/// A loan was drawn against the token.
pub const FLAG_DRAWN: u32 = 8;

impl LoanState {
    pub fn none() -> Self {
        LoanState {
            principal: 0,
            rate_bp: 0,
            drawdown_ts: 0,
            maturity_ts: 0,
            principal_paid: 0,
            interest_paid: 0,
            interest_accrued: 0,
            last_accrual_ts: 0,
            flags: 0,
        }
    }

    pub fn drawn(&self) -> bool {
        self.flags & FLAG_DRAWN != 0
    }
}

/// A loan's money state, stored inside its food token entry (C6).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoanState {
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
    /// FLAG_OVERDUE | FLAG_DEFAULTED | FLAG_CLOSED | FLAG_DRAWN.
    pub flags: u32,
}

/// Food token fact written by the attester (F2): copied from the Polygon FoodToken of a crop
/// plan. Until T3's plan engine, the attestation is how a loan is approved.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoodToken {
    pub member: Address,
    /// The plan's recommended cash part; a cap only when the union's switch is on (9 Oct).
    pub amount: i128,
    /// The plan's recommended machinery hours; a cap only with the same switch.
    pub hours: u32,
    /// The opening of its window (25 Oct): a week before its sowing window, attested (`FoodTokenFinal` ends it); a token attested
    /// before 25 Oct keeps its season's SOS here.
    pub from_date: u64,
    /// The loan drawn against it (C6: the loan lives here; the token id is the loan id).
    /// All zero until drawn; `FLAG_DRAWN` says a loan exists (one per token).
    pub loan: LoanState,
}

/// A food token's last draw date and whether it lapsed unused (executive decision 25 Oct). Its own key, so the
/// `FoodToken` entries already stored keep their layout; a token without one has no last date.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoodTokenFinal {
    /// The last sowing date for the crop in its season: a loan may be drawn up to and including it.
    pub final_date: u64,
    /// Lapsed by anyone after `final_date` while undrawn: never drawn or attested again.
    pub lapsed: bool,
}

/// What `food_token` returns: the stored fact with its last draw date (0 = none) and the lapsed flag.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoodTokenView {
    pub member: Address,
    pub amount: i128,
    pub hours: u32,
    pub from_date: u64,
    pub final_date: u64,
    pub lapsed: bool,
    pub loan: LoanState,
}

/// The oracle's last crop milestone for a loan: its own entry, written only when reported.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Milestone {
    pub milestone: u32,
    pub digest: BytesN<32>,
    pub ts: u64,
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

/// A loan's voucher part (executive decision 26 Oct, revises D6): its value is lent by senior at
/// the draw and is debt from then on, with interest that senior earns (senior keeps USDC on-chain
/// to back it). The hours are hers to use whenever she likes: owners are paid in full from the
/// reserve, also after the loan is repaid. Handing hours back takes their whole value off her
/// debt and adds 5% of it owed to the union treasury.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoucherBook {
    pub hours: u32,
    /// The value of one centi tractor hour ×100 (one tractor hour) at the draw.
    pub per_hour: i128,
    /// Lent at the draw (hours × per_hour / 100), the senior principal.
    pub value: i128,
    /// Senior principal repaid or taken off by handed-back hours.
    pub repaid: i128,
    pub interest_accrued: i128,
    pub interest_paid: i128,
    pub last_accrual_ts: u64,
    /// Senior money set aside for her unclaimed hours (pays owners; outside senior cash).
    pub reserve: i128,
    /// 5% of handed-back hours' value, owed to the union treasury.
    pub fee_due: i128,
}

/// One union's counters in one entry (C6): fewer entries, less rent per call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionBook {
    /// Cash lent out and not yet repaid or written off.
    pub borrows: i128,
    pub treasury: i128,
    pub rainy: i128,
    /// Senior cash held back for unclaimed voucher hours (R36).
    pub earmark: i128,
    /// Loans past their payback date and not repaid or defaulted (R34).
    pub on_hold: u32,
}
