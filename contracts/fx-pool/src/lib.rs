#![no_std]
//! fx_pool, T1 scope (C3): the union's cash desk on-chain.
//!
//! - Cash scan: the leader records rupees a member brought (invest, repay, recovery) or the
//!   union's own cash (treasury); fx_pool mints the nIN and pays it into fund_core (R13, R14,
//!   R17). Member cash opens a 7-day escrow, the union's cash at hand (R15).
//! - Cash at hand never exceeds the union's treasury; a draw's cash part settles it (D9); an
//!   escrow left unsettled after 7 days is covered by burning treasury nIN.
//! - Cash-out (D13, R16): she escrows nIN under a one-time code; the leader's scan pays out
//!   and burns it; she reclaims it after expiry.
//! - nIN SAC admin (R8): member trustlines stay deauthorised and are authorised only inside
//!   the call that moves her nIN (R10), and only for a title holder or leader of the union.
//!
//! - Senior route (Nov 7): a senior investor the trust accepted deposits USDC, which stays
//!   on-chain as the guarantee behind vouchers; nIN is minted at the
//!   attester's INR/USD fix into fund_core's senior pool; every deposit is tagged offshore or
//!   domestic (R24, R46, R77). No USDC goes out yet: senior payouts are T2 (X15).
//!
//! LP offers, Reflector and the anchor (R18–R23) are T2.

use nila_common::{bump_instance, upgrade, FundCoreClient, LandTitleClient, Role, RolesClient};
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype,
    token::{StellarAssetClient, TokenClient},
    vec, Address, Bytes, BytesN, Env, IntoVal, Symbol, Vec,
};

contractmeta!(key = "binver", val = "0.3.0");
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
/// Ten seconds, not one: a cash-out must outlive a few ledgers between request and pay-out.
#[cfg(feature = "testnet-short-windows")]
pub const DAY: u64 = 10;

/// Cash at hand is lent out or settled within 7 days (R15).
pub const ESCROW_TERM: u64 = 7 * DAY;
/// A cash-out code lives at most 7 days.
pub const MAX_CASH_OUT_TERM: u64 = 7 * DAY;
/// ₹10,000 per scan and per cash-out at launch (R76), 7 decimals.
pub const DEFAULT_MAX_SCAN: i128 = 10_000 * 10_000_000;
/// Open escrows per union: the leader settles before scanning more.
pub const MAX_OPEN_ESCROWS: u32 = 64;
/// One unit of the INR/USD fix: 7 decimals, like nIN and USDC.
pub const RATE_ONE: i128 = 10_000_000;
/// The attester's fix is valid at most 2 days (R77).
pub const MAX_RATE_AGE: u64 = 2 * DAY;
/// The LP's fee: 1% (R21).
pub const DEFAULT_LP_FEE_BP: u32 = 100;
/// Upgrade timelock at deploy: 48 hours (R75, C15); short builds rehearse it in seconds.
pub const UPGRADE_DELAY: u64 = 2 * DAY;

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
    ScanUsed = 12,
    TooManyEscrows = 13,
    NotExpired = 14,
    Expired = 15,
    CodeUsed = 16,
    BadExpiry = 17,
    /// A repay or recovery scan names no loan.
    NoLoan = 18,
    CoreAlreadySet = 19,
    UpgradeNotReady = 20,
    /// Above the 30-day maximum.
    BadDelay = 21,
    /// The INR/USD fix is zero, negative or valid too long.
    BadRate = 22,
    /// Senior USDC may not fall below the union's voucher floor.
    BelowVoucherFloor = 23,
    NotAnchor = 24,
    NotLp = 25,
    /// The offer is not in the state this call needs.
    OfferState = 26,
    TooManyOffers = 27,
    /// The union's USDC (LP-collected plus senior above the floor) does not cover it.
    NoUsdc = 28,
}

/// Where a senior investor's USDC comes from (R24, D1): kept per deposit so the pools can be split later.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Tag {
    Offshore = 0,
    Domestic = 1,
}

