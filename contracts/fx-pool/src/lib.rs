#![no_std]
//! fx_pool, T1 scope (C3): the union's cash desk on-chain.
//!
//! - Cash scan: the leader records rupees a member brought (invest, repay, recovery) or the
//!   union's own cash (treasury); fx_pool mints the nIN and pays it into fund_core (R13, R14,
//!   R17). Member cash opens a 7-day escrow, the union's cash at hand (R15).
//! - Cash at hand never exceeds the union's treasury; a draw's cash part settles it (D9); an
//!   escrow left unsettled after 7 days is covered by burning treasury nIN.
//! - Cash-out (24 Oct): `cash_out_signed`, her signed QR (NILA:N) scanned by the leader; her nIN
//!   is clawed back. The code cash-out (`request_cash_out`, `reclaim_cash_out`) and the move
//!   (`mint_moved`, `pay_out`) were removed on 4 Nov: the moved Polygon nIN wallets are settled by
//!   hand at the desk, outside the contract.
//! - nIN SAC admin (R8): member trustlines stay deauthorised and are authorised only inside
//!   the call that moves her nIN (R10), and only for a title holder or leader of the union.
//!
//! - Senior route (executive decision 2 Nov): the `senior` contract holds all senior USDC and
//!   calls `mint_senior_nin` to put the nIN at the attester's fix into fund_core's senior pool.
//!   fx_pool holds no senior USDC.
//!
//! - LP offers (C5b, Nov 7, `lp.rs`): Bring and Collect through member LPs the anchor accepted
//!   (R18–R23); `quote` gives the SEP-38 price (C9). Reflector is later (R77).

use nila_common::{bump_instance, upgrade, FundCoreClient, LandTitleClient, Role, RolesClient};
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype,
    token::{StellarAssetClient, TokenClient},
    vec, Address, Bytes, BytesN, Env, IntoVal, Symbol, Vec,
};

contractmeta!(key = "binver", val = "0.3.0");

// Demo builds only (feature `demo-upgrade-delay`): the upgrade timelock is 60 seconds, so a demo deployment can be
// upgraded in place and keep its state. Never for mainnet: the wasm carries this marker, which deploys refuse.
#[cfg(feature = "demo-upgrade-delay")]
soroban_sdk::contractmeta!(
    key = "demo_upgrade_delay",
    val = "demo-upgrade-delay: 60 s, never mainnet"
);
contractmeta!(
    key = "desc",
    val = "Nila fx_pool — cash scan, cash at hand, member cash-out, nIN authoriser"
);
#[cfg(feature = "testnet-short-windows")]
contractmeta!(
    key = "windows",
    val = "testnet-short: one day is ten seconds"
);

#[cfg(not(feature = "testnet-short-windows"))]
pub const DAY: u64 = 86_400;
/// Ten seconds, not one: an escrow or offer must outlive a few ledgers.
#[cfg(feature = "testnet-short-windows")]
pub const DAY: u64 = 10;

/// Cash at hand is lent out or settled within 7 days (R15).
pub const ESCROW_TERM: u64 = 7 * DAY;
/// ₹10,000 per scan and per cash-out at launch (R76), 7 decimals.
pub const DEFAULT_MAX_SCAN: i128 = 10_000 * 10_000_000;
/// Open escrows per union: the leader settles before scanning more.
pub const MAX_OPEN_ESCROWS: u32 = 64;
/// One unit of the INR/USD fix: 7 decimals, like nIN and USDC.
pub const RATE_ONE: i128 = 10_000_000;
/// The attester's fixes are at least 12 hours apart (executive decision 2 Nov).
pub const RATE_MIN_GAP: u64 = 12 * 3_600;
/// One nIN (7 decimals): the cash scan's Collect offer is in whole rupees.
const NIN_UNIT: i128 = 10_000_000;
/// The attester's fix is valid at most 2 days (R77).
pub const MAX_RATE_AGE: u64 = 2 * DAY;
/// Default band for the attester's fix: at most 5% from the last one (R77); beyond, the trust
/// overrides. Open and adjustable (executive decision, 12 Oct): `set_rate_band`.
pub const DEFAULT_RATE_BAND_BP: u32 = 500;
/// The LP's fee: 1% (R21).
pub const DEFAULT_LP_FEE_BP: u32 = 100;
/// Upgrade timelock at deploy: 48 hours (R75, C15); short builds rehearse it in seconds.
#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = 2 * DAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    NotCore = 2,
    NotFound = 3,
    AmountZero = 4,
    CapExceeded = 5,
    StaleOracle = 6,
    Unauthorized = 7,
    Paused = 8,
    NotLeader = 9,
    NotMember = 10,
    /// Cash at hand would exceed the union's treasury (R15).
    AboveTreasury = 11,
    /// Unused since 24 Oct (scan receipts are no longer stored); the code stays reserved.
    ScanUsed = 12,
    TooManyEscrows = 13,
    NotExpired = 14,
    // 15 (Expired) and 16 (CodeUsed) removed 4 Nov with the move's code cash-out; numbers not reused.
    BadExpiry = 17,
    /// A repay or recovery scan names no loan.
    NoLoan = 18,
    CoreAlreadySet = 19,
    UpgradeNotReady = 20,
    /// Above the 30-day maximum.
    BadDelay = 21,
    /// The INR/USD fix is zero, negative or valid too long.
    BadRate = 22,
    NotAnchor = 24,
    NotLp = 25,
    /// The offer is not in the state this call needs.
    OfferState = 26,
    TooManyOffers = 27,
    /// The union's USDC (its treasury USDC for a Bring, the leader-swap USDC for a swap back) does not
    /// cover it.
    NoUsdc = 28,
    // 29 (AlreadyMoved) and 30 (MoveBudget) removed 4 Nov with `mint_moved`; numbers not reused.
    /// The anchor's signed quote no longer matches what the offer pays or takes (C9b).
    QuoteChanged = 31,
    /// The anchor's signed quote has run out (C9b).
    QuoteExpired = 32,
    /// The anchor has not checked this account's identity (K1, 22 Oct).
    NotIdChecked = 33,
    /// No position of this LP in the union (31 Oct).
    NoPosition = 34,
    /// The fix is within the 5% margin of the position's opening rate: no settle (31 Oct).
    InMargin = 35,
    /// The attester's last fix is less than 12 hours old (executive decision 2 Nov).
    RateTooSoon = 36,
    /// The senior contract is set once (2 Nov).
    SeniorAlreadySet = 37,
    /// The LP's account `pay` did not bring fx_pool the USDC asked (review 5 Nov).
    NotPaid = 38,
}

