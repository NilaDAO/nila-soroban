#![no_std]
//! machinery: machinery vouchers on Stellar (C5, Nov 7; executive decision 11 Oct, N2).
//!
//! - **Rate table** per union and machine type, in parts per hour: driver, diesel, maintenance,
//!   insurance, depreciation, margin (D16, D20, R57). Set by the union account (its leaders,
//!   3 of 5); open and adjustable, like the prepay share (executive decision, 12 Oct).
//! - **Machines** (D15, R82, R83): a leader registers a member's machine (L13); the owner
//!   confirms (N9) and can put it on hold with a reason.
//! - **Vouchers** (R51–R55): a loan's voucher part is machinery in **tractor hours** (executive
//!   decision 23 Oct), held in hundredths (`units`, centi tractor hours): one tractor hour is the
//!   rate of machine type 1 (tractor + rotavator); an hour of another machine uses its rate ÷ the
//!   tractor rate (3 tiller hours at ₹500 against ₹1,100 use 137 units). Granted by fund_core at
//!   the draw. Non-transferable by construction (C5). She signs a one-time authorisation for
//!   `claim` up to X units (A8a QR; Soroban's nonce and expiry ledger, ~48 h; executive decision
//!   23 Oct); the owner scans it and claims the whole hours worked (D19), within the machine's
//!   hours a week (R84).
//! - **Pay** (D18, R85): at the claim fund_core pays the owner the prepay (diesel plus a share of
//!   the driver part, X32) from senior money; the rest is pending until she repays.
//!
//! Money never sits here: fund_core moves it. This contract keeps the facts.

use nila_common::{bump_instance, upgrade, FundCoreClient, LandTitleClient, RolesClient};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, vec, Address,
    BytesN, Env, IntoVal, Vec,
};

contractmeta!(key = "binver", val = "0.1.0");

// Demo builds only (feature `demo-upgrade-delay`): the upgrade timelock is 60 seconds, so a demo deployment can be
// upgraded in place and keep its state. Never for mainnet: the wasm carries this marker, which deploys refuse.
#[cfg(feature = "demo-upgrade-delay")]
soroban_sdk::contractmeta!(
    key = "demo_upgrade_delay",
    val = "demo-upgrade-delay: 60 s, never mainnet"
);
contractmeta!(
    key = "desc",
    val = "Nila machinery — rate table, machines, vouchers, owner claims"
);
#[cfg(feature = "testnet-short-windows")]
contractmeta!(
    key = "windows",
    val = "testnet-short: one day is ten seconds"
);

#[cfg(not(feature = "testnet-short-windows"))]
pub const DAY: u64 = 86_400;
#[cfg(feature = "testnet-short-windows")]
pub const DAY: u64 = 10;

pub const WEEK: u64 = 7 * DAY;
/// How long a week's hour count lives, in ledgers (~5 s each): eight days covers any claim in
/// that week, whatever day it was first written.
pub const WEEK_USE_LEDGERS: u32 = 8 * nila_common::DAY_IN_LEDGERS;
/// Machine types per union in the rate table.
pub const MAX_TYPES: u32 = 16;
/// Default share of the driver part paid at the job (X32 open; the union changes it).
pub const DEFAULT_PREPAY_DRIVER_BP: u32 = 5_000;
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
    NotLeader = 4,
    NotMember = 5,
    NotOwner = 6,
    NoRate = 7,
    BadArg = 8,
    TooManyTypes = 9,
    /// Not confirmed by the owner, or on hold.
    Unavailable = 10,
    /// More hours than her authorisation, the voucher or the machine's week allows.
    TooManyHours = 11,
    Paused = 15,
    VoucherExists = 16,
    UpgradeNotReady = 17,
    BadDelay = 18,
    CoreAlreadySet = 19,
    /// An owner cannot claim on her own voucher.
    SelfClaim = 20,
}

