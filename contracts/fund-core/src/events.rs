use nila_common::Tranche;
use soroban_sdk::{contractevent, Address, BytesN};

#[contractevent]
pub struct UnionConfigSet {
    #[topic]
    pub union: Address,
    pub max_loan: i128,
    pub gate: i128,
    pub rate_bp: u32,
}

#[contractevent]
pub struct FxPoolSet {
    pub fx_pool: Address,
}

#[contractevent]
pub struct Deposit {
    #[topic]
    pub investor: Address,
    #[topic]
    pub union: Address,
    pub tranche: Tranche,
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
    pub shares: i128,
    pub principal_snap: i128,
}

#[contractevent]
pub struct Claimed {
    #[topic]
    pub investor: Address,
    #[topic]
    pub union: Address,
    pub tranche: Tranche,
    pub amount: i128,
}

#[contractevent]
pub struct FoodTokenAttested {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub member: Address,
    pub amount: i128,
    pub from_date: u64,
}

/// For the attester's F3 fact (R31): the food token is now redeemed.
#[contractevent]
pub struct LoanDrawn {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub borrower: Address,
    pub token_id: BytesN<32>,
    pub cash: i128,
    pub rate_bp: u32,
    pub escrow_id: u64,
}

#[contractevent]
pub struct LoanRepaid {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub interest_paid: i128,
    pub principal_paid: i128,
    pub closed: bool,
}

#[contractevent]
pub struct LoanOverdue {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub due_ts: u64,
}

#[contractevent]
pub struct LoanDefaulted {
    #[topic]
    pub union: Address,
    #[topic]
    pub loan_id: BytesN<32>,
    pub member_applied: i128,
    pub junior_applied: i128,
    /// Loss the junior pool could not cover; senior is never cut for a cash part.
    pub shortfall: i128,
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