/// INR per USD, 7 decimals (nIN per USDC), set by the attester from the FBIL fix (R77).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateFix {
    pub inr_per_usd: i128,
    pub valid_until: u64,
    pub set_by: Address,
}

/// What the rupees at the desk are for.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Purpose {
    /// Her junior investment (R13, R25).
    Invest = 0,
    /// Her loan repayment, part allowed (R14).
    Repay = 1,
    /// Money recovered after a default.
    Recovery = 2,
    /// Refused since 22 Oct (executive decision): the treasury is the mirror of cash and its nIN is
    /// always USDC-backed, so counted cash never goes into it. Kept for the value's position.
    Treasury = 3,
    /// Rupees an LP brought for a loan the drawer could not pay (Bring, R18): cash at hand
    /// until the draw hands them out (executive decision, 12 Oct). Opened by `confirm`, never by a scan.
    Bring = 4,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Roles,
    LandTitle,
    Usdc,
    Nin,
    FundCore,
    DailyCap,
    MaxScan,
    Paused,
    NextEscrow,
    CashAtHand(Address),
    /// A union's open escrows, oldest first, in one entry (24 Oct).
    OpenEscrows(Address),
    Rate,
    /// The trust's anchor account: it accepts LPs after SEP-12 (R20).
    Anchor,
    /// A member LP of a union (R20).
    LpOf(Address, Address),
    LpFeeBp,
    NextOffer,
    Offer(u64),
    /// Open offer ids of a union.
    OffersOf(Address),
    /// USDC paid out on a day (day index), against the daily cap (R76, C15).
    DayOut(u64),
    /// How far the attester's fix may move from the last one, in bp.
    RateBandBp,
    /// The anchor's swap key (ed25519 public key): it signs each LP accept (C9b, C16). Instance.
    SwapKey,
    /// USDC a union's leaders swapped into its treasury: the backing of that treasury nIN (22 Oct).
    TreasuryUsdc(Address),
    /// An account whose identity the anchor checked (K1, SEP-12): its reference to the anchor's record (22 Oct).
    IdChecked(Address),
    /// USDC a union's id-checked leaders swapped into nIN in their own wallets: paid back only from here (22 Oct).
    LeaderSwapUsdc(Address),
    /// (union, account) -> USDC that account swapped in and has not swapped back (review 5 Nov):
    /// a swap back is capped by it, so nIN from elsewhere never draws on others' backing.
    SwapUsdc(Address, Address),
    /// An LP's position in a union (union, LP), 31 Oct.
    LpPos(Address, Address),
    /// USDC held in fx_pool for a union's LPs' Collect positions (31 Oct).
    LpLocked(Address),
    /// An LP's accepted offer not scanned yet (union, LP): one at a time (31 Oct).
    Busy(Address, Address),
    /// The senior contract (executive decision 2 Nov): it holds all senior USDC and is the one caller of
    /// `mint_senior_nin`. Instance, set once.
    Senior,
    /// When the attester last set the fix (2 Nov): at most one fix per 12 hours. Instance.
    AttesterFixAt,
}

/// One open batch of counted rupees at the union office, kept in the union's one list
/// (`OpenEscrows`, 24 Oct): a scan adds a row instead of a record of its own, so it pays no
/// rent for a new entry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenEscrow {
    pub id: u64,
    pub member: Address,
    pub purpose: Purpose,
    pub amount: i128,
    pub remaining: i128,
    pub expires: u64,
}

#[contractevent]
pub struct FundCoreSet {
    pub fund_core: Address,
}

#[contractevent]
pub struct MaxScanSet {
    pub max_scan: i128,
}

#[contractevent]
pub struct CashScanned {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub purpose: Purpose,
    /// Rupees taken in and nIN minted (after any repay change was handed back).
    pub amount: i128,
    pub scan_hash: BytesN<32>,
    pub escrow_id: u64,
    /// The Collect offer this scan opened for its cash above the treasury (24 Oct); 0 for none.
    pub collect_offer: u64,
}

