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
    /// Collected over the desk (`claim_junior_cash`): burned and booked against cash at hand, not paid
    /// to a wallet (executive decision 22 Oct).
    pub cash: bool,
    /// The part of `amount` booked against cash at hand (`fx_pool.settle_cash`), as `LoanDrawn.cash_settled`;
    /// 0 on the wallet paths. Less than `amount` when the desk's cash at hand was short (desk reconciliation).
    pub cash_settled: i128,
}

#[contractevent]
pub struct FoodTokenAttested {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub member: Address,
    pub amount: i128,
    pub hours: u32,
    /// The last draw date (25 Oct; was from_date, the SOS floor).
    pub final_date: u64,
}

/// An undrawn food token past its last draw date, lapsed by anyone (25 Oct). The oracle then burns the Polygon
/// FoodToken (burnToken) after its own checks, so its fields are free again for that season. No funds move.
#[contractevent]
pub struct FoodTokenLapsed {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub member: Address,
    pub final_date: u64,
}

/// For the attester's F3 fact (R31): the food token is now redeemed.
#[contractevent]
pub struct LoanDrawn {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub borrower: Address,
    pub cash: i128,
    pub hours: u32,
    /// The voucher's value lent by senior with it (26 Oct): owed with the cash.
    pub voucher: i128,
    pub rate_bp: u32,
    /// How much of the union's cash at hand in fx_pool the draw settled (D9).
    pub cash_settled: i128,
}

#[contractevent]
pub struct LoanRepaid {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub interest_paid: i128,
    pub principal_paid: i128,
    /// To the union treasury: the 5% fee of handed-back voucher hours (26 Oct).
    pub fee_paid: i128,
    /// To senior: the voucher's value (its interest is in interest_paid).
    pub senior_paid: i128,
    pub closed: bool,
}

#[contractevent]
pub struct LoanOverdue {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub due_ts: u64,
}

#[contractevent]
pub struct LoanDefaulted {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub member_applied: i128,
    pub junior_applied: i128,
    pub senior_applied: i128,
    /// Interest accrued and unpaid at the default, written off with the principal.
    pub interest_written_off: i128,
    /// Owners' pending parts paid at the default (D17), and any left unpaid for lack of cash.
    pub dues_paid: i128,
    pub dues_unpaid: i128,
    /// Senior voucher prepay her investment covered, and the senior loss on the rest.
    pub senior_prepay_covered: i128,
    pub senior_prepay_lost: i128,
}

#[contractevent]
pub struct RecoveryCredited {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub recovered: i128,
}

#[contractevent]
pub struct MaturityReported {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub maturity_ts: u64,
}

#[contractevent]
pub struct MilestoneReported {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub milestone: u32,
    pub milestone_digest: BytesN<32>,
}

#[contractevent]
pub struct Paused {
    pub paused: bool,
}

/// Treasury in (cash scan or a leader's own nIN, C10; positive) or out to a leader (negative), R17.
#[contractevent]
pub struct TreasuryMoved {
    #[topic]
    pub union: Address,
    pub amount: i128,
    pub rainy: bool,
}

/// An fx_pool escrow expired unsettled; its nIN burned from the treasury (R15).
#[contractevent]
pub struct EscrowCovered {
    #[topic]
    pub union: Address,
    pub amount: i128,
    pub covered: i128,
}

#[contractevent]
pub struct MachinerySet {
    pub machinery: Address,
}

/// An owner claimed hours on her voucher: prepay paid from senior, the rest pending (R85).
#[contractevent]
pub struct VoucherPaid {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub owner: Address,
    pub hours: u32,
    pub prepay: i128,
    pub pending: i128,
}

#[contractevent]
pub struct DuesCollected {
    #[topic]
    pub union: Address,
    #[topic]
    pub owner: Address,
    pub amount: i128,
}

/// She released a voucher loan that owed nothing: closed, unclaimed hours lapse.
#[contractevent]
pub struct VoucherReturned {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    /// Centi tractor hours handed back, their value off her debt, and the 5% owed to the treasury.
    pub units: u32,
    pub value: i128,
    pub fee: i128,
}