/// The trust's acceptance of a senior investor: global or domestic, never a member. `kyc_ref`
/// points at the KYC record kept off-chain; no personal data on-chain (C17). Not an LP: an LP
/// is a member who swaps cash for USDC for a fee and never lends (executive decision, 12 Oct; T2).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeniorInvestor {
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
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
    /// The union's own cash into its treasury (R17); no escrow.
    Treasury = 3,
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
    Escrow(u64),
    /// Open escrow ids of a union, oldest first.
    EscrowsOf(Address),
    CashAtHand(Address),
    Scan(BytesN<32>),
    CashOut(BytesN<32>),
    /// A code paid out or reclaimed: never accepted again (its preimage is public).
    CodeUsed(BytesN<32>),
    SeniorInvestor(Address),
    Rate,
    /// USDC taken in per union and tag (R24).
    SeniorUsdc(Address, Tag),
    /// Senior USDC kept back per union as the voucher guarantee (executive decision, 12 Oct).
    VoucherFloor(Address),
    /// The trust's anchor account: it accepts LPs after SEP-12 (R20).
    Anchor,
    /// A member LP of a union (R20).
    LpOf(Address, Address),
    /// USDC LPs brought into the union through Collect offers (domestic).
    LpUsdc(Address),
    LpFeeBp,
    NextOffer,
    Offer(u64),
    /// Open offer ids of a union.
    OffersOf(Address),
}

/// Rupees a leader took in, not yet lent out or settled (R15).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub union: Address,
    pub member: Address,
    pub purpose: Purpose,
    pub amount: i128,
    pub remaining: i128,
    pub expires: u64,
}

/// Her nIN held under a one-time code until the leader pays out or she reclaims (D13).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CashOut {
    pub member: Address,
    pub union: Address,
    pub amount: i128,
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

#[contractevent]
pub struct CashOutRequested {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub code_hash: BytesN<32>,
    pub amount: i128,
    pub expires: u64,
}

#[contractevent]
pub struct CashOutPaid {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub code_hash: BytesN<32>,
    pub leader: Address,
    pub amount: i128,
}

#[contractevent]
pub struct CashOutReclaimed {
    #[topic]
    pub union: Address,
    #[topic]
    pub member: Address,
    pub code_hash: BytesN<32>,
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

#[contractevent]
pub struct SeniorInvestorSet {
    #[topic]
    pub investor: Address,
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
}

#[contractevent]
pub struct RateSet {
    pub inr_per_usd: i128,
    pub valid_until: u64,
    pub set_by: Address,
}

#[contractevent]
pub struct SeniorMinted {
    #[topic]
    pub union: Address,
    #[topic]
    pub investor: Address,
    pub tag: Tag,
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

fn max_scan(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::MaxScan)
        .unwrap_or(DEFAULT_MAX_SCAN)
}

fn check_amount(env: &Env, amount: i128) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::AmountZero);
    }
    if amount > max_scan(env) {
        return Err(Error::CapExceeded);
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

fn escrows_of(env: &Env, union: &Address) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::EscrowsOf(union.clone()))
        .unwrap_or(vec![env])
}

fn save_escrows_of(env: &Env, union: &Address, ids: &Vec<u64>) {
    let k = DataKey::EscrowsOf(union.clone());
    env.storage().persistent().set(&k, ids);
    bump(env, &k);
}