#[contractevent]
pub struct CashSettled {
    #[topic]
    pub union: Address,
    pub amount: i128,
    pub cash_at_hand: i128,
}

#[contractevent]
pub struct EscrowExpired {
    #[topic]
    pub union: Address,
    #[topic]
    pub escrow_id: u64,
    pub remaining: i128,
    pub covered: i128,
}

/// A signed cash-out at the desk (24 Oct): her nIN burned, the rupees counted out to her.
#[contractevent]
pub struct CashOutSigned {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub leader: Address,
    pub amount: i128,
}

#[contractevent]
pub struct DailyCapSet {
    pub daily_cap: i128,
}

#[contractevent]
pub struct Activated {
    pub fund_core: Address,
}

/// The senior contract set (2 Nov).
#[contractevent]
pub struct SeniorSet {
    pub senior: Address,
}

/// Senior nIN minted for the senior contract (2 Nov): into the union's senior pool in `investor`'s name.
#[contractevent]
pub struct SeniorNinMinted {
    #[topic]
    pub union: Address,
    #[topic]
    pub investor: Address,
    pub nin: i128,
}

#[contractevent]
pub struct RateSet {
    pub inr_per_usd: i128,
    pub valid_until: u64,
    pub set_by: Address,
}

#[contractevent]
pub struct RateBandSet {
    pub band_bp: u32,
}

/// A leader's own swap (22 Oct): USDC → nIN in her wallet (`to_nin`), or back.
#[contractevent]
pub struct LeaderSwap {
    #[topic]
    pub union: Address,
    #[topic]
    pub leader: Address,
    pub to_nin: bool,
    pub usdc: i128,
    pub nin: i128,
    pub inr_per_usd: i128,
}

#[contractevent]
pub struct TreasuryUsdcIn {
    #[topic]
    pub union: Address,
    #[topic]
    pub leader: Address,
    pub usdc: i128,
    pub nin: i128,
    pub inr_per_usd: i128,
}

#[contractevent]
pub struct Paused {
    pub paused: bool,
}

#[contract]
pub struct FxPoolContract;

// ---------- helpers ----------

fn get<V: soroban_sdk::TryFromVal<Env, soroban_sdk::Val>>(env: &Env, k: &DataKey) -> V {
    env.storage().instance().get(k).unwrap()
}

fn require_admin(env: &Env) -> Result<(), Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(())
}

fn core_addr(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::FundCore)
        .ok_or(Error::NotCore)
}

fn require_core(env: &Env) -> Result<Address, Error> {
    let core = core_addr(env)?;
    core.require_auth();
    Ok(core)
}

fn require_not_paused(env: &Env) -> Result<(), Error> {
    if env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        return Err(Error::Paused);
    }
    Ok(())
}

fn require_leader(env: &Env, union: &Address, leader: &Address) -> Result<(), Error> {
    leader.require_auth();
    if !RolesClient::new(env, &get(env, &DataKey::Roles)).is_leader(union, leader) {
        return Err(Error::NotLeader);
    }
    Ok(())
}

/// Only members hold nIN (C3): a title the union minted, or a leader of the union.
fn is_member_or_leader(env: &Env, union: &Address, who: &Address) -> bool {
    LandTitleClient::new(env, &get(env, &DataKey::LandTitle)).holds_title(union, who)
        || RolesClient::new(env, &get(env, &DataKey::Roles)).is_leader(union, who)
}

fn nin_addr(env: &Env) -> Address {
    get(env, &DataKey::Nin)
}

pub(crate) fn max_scan(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::MaxScan)
        .unwrap_or(DEFAULT_MAX_SCAN)
}

// Swaps, USDC into the treasury and the move mint stay capped at max_scan.
fn check_amount(env: &Env, amount: i128) -> Result<(), Error> {
    check_positive(amount)?;
    if amount > max_scan(env) {
        return Err(Error::CapExceeded);
    }
    Ok(())
}

// Cash scans and desk payouts are not capped (30 Oct, executive decision): the ₹10,000 is the crop plan's and the
// leaders' agreement for the first cycle, not code.
fn check_positive(amount: i128) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::AmountZero);
    }
    Ok(())
}

fn bump(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
}

fn cash_at_hand(env: &Env, union: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::CashAtHand(union.clone()))
        .unwrap_or(0)
}

fn set_cash_at_hand(env: &Env, union: &Address, v: i128) {
    let k = DataKey::CashAtHand(union.clone());
    env.storage().persistent().set(&k, &v);
    bump(env, &k);
}

/// Rupees came into the union's drawer: a 7-day escrow on its cash at hand (R15). The caller
/// checks the treasury cap and the open-escrow limit.
fn open_escrow(
    env: &Env,
    union: &Address,
    member: &Address,
    purpose: Purpose,
    amount: i128,
) -> u64 {
    let id = env
        .storage()
        .instance()
        .get(&DataKey::NextEscrow)
        .unwrap_or(0u64)
        + 1;
    env.storage().instance().set(&DataKey::NextEscrow, &id);
    let mut rows = open_escrows(env, union);
    rows.push_back(OpenEscrow {
        id,
        member: member.clone(),
        purpose,
        amount,
        remaining: amount,
        expires: env.ledger().timestamp() + ESCROW_TERM,
    });
    save_open_escrows(env, union, &rows);
    set_cash_at_hand(env, union, cash_at_hand(env, union) + amount);
    id
}

