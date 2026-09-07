use soroban_sdk::{contracterror, contracttype};

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
    SignatureInvalid = 9,
    NotExists = 10,
    BadKink = 11,
    BadShares = 12,
    BadState = 13,
    BadArg = 14,
    LoanNotExist = 15,
    LoanClosed = 16,
    LoanHasBeenAccepted = 17,
    MaxLoanAmount = 18,
    InterestNotSettled = 19,
    MaturityAlreadySet = 20,
    BadMaturity = 21,
    DateInFuture = 22,
    NotDefaulted = 23,
    NoYield = 24,
    VoucherAmountTooHigh = 25,
    BadRatio = 26,
    Paused = 27,
    NotAdmin = 28,
    Overflow = 29,
    NotFxPool = 30,
    UpgradeNotReady = 31,
    CapExceeded = 32,
    NegativeAmount = 33,
}

/// Borrower-chosen terms plus oracle-authorized bounds (Stellar voucher).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DrawParams {
    pub loan_id: soroban_sdk::BytesN<32>,
    pub loan_type: soroban_sdk::BytesN<32>,
    pub amount: i128,
    pub rate_bp: u32,
    pub maturity_ts: u64,
    pub max_amount: i128,
    pub min_rate_bp: u32,
    pub fast_draw: bool,
    pub escrow_id: u64,
    pub sos_date: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub admin: soroban_sdk::Address,
    pub roles: soroban_sdk::Address,
    pub membership: soroban_sdk::Address,
    pub viewer: soroban_sdk::Address,
    pub nin: soroban_sdk::Address,
    pub fx_pool: Option<soroban_sdk::Address>,
    pub treasury_fee_bp: u32,
    pub rainy_fee_bp: u32,
    pub per_loan_cap: i128,
    pub upgrade_delay: u64,
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
    pub entry_index: i128,
    pub unclaimed: i128,
    pub pending_principal_snap: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub wasm_hash: soroban_sdk::BytesN<32>,
    pub eta: u64,
}