/// Takes `amount` off the union's open escrows, oldest first; returns what it took.
fn settle(env: &Env, union: &Address, amount: i128) -> i128 {
    let ids = escrows_of(env, union);
    let mut open = vec![env];
    let mut left = amount;
    for id in ids.iter() {
        let k = DataKey::Escrow(id);
        if left > 0 {
            if let Some(mut e) = env.storage().persistent().get::<_, Escrow>(&k) {
                let take = core::cmp::min(left, e.remaining);
                e.remaining -= take;
                left -= take;
                if e.remaining == 0 {
                    env.storage().persistent().remove(&k);
                    continue;
                }
                env.storage().persistent().set(&k, &e);
                bump(env, &k);
            }
        }
        open.push_back(id);
    }
    let took = amount - left;
    if took > 0 {
        save_escrows_of(env, union, &open);
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

fn close_code(env: &Env, code_hash: &BytesN<32>) {
    let k = DataKey::CodeUsed(code_hash.clone());
    env.storage().persistent().set(&k, &true);
    bump(env, &k);
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

    /// USDC out per day (R76); used once USDC moves (T2).
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

    /// The leader scanned rupees in (C6, L10, N11). `scan_hash` is the receipt's hash and
    /// is used once. Invest, repay and recovery open a 7-day escrow on the union's cash at
    /// hand, which may not exceed its treasury (R15). A repay larger than what she owes
    /// mints only what is owed; the leader hands back the change. Returns the escrow id
    /// (0 for a treasury deposit).
    #[allow(clippy::too_many_arguments)]
    pub fn cash_scan_mint(
        env: Env,
        leader: Address,
        union: Address,
        purpose: Purpose,
        member: Address,
        amount: i128,
        scan_hash: BytesN<32>,
        loan_id: Option<BytesN<32>>,
    ) -> Result<u64, Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &leader)?;
        check_amount(&env, amount)?;
        let sk = DataKey::Scan(scan_hash.clone());
        if env.storage().persistent().has(&sk) {
            return Err(Error::ScanUsed);
        }
        env.storage().persistent().set(&sk, &true);
        bump(&env, &sk);

        let core = core_addr(&env)?;
        let fc = FundCoreClient::new(&env, &core);
        let escrowed = purpose != Purpose::Treasury;
        let mut ids = escrows_of(&env, &union);
        if escrowed {
            if ids.len() >= MAX_OPEN_ESCROWS {
                return Err(Error::TooManyEscrows);
            }
            if cash_at_hand(&env, &union) + amount > fc.union_treasury(&union) {
                return Err(Error::AboveTreasury);
            }
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
            Purpose::Treasury => {
                authorize_pull(&env, &core, amount);
                fc.deposit_union_treasury(&union, &amount);
            }
            Purpose::Repay => {
                let id = loan_id.ok_or(Error::NoLoan)?;
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
                let id = loan_id.ok_or(Error::NoLoan)?;
                authorize_pull(&env, &core, amount);
                fc.credit_recovery(&union, &id, &amount);
            }
        }

        let mut escrow_id = 0u64;
        if escrowed && taken > 0 {
            escrow_id = env
                .storage()
                .instance()
                .get(&DataKey::NextEscrow)
                .unwrap_or(0u64)
                + 1;
            env.storage()
                .instance()
                .set(&DataKey::NextEscrow, &escrow_id);
            let k = DataKey::Escrow(escrow_id);
            env.storage().persistent().set(
                &k,
                &Escrow {
                    union: union.clone(),
                    member: member.clone(),
                    purpose,
                    amount: taken,
                    remaining: taken,
                    expires: env.ledger().timestamp() + ESCROW_TERM,
                },
            );
            bump(&env, &k);
            ids.push_back(escrow_id);
            save_escrows_of(&env, &union, &ids);
            set_cash_at_hand(&env, &union, cash_at_hand(&env, &union) + taken);
        }
        CashScanned {
            union,
            member,
            purpose,
            amount: taken,
            scan_hash,
            escrow_id,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(escrow_id)
    }

    /// Anyone, after expiry: the union kept rupees it neither lent nor settled, so the same
    /// amount of its treasury nIN is burned in fund_core (R15).
    pub fn burn_expired_escrow(env: Env, escrow_id: u64) -> Result<i128, Error> {
        require_not_paused(&env)?;
        let k = DataKey::Escrow(escrow_id);
        let e: Escrow = env.storage().persistent().get(&k).ok_or(Error::NotFound)?;
        if env.ledger().timestamp() < e.expires {
            return Err(Error::NotExpired);
        }
        env.storage().persistent().remove(&k);
        let mut ids = escrows_of(&env, &e.union);
        if let Some(i) = ids.first_index_of(escrow_id) {
            ids.remove(i);
        }
        save_escrows_of(&env, &e.union, &ids);
        set_cash_at_hand(&env, &e.union, cash_at_hand(&env, &e.union) - e.remaining);
        let covered = FundCoreClient::new(&env, &core_addr(&env)?)
            .cover_expired_escrow(&e.union, &e.remaining);
        EscrowExpired {
            union: e.union,
            escrow_id,
            remaining: e.remaining,
            covered,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(covered)
    }

    // ---------- senior route (R24, R46, R77) ----------

    /// The trust accepts or drops a senior investor (KYC kept off-chain).
    pub fn set_senior_investor(
        env: Env,
        investor: Address,
        accepted: bool,
        kyc_ref: BytesN<32>,
    ) -> Result<(), Error> {
        require_admin(&env)?;
        let k = DataKey::SeniorInvestor(investor.clone());
        env.storage().persistent().set(
            &k,
            &SeniorInvestor {
                accepted,
                kyc_ref: kyc_ref.clone(),
            },
        );
        bump(&env, &k);
        SeniorInvestorSet {
            investor,
            accepted,
            kyc_ref,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The attester posts the day's INR/USD fix (FBIL), valid at most 2 days (R77). Reflector
    /// replaces it later; this stays as the evented override.
    pub fn set_rate(
        env: Env,
        attester: Address,
        inr_per_usd: i128,
        valid_until: u64,
    ) -> Result<(), Error> {
        attester.require_auth();
        if !RolesClient::new(&env, &get(&env, &DataKey::Roles)).has_role(&attester, &Role::Attester)
        {
            return Err(Error::Unauthorized);
        }
        let now = env.ledger().timestamp();
        if inr_per_usd <= 0 || valid_until <= now || valid_until > now + MAX_RATE_AGE {
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
        RateSet {
            inr_per_usd,
            valid_until,
            set_by: attester,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// An accepted senior investor deposits USDC; it stays in fx_pool as the guarantee behind
    /// vouchers, and the same value in nIN goes into the union's senior pool in their name (R46).
    /// Members are refused: fund_core rejects title holders. Returns the nIN minted.
    pub fn mint_senior(
        env: Env,
        investor: Address,
        union: Address,
        usdc: i128,
        tag: Tag,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        if usdc <= 0 {
            return Err(Error::AmountZero);
        }
        let ok = env
            .storage()
            .persistent()
            .get::<_, SeniorInvestor>(&DataKey::SeniorInvestor(investor.clone()))
            .map(|l| l.accepted)
            .unwrap_or(false);
        if !ok {
            return Err(Error::Unauthorized);
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
        if nin <= 0 {
            return Err(Error::AmountZero);
        }
        let me = env.current_contract_address();
        let usdc_addr: Address = get(&env, &DataKey::Usdc);
        TokenClient::new(&env, &usdc_addr).transfer(&investor, &me, &usdc);
        StellarAssetClient::new(&env, &nin_addr(&env)).mint(&me, &nin);
        let core = core_addr(&env)?;
        authorize_pull(&env, &core, nin);
        FundCoreClient::new(&env, &core).deposit_senior(&union, &investor, &nin);
        let k = DataKey::SeniorUsdc(union.clone(), tag);
        // The guarantee is held, never spent: only what is above the floor can pay a Bring.
        let held: i128 = env.storage().persistent().get(&k).unwrap_or(0);
        env.storage().persistent().set(&k, &(held + usdc));
        bump(&env, &k);
        SeniorMinted {
            union,
            investor,
            tag,
            usdc,
            nin,
            inr_per_usd: rate.inr_per_usd,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(nin)
    }

    pub fn senior_investor(env: Env, investor: Address) -> Option<SeniorInvestor> {
        env.storage()
            .persistent()
            .get(&DataKey::SeniorInvestor(investor))
    }

    pub fn rate(env: Env) -> Option<RateFix> {
        env.storage().instance().get(&DataKey::Rate)
    }

    /// USDC taken in for a union under one tag (R24).
    pub fn senior_usdc(env: Env, union: Address, tag: Tag) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::SeniorUsdc(union, tag))
            .unwrap_or(0)
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

    // ---------- member cash-out (D13, R16) ----------

    /// A7: her nIN moves into fx_pool under `code_hash` = sha256 of the one-time code she
    /// shows the leader. Expires at most 7 days out.
    pub fn request_cash_out(
        env: Env,
        member: Address,
        union: Address,
        amount: i128,
        code_hash: BytesN<32>,
        expires: u64,
    ) -> Result<(), Error> {
        require_not_paused(&env)?;
        member.require_auth();
        check_amount(&env, amount)?;
        let now = env.ledger().timestamp();
        if expires <= now || expires > now + MAX_CASH_OUT_TERM {
            return Err(Error::BadExpiry);
        }
        if !is_member_or_leader(&env, &union, &member) {
            return Err(Error::NotMember);
        }
        let k = DataKey::CashOut(code_hash.clone());
        if env.storage().persistent().has(&k)
            || env
                .storage()
                .persistent()
                .has(&DataKey::CodeUsed(code_hash.clone()))
        {
            return Err(Error::CodeUsed);
        }
        sandwich_transfer(
            &env,
            &member,
            &env.current_contract_address(),
            &member,
            amount,
        );
        env.storage().persistent().set(
            &k,
            &CashOut {
                member: member.clone(),
                union: union.clone(),
                amount,
                expires,
            },
        );
        bump(&env, &k);
        CashOutRequested {
            union,
            member,
            code_hash,
            amount,
            expires,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// A1b / C6: the leader scanned her code and counted the rupees out. The nIN burns and
    /// the cash leaves the union's cash at hand.
    pub fn pay_out(env: Env, leader: Address, code: Bytes) -> Result<i128, Error> {
        require_not_paused(&env)?;
        let code_hash: BytesN<32> = env.crypto().sha256(&code).into();
        let k = DataKey::CashOut(code_hash.clone());
        let co: CashOut = env.storage().persistent().get(&k).ok_or(Error::NotFound)?;
        require_leader(&env, &co.union, &leader)?;
        if env.ledger().timestamp() >= co.expires {
            return Err(Error::Expired);
        }
        env.storage().persistent().remove(&k);
        close_code(&env, &code_hash);
        TokenClient::new(&env, &nin_addr(&env)).burn(&env.current_contract_address(), &co.amount);
        settle(&env, &co.union, co.amount);
        CashOutPaid {
            union: co.union,
            member: co.member,
            code_hash,
            leader,
            amount: co.amount,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(co.amount)
    }

    /// Anyone, after expiry: an uncollected cash-out goes back to her wallet.
    pub fn reclaim_cash_out(env: Env, code_hash: BytesN<32>) -> Result<i128, Error> {
        let k = DataKey::CashOut(code_hash.clone());
        let co: CashOut = env.storage().persistent().get(&k).ok_or(Error::NotFound)?;
        if env.ledger().timestamp() < co.expires {
            return Err(Error::NotExpired);
        }
        env.storage().persistent().remove(&k);
        close_code(&env, &code_hash);
        // Her own nIN back (C12): the one authorisation without a fresh membership check.
        sandwich_transfer(
            &env,
            &env.current_contract_address(),
            &co.member,
            &co.member,
            co.amount,
        );
        CashOutReclaimed {
            union: co.union,
            member: co.member,
            code_hash,
            amount: co.amount,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(co.amount)
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
        escrows_of(&env, &union)
    }

    pub fn get_escrow(env: Env, escrow_id: u64) -> Option<Escrow> {
        env.storage().persistent().get(&DataKey::Escrow(escrow_id))
    }

    pub fn cash_out(env: Env, code_hash: BytesN<32>) -> Option<CashOut> {
        env.storage().persistent().get(&DataKey::CashOut(code_hash))
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
pub use lp::{LpMember, Offer, OfferKind, OfferState};

#[cfg(test)]
mod test;