/// Cash at hand may not exceed fund_core's `cash_room` (31 Oct; was the treasury, R15), nor the open escrows the limit.
fn check_room(env: &Env, union: &Address, amount: i128) -> Result<(), Error> {
    if open_escrows(env, union).len() >= MAX_OPEN_ESCROWS {
        return Err(Error::TooManyEscrows);
    }
    if cash_at_hand(env, union) + amount
        > FundCoreClient::new(env, &core_addr(env)?).cash_room(union)
    {
        return Err(Error::AboveTreasury);
    }
    Ok(())
}

/// A union's open escrows, oldest first (one list since 24 Oct).
fn open_escrows(env: &Env, union: &Address) -> Vec<OpenEscrow> {
    env.storage()
        .persistent()
        .get(&DataKey::OpenEscrows(union.clone()))
        .unwrap_or(vec![env])
}

fn save_open_escrows(env: &Env, union: &Address, rows: &Vec<OpenEscrow>) {
    let k = DataKey::OpenEscrows(union.clone());
    env.storage().persistent().set(&k, rows);
    bump(env, &k);
}

/// Takes `amount` off the union's open escrows, oldest first; returns what it took.
fn settle(env: &Env, union: &Address, amount: i128) -> i128 {
    let mut open = vec![env];
    let mut left = amount;
    for mut e in open_escrows(env, union).iter() {
        if left > 0 {
            let take = core::cmp::min(left, e.remaining);
            e.remaining -= take;
            left -= take;
        }
        if e.remaining > 0 {
            open.push_back(e);
        }
    }
    let took = amount - left;
    if took > 0 {
        save_open_escrows(env, union, &open);
        let hand = cash_at_hand(env, union) - took;
        set_cash_at_hand(env, union, hand);
        CashSettled {
            union: union.clone(),
            amount: took,
            cash_at_hand: hand,
        }
        .publish(env);
    }
    took
}

/// Her own swap: not paused, a leader or title holder of the union (self mode, executive decision 4 Nov), her
/// identity checked, a fresh rate.
fn leader_swap_checks(
    env: &Env,
    union: &Address,
    leader: &Address,
    amount: i128,
) -> Result<RateFix, Error> {
    require_not_paused(env)?;
    leader.require_auth();
    if !is_member_or_leader(env, union, leader) {
        return Err(Error::NotMember);
    }
    if !env
        .storage()
        .persistent()
        .has(&DataKey::IdChecked(leader.clone()))
    {
        return Err(Error::NotIdChecked);
    }
    if amount <= 0 {
        return Err(Error::AmountZero);
    }
    let rate: RateFix = env
        .storage()
        .instance()
        .get(&DataKey::Rate)
        .ok_or(Error::StaleOracle)?;
    if env.ledger().timestamp() > rate.valid_until {
        return Err(Error::StaleOracle);
    }
    Ok(rate)
}

fn add_leader_swap(env: &Env, union: &Address, who: &Address, usdc: i128) {
    for k in [
        DataKey::LeaderSwapUsdc(union.clone()),
        DataKey::SwapUsdc(union.clone(), who.clone()),
    ] {
        let held: i128 = env.storage().persistent().get(&k).unwrap_or(0);
        env.storage().persistent().set(&k, &(held + usdc));
        bump(env, &k);
    }
}

/// R10: her trustline is authorised only for the transfer and deauthorised right after.
fn sandwich_transfer(env: &Env, from: &Address, to: &Address, member: &Address, amount: i128) {
    let nin = nin_addr(env);
    let sac = StellarAssetClient::new(env, &nin);
    sac.set_authorized(member, &true);
    TokenClient::new(env, &nin).transfer(from, to, &amount);
    sac.set_authorized(member, &false);
}

/// fx_pool signs the nIN transfer fund_core pulls from it inside the next call.
fn authorize_pull(env: &Env, core: &Address, amount: i128) {
    let me = env.current_contract_address();
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: nin_addr(env),
                fn_name: Symbol::new(env, "transfer"),
                args: (me, core.clone(), amount).into_val(env),
            },
            sub_invocations: vec![env],
        }),
    ]);
}

#[contractimpl]
impl FxPoolContract {
    pub fn __constructor(
        env: Env,
        admin: Address,
        roles: Address,
        land_title: Address,
        nin: Address,
        usdc: Address,
        daily_cap: i128,
    ) {
        let s = env.storage().instance();
        s.set(&DataKey::Admin, &admin);
        s.set(&DataKey::Roles, &roles);
        s.set(&DataKey::LandTitle, &land_title);
        s.set(&DataKey::Nin, &nin);
        s.set(&DataKey::Usdc, &usdc);
        s.set(&DataKey::DailyCap, &daily_cap);
        s.set(&DataKey::MaxScan, &DEFAULT_MAX_SCAN);
        upgrade::init(&env, UPGRADE_DELAY);
        bump_instance(&env);
    }

    // ---------- admin ----------