/// One machine hour, in nIN (7 decimals), by part (D16, D20).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateParts {
    pub driver: i128,
    pub diesel: i128,
    pub maintenance: i128,
    pub insurance: i128,
    pub depreciation: i128,
    pub margin: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Machine {
    pub union: Address,
    pub owner: Address,
    pub machine_type: u32,
    pub hours_per_week: u32,
    /// Hash of the 8 photos at 45° (L13); the photos stay off-chain.
    pub photos_hash: BytesN<32>,
    pub confirmed: bool,
    pub on_hold: bool,
    /// 0 none, 1 in repair, 2 driver not available, 3 other (R83).
    pub hold_reason: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Voucher {
    pub member: Address,
    pub hours: u32,
    pub used: u32,
    /// Lapsed when the loan closed or defaulted.
    pub void: bool,
    /// Unused since the signed claim (23 Oct); kept so stored vouchers keep their layout.
    pub code_hash: Option<BytesN<32>>,
    pub code_max: u32,
    pub code_expires: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Roles,
    LandTitle,
    FundCore,
    Paused,
    Rate(Address, u32),
    Types(Address),
    PrepayDriverBp(Address),
    NextMachine,
    Machine(u64),
    Voucher(Address, BytesN<32>),
    /// Hours a machine worked in a week (machine id, week index). **Temporary** storage (C7):
    /// deleted by the network once the week is over.
    WeekUse(u64, u64),
}

#[contractevent]
pub struct RateSet {
    #[topic]
    pub union: Address,
    #[topic]
    pub machine_type: u32,
    pub parts: RateParts,
}

#[contractevent]
pub struct PrepayShareSet {
    #[topic]
    pub union: Address,
    pub driver_bp: u32,
}

#[contractevent]
pub struct MachineRegistered {
    #[topic]
    pub union: Address,
    #[topic]
    pub machine_id: u64,
    pub owner: Address,
    pub machine_type: u32,
    pub hours_per_week: u32,
}

#[contractevent]
pub struct MachineUpdated {
    #[topic]
    pub machine_id: u64,
    pub confirmed: bool,
    pub on_hold: bool,
    pub hold_reason: u32,
}

#[contractevent]
pub struct VoucherGranted {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub member: Address,
    pub hours: u32,
}

#[contractevent]
pub struct VoucherClaimed {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub machine_id: u64,
    pub owner: Address,
    /// Hours worked on the machine, in hundredths of an hour.
    pub hours: u32,
    /// Centi tractor hours they used of her voucher.
    pub units: u32,
    pub prepay: i128,
    pub pending: i128,
}

#[contractevent]
pub struct VoucherVoided {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub unused_hours: u32,
}

#[contractevent]
pub struct Paused {
    pub paused: bool,
}

#[contract]
pub struct MachineryContract;

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

fn require_core(env: &Env) -> Result<(), Error> {
    core_addr(env)?.require_auth();
    Ok(())
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

fn holds_title(env: &Env, union: &Address, who: &Address) -> bool {
    LandTitleClient::new(env, &get(env, &DataKey::LandTitle)).holds_title(union, who)
}

fn bump(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
}

fn prepay_driver_bp(env: &Env, union: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::PrepayDriverBp(union.clone()))
        .unwrap_or(DEFAULT_PREPAY_DRIVER_BP)
}

fn rate(env: &Env, union: &Address, t: u32) -> Option<RateParts> {
    env.storage()
        .persistent()
        .get(&DataKey::Rate(union.clone(), t))
}

/// Paid at the job, per hour: diesel plus the union's share of the driver part (D18).
fn prepay_per_hour(r: &RateParts, driver_bp: u32) -> i128 {
    r.diesel + r.driver * i128::from(driver_bp) / 10_000
}

fn total_per_hour(r: &RateParts) -> i128 {
    r.driver + r.diesel + r.maintenance + r.insurance + r.depreciation + r.margin
}

/// The machine type whose hour is the voucher's unit (executive decision 23 Oct).
pub const TRACTOR_TYPE: u32 = 1;

/// Centi tractor hours that `centi` hundredths of an hour on a machine at rate `r` use, rounded up.
fn units_for(r: &RateParts, tractor: &RateParts, centi: u32) -> Result<u32, Error> {
    let t = total_per_hour(tractor);
    if t <= 0 {
        return Err(Error::NoRate);
    }
    let n = total_per_hour(r)
        .checked_mul(i128::from(centi))
        .ok_or(Error::BadArg)?;
    u32::try_from((n + t - 1) / t).map_err(|_| Error::BadArg)
}

fn load_machine(env: &Env, id: u64) -> Result<Machine, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Machine(id))
        .ok_or(Error::NotFound)
}

