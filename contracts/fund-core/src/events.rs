use nila_common::Tranche;
use soroban_sdk::{contractevent, Address, BytesN};

#[contractevent]
pub struct UnionReserveUpdated {
    #[topic]
    pub union: Address,
    pub safety_bp: u32,
    pub safety_floor: i128,
    pub hard_stop: bool,
}

#[contractevent]
pub struct RateParamsUpdated {
    #[topic]
    pub union: Address,
    pub base_rate_bp: u32,
    pub kink_util_bp: u32,
    pub slope1_bp: u32,
    pub slope2_bp: u32,
    pub max_rate_bp: u32,
}

#[contractevent]
pub struct JuniorMarketCreated {
    #[topic]
    pub union: Address,
    pub loan_type: BytesN<32>,
}

#[contractevent]
pub struct Deposit {
    #[topic]
    pub investor: Address,
    #[topic]
    pub union: Address,
    pub tranche: Tranche,
    pub loan_type: BytesN<32>,
    pub amount: i128,
    pub shares_out: i128,
}

#[contractevent]
pub struct UnbondRequested {
    #[topic]
    pub investor: Address,
    #[topic]
    pub union: Address,
    pub tranche: Tranche,
    pub loan_type: BytesN<32>,
    pub shares: i128,
    pub principal_snap: i128,
}

#[contractevent]
pub struct UnbondPromoted {
    #[topic]
    pub investor: Address,
    #[topic]
    pub union: Address,
    pub tranche: Tranche,
    pub loan_type: BytesN<32>,
    pub shares: i128,
}

#[contractevent]
pub struct Claimed {
    #[topic]
    pub investor: Address,
    #[topic]
    pub union: Address,
    pub tranche: Tranche,
    pub loan_type: BytesN<32>,
    pub amount: i128,
}

#[contractevent]
pub struct YieldClaimed {
    #[topic]
    pub investor: Address,
    #[topic]
    pub union: Address,
    pub tranche: Tranche,
    pub loan_type: BytesN<32>,
    pub amount: i128,
}

#[contractevent]
pub struct LoanClaimed {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub borrower: Address,
    pub loan_type: BytesN<32>,
    pub amount: i128,
    pub rate_bp: u32,
    pub sos_date: u64,
    pub drawdown_ts: u64,
    pub fast_draw: bool,
}

#[contractevent]
pub struct LoanAccepted {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub amount: i128,
    pub borrower: Address,
}

#[contractevent]
pub struct LoanRepaid {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub interest_paid: i128,
    pub principal_paid: i128,
}

#[contractevent]
pub struct LoanDefaulted {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub junior_applied: i128,
    pub senior_applied: i128,
}

#[contractevent]
pub struct LoanRemoved {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
}

#[contractevent]
pub struct RecoveryCredited {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub recovered: i128,
}

#[contractevent]
pub struct MaturityReported {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub maturity_ts: u64,
}

#[contractevent]
pub struct MilestoneReported {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub milestone: u32,
    pub milestone_digest: BytesN<32>,
}

#[contractevent]
pub struct MaturitiesCredited {
    #[topic]
    pub union: Address,
    pub date: u64,
    pub amount: i128,
}

#[contractevent]
pub struct Paused {
    pub paused: bool,
}

#[contractevent]
pub struct UpgradeProposed {
    pub wasm_hash: BytesN<32>,
    pub eta: u64,
}

#[contractevent]
pub struct UpgradeApplied {
    pub wasm_hash: BytesN<32>,
}

#[contractevent]
pub struct CapUpdated {
    pub per_loan_cap: i128,
}