    /// Set once: after that only an upgrade (timelocked) can point fx_pool elsewhere.
    pub fn set_fund_core(env: Env, fund_core: Address) -> Result<(), Error> {
        require_admin(&env)?;
        if env.storage().instance().has(&DataKey::FundCore) {
            return Err(Error::CoreAlreadySet);
        }
        env.storage().instance().set(&DataKey::FundCore, &fund_core);
        FundCoreSet { fund_core }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// After the issuer's `set_admin(fx_pool)` on the nIN SAC: authorises the two contract
    /// balances that hold nIN, fx_pool's own and fund_core's. Nothing else.
    pub fn activate(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        let core = core_addr(&env)?;
        let sac = StellarAssetClient::new(&env, &nin_addr(&env));
        sac.set_authorized(&env.current_contract_address(), &true);
        sac.set_authorized(&core, &true);
        Activated { fund_core: core }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Per scan and per cash-out (R76); raised in steps, evented (C15, C19).
    pub fn set_max_scan(env: Env, max_scan: i128) -> Result<(), Error> {
        require_admin(&env)?;
        if max_scan <= 0 {
            return Err(Error::AmountZero);
        }
        env.storage().instance().set(&DataKey::MaxScan, &max_scan);
        MaxScanSet { max_scan }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// USDC out per day (R76): caps what Bring offers pay out.
    pub fn set_daily_cap(env: Env, cap: i128) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::DailyCap, &cap);
        DailyCapSet { daily_cap: cap }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn pause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        Paused { paused: true }.publish(&env);
        Ok(())
    }

    pub fn unpause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        Paused { paused: false }.publish(&env);
        Ok(())
    }

    // ---------- cash scan (leader) ----------