fn save_machine(env: &Env, id: u64, m: &Machine) {
    let k = DataKey::Machine(id);
    env.storage().persistent().set(&k, m);
    bump(env, &k);
    MachineUpdated {
        machine_id: id,
        confirmed: m.confirmed,
        on_hold: m.on_hold,
        hold_reason: m.hold_reason,
    }
    .publish(env);
}

fn load_voucher(env: &Env, union: &Address, token_id: &BytesN<32>) -> Result<Voucher, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Voucher(union.clone(), token_id.clone()))
        .ok_or(Error::NotFound)
}

fn save_voucher(env: &Env, union: &Address, token_id: &BytesN<32>, v: &Voucher) {
    let k = DataKey::Voucher(union.clone(), token_id.clone());
    env.storage().persistent().set(&k, v);
    bump(env, &k);
}

#[contractimpl]
impl MachineryContract {
    pub fn __constructor(env: Env, admin: Address, roles: Address, land_title: Address) {
        let s = env.storage().instance();
        s.set(&DataKey::Admin, &admin);
        s.set(&DataKey::Roles, &roles);
        s.set(&DataKey::LandTitle, &land_title);
        upgrade::init(&env, UPGRADE_DELAY);
        bump_instance(&env);
    }

    /// Set once; only a timelocked upgrade can repoint it.
    pub fn set_fund_core(env: Env, fund_core: Address) -> Result<(), Error> {
        require_admin(&env)?;
        if env.storage().instance().has(&DataKey::FundCore) {
            return Err(Error::CoreAlreadySet);
        }
        env.storage().instance().set(&DataKey::FundCore, &fund_core);
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

    // ---------- the union's rate table (R57, D16, D20) ----------

    /// The union account (its leaders, 3 of 5) sets the price of a machine hour in parts. The
    /// amounts are open and stay adjustable by the union (executive decision, 12 Oct), like the prepay share.
    pub fn set_rate(
        env: Env,
        union: Address,
        machine_type: u32,
        parts: RateParts,
    ) -> Result<(), Error> {
        union.require_auth();
        if parts.driver < 0
            || parts.diesel < 0
            || parts.maintenance < 0
            || parts.insurance < 0
            || parts.depreciation < 0
            || parts.margin < 0
            || total_per_hour(&parts) <= 0
        {
            return Err(Error::BadArg);
        }
        let tk = DataKey::Types(union.clone());
        let mut types: Vec<u32> = env.storage().persistent().get(&tk).unwrap_or(vec![&env]);
        if !types.contains(machine_type) {
            if types.len() >= MAX_TYPES {
                return Err(Error::TooManyTypes);
            }
            types.push_back(machine_type);
            env.storage().persistent().set(&tk, &types);
        }
        bump(&env, &tk);
        let k = DataKey::Rate(union.clone(), machine_type);
        env.storage().persistent().set(&k, &parts);
        bump(&env, &k);
        RateSet {
            union,
            machine_type,
            parts,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// X32: the share of the driver part paid at the job.
    pub fn set_prepay_share(env: Env, union: Address, driver_bp: u32) -> Result<(), Error> {
        union.require_auth();
        if driver_bp > 10_000 {
            return Err(Error::BadArg);
        }
        let k = DataKey::PrepayDriverBp(union.clone());
        env.storage().persistent().set(&k, &driver_bp);
        bump(&env, &k);
        PrepayShareSet { union, driver_bp }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- machines (D15, R82, R83) ----------

    /// L13: a leader registers a member's machine; the owner confirms it on her phone.
    pub fn register_machine(
        env: Env,
        leader: Address,
        union: Address,
        owner: Address,
        machine_type: u32,
        hours_per_week: u32,
        photos_hash: BytesN<32>,
    ) -> Result<u64, Error> {
        require_not_paused(&env)?;
        leader.require_auth();
        if !RolesClient::new(&env, &get(&env, &DataKey::Roles)).is_leader(&union, &leader) {
            return Err(Error::NotLeader);
        }
        if !holds_title(&env, &union, &owner) {
            return Err(Error::NotMember);
        }
        if rate(&env, &union, machine_type).is_none() {
            return Err(Error::NoRate);
        }
        if hours_per_week == 0 || hours_per_week > 7 * 24 {
            return Err(Error::BadArg);
        }
        let id = env
            .storage()
            .instance()
            .get(&DataKey::NextMachine)
            .unwrap_or(0u64)
            + 1;
        env.storage().instance().set(&DataKey::NextMachine, &id);
        save_machine(
            &env,
            id,
            &Machine {
                union: union.clone(),
                owner: owner.clone(),
                machine_type,
                hours_per_week,
                photos_hash,
                confirmed: false,
                on_hold: false,
                hold_reason: 0,
            },
        );
        MachineRegistered {
            union,
            machine_id: id,
            owner,
            machine_type,
            hours_per_week,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(id)
    }

    /// N9: the owner confirms her machine.
    pub fn confirm_machine(env: Env, owner: Address, machine_id: u64) -> Result<(), Error> {
        owner.require_auth();
        let mut m = load_machine(&env, machine_id)?;
        if m.owner != owner {
            return Err(Error::NotOwner);
        }
        m.confirmed = true;
        save_machine(&env, machine_id, &m);
        Ok(())
    }

    /// R83: on hold with a reason (1 in repair, 2 driver not available, 3 other), and back.
    pub fn set_hold(
        env: Env,
        owner: Address,
        machine_id: u64,
        on_hold: bool,
        reason: u32,
    ) -> Result<(), Error> {
        owner.require_auth();
        let mut m = load_machine(&env, machine_id)?;
        if m.owner != owner {
            return Err(Error::NotOwner);
        }
        if on_hold && !(1..=3).contains(&reason) {
            return Err(Error::BadArg);
        }
        m.on_hold = on_hold;
        m.hold_reason = if on_hold { reason } else { 0 };
        save_machine(&env, machine_id, &m);
        Ok(())
    }

    // ---------- vouchers ----------

    /// fund_core at the draw: her loan's voucher part.
    pub fn grant(
        env: Env,
        union: Address,
        member: Address,
        token_id: BytesN<32>,
        hours: u32,
    ) -> Result<(), Error> {
        require_core(&env)?;
        let k = DataKey::Voucher(union.clone(), token_id.clone());
        if env.storage().persistent().has(&k) {
            return Err(Error::VoucherExists);
        }
        save_voucher(
            &env,
            &union,
            &token_id,
            &Voucher {
                member: member.clone(),
                hours,
                used: 0,
                void: false,
                code_hash: None,
                code_max: 0,
                code_expires: 0,
            },
        );
        VoucherGranted {
            union,
            token_id,
            member,
            hours,
        }
        .publish(&env);
        Ok(())
    }

    /// fund_core when the loan closes or defaults: the unclaimed hours lapse.
    pub fn void(env: Env, union: Address, token_id: BytesN<32>) -> Result<(), Error> {
        require_core(&env)?;
        if let Ok(mut v) = load_voucher(&env, &union, &token_id) {
            if !v.void {
                v.void = true;
                save_voucher(&env, &union, &token_id, &v);
                VoucherVoided {
                    union,
                    token_id,
                    unused_hours: v.hours - v.used,
                }
                .publish(&env);
            }
        }
        Ok(())
    }

    /// D19: the owner scans her QR after the job and claims the hours worked, `hours` in hundredths of
    /// an hour (23 Oct), using ≤ `max_units` centi tractor hours, the units she signed for: her
    /// authorisation of (union, token_id, max_units) is used once, its expiry ledger ~48 h out). fund_core pays the prepay from senior money at once and books the
    /// rest as pending (D18, R85).
    #[allow(clippy::too_many_arguments)]
    pub fn claim(
        env: Env,
        owner: Address,
        machine_id: u64,
        union: Address,
        token_id: BytesN<32>,
        max_units: u32,
        hours: u32,
    ) -> Result<(i128, i128), Error> {
        require_not_paused(&env)?;
        owner.require_auth();
        let m = load_machine(&env, machine_id)?;
        if m.owner != owner {
            return Err(Error::NotOwner);
        }
        if m.union != union || !m.confirmed || m.on_hold {
            return Err(Error::Unavailable);
        }
        let mut v = load_voucher(&env, &union, &token_id)?;
        if v.member == owner {
            return Err(Error::SelfClaim);
        }
        if v.void {
            return Err(Error::NotFound);
        }
        v.member
            .require_auth_for_args((union.clone(), token_id.clone(), max_units).into_val(&env));
        let now = env.ledger().timestamp();
        let r = rate(&env, &union, m.machine_type).ok_or(Error::NoRate)?;
        let tractor = rate(&env, &union, TRACTOR_TYPE).ok_or(Error::NoRate)?;
        // The hours worked on this machine (hundredths), in tractor-hour units against her voucher.
        let units = units_for(&r, &tractor, hours)?;
        if hours == 0 || units > max_units || units > v.hours - v.used {
            return Err(Error::TooManyHours);
        }
        let wk = DataKey::WeekUse(machine_id, now / WEEK);
        let week_used: u32 = env.storage().temporary().get(&wk).unwrap_or(0);
        // The week counts hundredths of an hour; hours_per_week stays whole hours (L13).
        if week_used + hours > m.hours_per_week.saturating_mul(100) {
            return Err(Error::TooManyHours);
        }
        // Temporary storage (C7): the count only matters for its week, then it is deleted by
        // the network. It is kept alive a little past the week's end, never less.
        env.storage().temporary().set(&wk, &(week_used + hours));
        env.storage()
            .temporary()
            .extend_ttl(&wk, WEEK_USE_LEDGERS, WEEK_USE_LEDGERS);
        let pre = prepay_per_hour(&r, prepay_driver_bp(&env, &union));
        // Money for hundredths of an hour, rounded down; pending takes the rest so the two add up to
        // the machine's rate × the time worked.
        let prepay = pre * i128::from(hours) / 100;
        let pending = total_per_hour(&r) * i128::from(hours) / 100 - prepay;
        // One claim per signature; the rest of her hours stay with her (R55).
        v.used += units;
        save_voucher(&env, &union, &token_id, &v);
        FundCoreClient::new(&env, &core_addr(&env)?)
            .pay_voucher(&union, &token_id, &owner, &units, &prepay, &pending);
        VoucherClaimed {
            union,
            token_id,
            machine_id,
            owner,
            hours,
            units,
            prepay,
            pending,
        }
        .publish(&env);
        bump_instance(&env);
        Ok((prepay, pending))
    }

    // ---------- views ----------

    /// The senior money one tractor hour (the voucher's unit) needs at the job: the prepay of
    /// machine type 1. Another machine's hour uses fewer or more units at the same prepay ratio.
    pub fn prepay_per_tractor_hour(env: Env, union: Address) -> i128 {
        rate(&env, &union, TRACTOR_TYPE)
            .map(|r| prepay_per_hour(&r, prepay_driver_bp(&env, &union)))
            .unwrap_or(0)
    }

    pub fn rate(env: Env, union: Address, machine_type: u32) -> Option<RateParts> {
        rate(&env, &union, machine_type)
    }

    pub fn prepay_share(env: Env, union: Address) -> u32 {
        prepay_driver_bp(&env, &union)
    }

    pub fn machine(env: Env, machine_id: u64) -> Option<Machine> {
        env.storage()
            .persistent()
            .get(&DataKey::Machine(machine_id))
    }

    pub fn voucher(env: Env, union: Address, token_id: BytesN<32>) -> Option<Voucher> {
        env.storage()
            .persistent()
            .get(&DataKey::Voucher(union, token_id))
    }

    /// Hundredths of an hour worked on the machine this week (23 Oct).
    pub fn hours_this_week(env: Env, machine_id: u64) -> u32 {
        env.storage()
            .temporary()
            .get(&DataKey::WeekUse(
                machine_id,
                env.ledger().timestamp() / WEEK,
            ))
            .unwrap_or(0)
    }

    pub fn fund_core(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::FundCore)
    }

    pub fn admin(env: Env) -> Address {
        get(&env, &DataKey::Admin)
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    // ---------- upgrade (admin, timelock, nila_common::upgrade) ----------

    pub fn set_upgrade_delay(env: Env, delay_seconds: u64) -> Result<(), Error> {
        require_admin(&env)?;
        if !upgrade::set_delay(&env, delay_seconds) {
            return Err(Error::BadDelay);
        }
        bump_instance(&env);
        Ok(())
    }

    pub fn upgrade_delay(env: Env) -> u64 {
        upgrade::delay(&env)
    }

    pub fn pending_upgrade(env: Env) -> Option<upgrade::PendingUpgrade> {
        upgrade::pending(&env)
    }

    pub fn pending_delay(env: Env) -> Option<upgrade::PendingDelay> {
        upgrade::pending_delay(&env)
    }

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
}

#[cfg(test)]
mod test;