    /// The leader scanned rupees in (C6, L10, N11). `scan_hash` (the photo count's receipt) is
    /// only published in the CashScanned event, never stored (executive decision 24 Oct: the
    /// leader counting the notes is the trust signal; a stored receipt was rent for nothing).
    /// Invest, repay and recovery open a 7-day escrow on the union's cash at
    /// hand, which may not exceed its treasury (R15). A repay larger than what she owes
    /// mints only what is owed; the leader hands back the change. Returns the escrow id.
    /// Treasury is refused (22 Oct): counted cash never goes into the treasury.
    #[allow(clippy::too_many_arguments)]
    pub fn cash_scan_mint(
        env: Env,
        leader: Address,
        union: Address,
        purpose: Purpose,
        member: Address,
        amount: i128,
        scan_hash: BytesN<32>,
        token_id: Option<BytesN<32>>,
    ) -> Result<u64, Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &leader)?;
        check_positive(amount)?;
        let core = core_addr(&env)?;
        let fc = FundCoreClient::new(&env, &core);
        // Counted cash never goes into the treasury (executive decision 22 Oct); a Bring is opened
        // only by an LP's confirm.
        if purpose == Purpose::Treasury || purpose == Purpose::Bring {
            return Err(Error::Unauthorized);
        }
        // Cash at the office may pass the treasury up to fund_core's `cash_room` (executive decision 30 Oct, on-chain
        // 31 Oct): the treasury plus the cash members are due to take out again. Above it the scan is refused (below).
        if open_escrows(&env, &union).len() >= MAX_OPEN_ESCROWS {
            return Err(Error::TooManyEscrows);
        }

        let me = env.current_contract_address();
        let nin = nin_addr(&env);
        let token = TokenClient::new(&env, &nin);
        StellarAssetClient::new(&env, &nin).mint(&me, &amount);
        let mut taken = amount;
        // The signed pull covers only the very next call, so nothing may run in between.
        match purpose {
            Purpose::Invest => {
                authorize_pull(&env, &core, amount);
                fc.deposit_junior(&union, &member, &amount);
            }
            Purpose::Repay => {
                let id = token_id.ok_or(Error::NoLoan)?;
                // fund_core sends back whatever exceeds what she owes: burn it.
                let before = token.balance(&me);
                authorize_pull(&env, &core, amount);
                fc.repay_loan(&union, &id, &amount);
                let change = token.balance(&me) - (before - amount);
                if change > 0 {
                    token.burn(&me, &change);
                    taken -= change;
                }
            }
            Purpose::Recovery => {
                let id = token_id.ok_or(Error::NoLoan)?;
                authorize_pull(&env, &core, amount);
                fc.credit_recovery(&union, &id, &amount);
            }
            // Refused above.
            Purpose::Treasury | Purpose::Bring => return Err(Error::Unauthorized),
        }

        let mut escrow_id = 0u64;
        if taken > 0 {
            escrow_id = open_escrow(&env, &union, &member, purpose, taken);
            // After the deposit, so fund_core's room already holds what this scan changed there.
            if cash_at_hand(&env, &union) > fc.cash_room(&union) {
                return Err(Error::AboveTreasury);
            }
        }
        // No Collect offer from a scan any more (31 Oct): cash within the room goes back out to members.
        let collect_offer = 0u64;
        CashScanned {
            union,
            member,
            purpose,
            amount: taken,
            scan_hash,
            escrow_id,
            collect_offer,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(escrow_id)
    }

    /// Anyone, after expiry: the union kept rupees it neither lent nor settled, so the same
    /// amount of its treasury nIN is burned in fund_core (R15).
    pub fn burn_expired_escrow(env: Env, union: Address, escrow_id: u64) -> Result<i128, Error> {
        require_not_paused(&env)?;
        let mut rows = open_escrows(&env, &union);
        let i = rows
            .iter()
            .position(|r| r.id == escrow_id)
            .ok_or(Error::NotFound)? as u32;
        let e = rows.get(i).ok_or(Error::NotFound)?;
        if env.ledger().timestamp() < e.expires {
            return Err(Error::NotExpired);
        }
        rows.remove(i);
        save_open_escrows(&env, &union, &rows);
        set_cash_at_hand(&env, &union, cash_at_hand(&env, &union) - e.remaining);
        let covered =
            FundCoreClient::new(&env, &core_addr(&env)?).cover_expired_escrow(&union, &e.remaining);
        EscrowExpired {
            union,
            escrow_id,
            remaining: e.remaining,
            covered,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(covered)
    }

    // ---------- senior route (R24, R46, R77) ----------

    /// The attester posts the day's INR/USD fix (FBIL), valid at most 2 days (R77). Reflector
    /// replaces it later; this stays as the evented override.
    pub fn set_rate(
        env: Env,
        attester: Address,
        inr_per_usd: i128,
        valid_until: u64,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        attester.require_auth();
        if !RolesClient::new(&env, &get(&env, &DataKey::Roles)).has_role(&attester, &Role::Attester)
        {
            return Err(Error::Unauthorized);
        }
        let now = env.ledger().timestamp();
        // At most one attester fix per 12 hours (executive decision 2 Nov, review H4); the trust's
        // `override_rate` is not limited.
        let at: Option<u64> = env.storage().instance().get(&DataKey::AttesterFixAt);
        if at.is_some_and(|t| now < t + RATE_MIN_GAP) {
            return Err(Error::RateTooSoon);
        }
        if inr_per_usd <= 0 || valid_until <= now || valid_until > now + MAX_RATE_AGE {
            return Err(Error::BadRate);
        }
        // The attester only follows the market: within the band of the last fix (default 5%).
        // The first fix, or a bigger move, is the trust's (`override_rate`).
        let last: RateFix = env
            .storage()
            .instance()
            .get(&DataKey::Rate)
            .ok_or(Error::BadRate)?;
        let band_bp: u32 = env
            .storage()
            .instance()
            .get(&DataKey::RateBandBp)
            .unwrap_or(DEFAULT_RATE_BAND_BP);
        let band = last.inr_per_usd * i128::from(band_bp) / 10_000;
        if (inr_per_usd - last.inr_per_usd).abs() > band {
            return Err(Error::BadRate);
        }
        env.storage().instance().set(
            &DataKey::Rate,
            &RateFix {
                inr_per_usd,
                valid_until,
                set_by: attester.clone(),
            },
        );
        env.storage().instance().set(&DataKey::AttesterFixAt, &now);
        RateSet {
            inr_per_usd,
            valid_until,
            set_by: attester,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The attester's band (default 5%), evented; the trust adjusts it.
    pub fn set_rate_band(env: Env, band_bp: u32) -> Result<(), Error> {
        require_admin(&env)?;
        if band_bp > 10_000 {
            return Err(Error::BadRate);
        }
        env.storage().instance().set(&DataKey::RateBandBp, &band_bp);
        RateBandSet { band_bp }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The trust sets the first fix, or one beyond the attester's band (evented, R77).
    pub fn override_rate(env: Env, inr_per_usd: i128, valid_until: u64) -> Result<(), Error> {
        require_admin(&env)?;
        let now = env.ledger().timestamp();
        if inr_per_usd <= 0 || valid_until <= now || valid_until > now + MAX_RATE_AGE {
            return Err(Error::BadRate);
        }
        let admin: Address = get(&env, &DataKey::Admin);
        env.storage().instance().set(
            &DataKey::Rate,
            &RateFix {
                inr_per_usd,
                valid_until,
                set_by: admin.clone(),
            },
        );
        RateSet {
            inr_per_usd,
            valid_until,
            set_by: admin,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The senior contract (executive decision 2 Nov), set once by the trust: it holds all senior USDC.
    pub fn set_senior(env: Env, senior: Address) -> Result<(), Error> {
        require_admin(&env)?;
        if env.storage().instance().has(&DataKey::Senior) {
            return Err(Error::SeniorAlreadySet);
        }
        env.storage().instance().set(&DataKey::Senior, &senior);
        SeniorSet { senior }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The senior contract only (2 Nov): `nin` minted into the union's senior pool in `investor`'s name, for
    /// USDC the senior contract took in (or, for the vault, holds) at the fix. fx_pool holds no senior USDC.
    pub fn mint_senior_nin(
        env: Env,
        union: Address,
        investor: Address,
        nin: i128,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        let senior: Address = env
            .storage()
            .instance()
            .get(&DataKey::Senior)
            .ok_or(Error::Unauthorized)?;
        senior.require_auth();
        if nin <= 0 {
            return Err(Error::AmountZero);
        }
        let me = env.current_contract_address();
        StellarAssetClient::new(&env, &nin_addr(&env)).mint(&me, &nin);
        let core = core_addr(&env)?;
        authorize_pull(&env, &core, nin);
        FundCoreClient::new(&env, &core).deposit_senior(&union, &investor, &nin);
        SeniorNinMinted {
            union,
            investor,
            nin,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// A leader adds USDC to her union's treasury (executive decision 22 Oct): the USDC stays in
    /// fx_pool as the backing, and the same value in nIN at the attester's rate goes into the
    /// union treasury in fund_core. The only way, besides a leader's own nIN, that the treasury
    /// grows. Same cap per call as a scan. Returns the nIN minted.
    pub fn deposit_usdc_treasury(
        env: Env,
        leader: Address,
        union: Address,
        usdc: i128,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &leader)?;
        if usdc <= 0 {
            return Err(Error::AmountZero);
        }
        let rate: RateFix = env
            .storage()
            .instance()
            .get(&DataKey::Rate)
            .ok_or(Error::StaleOracle)?;
        if env.ledger().timestamp() > rate.valid_until {
            return Err(Error::StaleOracle);
        }
        let nin = usdc
            .checked_mul(rate.inr_per_usd)
            .map(|v| v / RATE_ONE)
            .ok_or(Error::CapExceeded)?;
        check_amount(&env, nin)?;
        let me = env.current_contract_address();
        let usdc_addr: Address = get(&env, &DataKey::Usdc);
        TokenClient::new(&env, &usdc_addr).transfer(&leader, &me, &usdc);
        StellarAssetClient::new(&env, &nin_addr(&env)).mint(&me, &nin);
        let core = core_addr(&env)?;
        authorize_pull(&env, &core, nin);
        FundCoreClient::new(&env, &core).deposit_union_treasury(&union, &nin);
        let k = DataKey::TreasuryUsdc(union.clone());
        let held: i128 = env.storage().persistent().get(&k).unwrap_or(0);
        env.storage().persistent().set(&k, &(held + usdc));
        bump(&env, &k);
        TreasuryUsdcIn {
            union,
            leader,
            usdc,
            nin,
            inr_per_usd: rate.inr_per_usd,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(nin)
    }

    /// A leader's own swap, USDC → nIN in her wallet (executive decision 22 Oct): a leader of the union whose identity
    /// the anchor checked; since 4 Nov also a title holder of the union in self mode, `leader` = her account. Her USDC stays in fx_pool, in the union's leader-swap bucket, as the backing of that nIN.
    pub fn swap_usdc_to_nin(
        env: Env,
        leader: Address,
        union: Address,
        usdc: i128,
    ) -> Result<i128, Error> {
        let rate = leader_swap_checks(&env, &union, &leader, usdc)?;
        let nin = usdc
            .checked_mul(rate.inr_per_usd)
            .map(|v| v / RATE_ONE)
            .ok_or(Error::CapExceeded)?;
        check_amount(&env, nin)?;
        let me = env.current_contract_address();
        TokenClient::new(&env, &get::<Address>(&env, &DataKey::Usdc)).transfer(&leader, &me, &usdc);
        StellarAssetClient::new(&env, &nin_addr(&env)).mint(&me, &nin);
        sandwich_transfer(&env, &me, &leader, &leader, nin);
        add_leader_swap(&env, &union, &leader, usdc);
        LeaderSwap {
            union,
            leader,
            to_nin: true,
            usdc,
            nin,
            inr_per_usd: rate.inr_per_usd,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(nin)
    }

    /// Back: her nIN is burned and USDC at the rate is paid only from the union's leader-swap bucket (never the LP,
    /// senior or treasury USDC), and never more than she swapped in herself and has not swapped back (review 5 Nov:
    /// owner income or other nIN goes out by the desk, `cash_out_signed`); refused (`NoUsdc`) when either is short.
    pub fn swap_nin_to_usdc(
        env: Env,
        leader: Address,
        union: Address,
        nin: i128,
    ) -> Result<i128, Error> {
        let rate = leader_swap_checks(&env, &union, &leader, nin)?;
        check_amount(&env, nin)?;
        let usdc = nin
            .checked_mul(RATE_ONE)
            .map(|v| v / rate.inr_per_usd)
            .ok_or(Error::CapExceeded)?;
        if usdc <= 0 {
            return Err(Error::AmountZero);
        }
        let k = DataKey::LeaderSwapUsdc(union.clone());
        let held: i128 = env.storage().persistent().get(&k).unwrap_or(0);
        let own_k = DataKey::SwapUsdc(union.clone(), leader.clone());
        let own: i128 = env.storage().persistent().get(&own_k).unwrap_or(0);
        if usdc > held || usdc > own {
            return Err(Error::NoUsdc);
        }
        let me = env.current_contract_address();
        sandwich_transfer(&env, &leader, &me, &leader, nin);
        TokenClient::new(&env, &nin_addr(&env)).burn(&me, &nin);
        TokenClient::new(&env, &get::<Address>(&env, &DataKey::Usdc)).transfer(&me, &leader, &usdc);
        env.storage().persistent().set(&k, &(held - usdc));
        bump(&env, &k);
        env.storage().persistent().set(&own_k, &(own - usdc));
        bump(&env, &own_k);
        LeaderSwap {
            union,
            leader,
            to_nin: false,
            usdc,
            nin,
            inr_per_usd: rate.inr_per_usd,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(usdc)
    }

    /// USDC in a union's leader-swap bucket (swap_usdc_to_nin less swap_nin_to_usdc).
    pub fn leader_swap_usdc(env: Env, union: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::LeaderSwapUsdc(union))
            .unwrap_or(0)
    }

    /// USDC an account swapped in and may still swap back (its cap on `swap_nin_to_usdc`).
    pub fn swap_usdc(env: Env, union: Address, account: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::SwapUsdc(union, account))
            .unwrap_or(0)
    }

    /// USDC behind a union's treasury nIN (deposit_usdc_treasury).
    pub fn treasury_usdc(env: Env, union: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::TreasuryUsdc(union))
            .unwrap_or(0)
    }

    pub fn senior(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Senior)
    }

    pub fn rate(env: Env) -> Option<RateFix> {
        env.storage().instance().get(&DataKey::Rate)
    }

    // ---------- calls from fund_core ----------

    /// The cash part of a draw left the union's cash at hand (D9, R35).
    pub fn settle_cash(env: Env, union: Address, amount: i128) -> Result<i128, Error> {
        require_core(&env)?;
        if amount <= 0 {
            return Err(Error::AmountZero);
        }
        Ok(settle(&env, &union, amount))
    }

    /// R10 around a fund_core payout (claim, treasury withdrawal). Authorising needs a
    /// title the union minted or a leader of the union (C3).
    pub fn set_member_auth(
        env: Env,
        union: Address,
        account: Address,
        authorized: bool,
    ) -> Result<(), Error> {
        require_core(&env)?;
        if authorized && !is_member_or_leader(&env, &union, &account) {
            return Err(Error::NotMember);
        }
        StellarAssetClient::new(&env, &nin_addr(&env)).set_authorized(&account, &authorized);
        Ok(())
    }

    // ---------- cash-out: the signed QR ----------

    /// The signed cash-out (executive decision 24 Oct; a machine owner's income): her QR carries her
    /// authorisation of (union, amount), used once by its Soroban nonce, expiring at its ledger (~48 h); any
    /// leader of her union scans it once the notes are handed over and submits. Her nIN is clawed back
    /// (fx_pool is the nIN admin) and the rupees leave the union's cash at hand. No escrow, no code.
    pub fn cash_out_signed(
        env: Env,
        leader: Address,
        union: Address,
        member: Address,
        amount: i128,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &leader)?;
        member.require_auth_for_args((union.clone(), amount).into_val(&env));
        check_positive(amount)?;
        if !is_member_or_leader(&env, &union, &member) {
            return Err(Error::NotMember);
        }
        StellarAssetClient::new(&env, &nin_addr(&env)).clawback(&member, &amount);
        settle(&env, &union, amount);
        CashOutSigned {
            union,
            member,
            leader,
            amount,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(amount)
    }

    // ---------- upgrade (admin, timelock, nila_common::upgrade) ----------

    /// Raising applies at once and voids a filed lowering; lowering applies only when set again
    /// after the current delay, within 7 days; at most 30 days.
    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        require_admin(&env)?;
        if !upgrade::set_delay(&env, delay_seconds) {
            return Err(Error::BadDelay);
        }
        bump_instance(&env);
        Ok(())
    }

    pub fn pending_upgrade(env: Env) -> Option<upgrade::PendingUpgrade> {
        upgrade::pending(&env)
    }

    /// A filed lowering of the delay, if any.
    pub fn pending_delay(env: Env) -> Option<upgrade::PendingDelay> {
        upgrade::pending_delay(&env)
    }

    /// Files the new wasm; `execute_upgrade` applies it once the delay has passed.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        require_admin(&env)?;
        upgrade::propose(&env, new_wasm_hash);
        bump_instance(&env);
        Ok(())
    }

    pub fn execute_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        if !upgrade::execute(&env) {
            return Err(Error::UpgradeNotReady);
        }
        Ok(())
    }

    pub fn cancel_upgrade(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        upgrade::cancel(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- views ----------

    pub fn cash_at_hand(env: Env, union: Address) -> i128 {
        cash_at_hand(&env, &union)
    }

    pub fn escrows_of(env: Env, union: Address) -> Vec<u64> {
        let mut ids = vec![&env];
        for r in open_escrows(&env, &union).iter() {
            ids.push_back(r.id);
        }
        ids
    }

    /// All of the union's open escrows in one read (24 Oct).
    pub fn open_escrows(env: Env, union: Address) -> Vec<OpenEscrow> {
        open_escrows(&env, &union)
    }

    pub fn get_escrow(env: Env, union: Address, escrow_id: u64) -> Option<OpenEscrow> {
        open_escrows(&env, &union)
            .iter()
            .find(|r| r.id == escrow_id)
    }

    pub fn max_scan(env: Env) -> i128 {
        max_scan(&env)
    }

    pub fn daily_cap(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::DailyCap)
            .unwrap_or(0)
    }

    pub fn fund_core(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::FundCore)
    }

    pub fn admin(env: Env) -> Address {
        get(&env, &DataKey::Admin)
    }

    pub fn upgrade_delay(env: Env) -> u64 {
        upgrade::delay(&env)
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }
}

mod lp;
pub use lp::{LpMember, LpPosition, LpQuote, Offer, OfferKind, OfferState, Quote, ACCEPT_PREFIX};

#[cfg(test)]
mod test;
