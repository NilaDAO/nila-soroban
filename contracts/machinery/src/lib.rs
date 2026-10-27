#![no_std]
//! machinery: machinery vouchers on Stellar (C5, Nov 7; executive decision 11 Oct, N2).
//!
//! - **Price table** per union and machine type, in parts **per acre** (executive decision 27 Oct, was per
//!   hour): driver, diesel, maintenance, insurance, depreciation, margin (D16, D20, R57). Set by the union
//!   account (its leaders, 3 of 5); open and adjustable, like the prepay share (executive decision, 12 Oct).
//!   Optionally a price per machine type **and crop family** (27 Oct: hauling sugarcane is not hauling paddy);
//!   without one the type's price applies.
//! - **Machines** (D15, R82, R83): a leader registers a member's machine (L13); the owner
//!   confirms (N9) and can put it on hold with a reason.
//! - **Vouchers** (R51–R55): a loan's voucher part is machinery in **tractor acres** (executive
//!   decision 27 Oct; was tractor hours, 23 Oct), held in hundredths (`units`): one tractor acre is the
//!   price of machine type 1 (tractor + rotavator) for an acre; an acre of another machine (or crop) uses
//!   its price ÷ the tractor's. Granted by fund_core at the draw. Non-transferable by construction (C5).
//!   She signs a one-time authorisation for `claim` up to X units (her QR; Soroban's nonce and expiry
//!   ledger, ~48 h); the owner scans it and claims the acres worked (D19), never more than the crop
//!   share's own area (its token id's area), within the machine's acres a week (R84).
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
    /// More than her authorisation, the voucher, the share's area or the machine's week allows.
    TooMuchWork = 11,
    Paused = 15,
    VoucherExists = 16,
    UpgradeNotReady = 17,
    BadDelay = 18,
    CoreAlreadySet = 19,
    /// An owner cannot claim on her own voucher.
    SelfClaim = 20,
}

/// One acre of a machine's work, in nIN (7 decimals), by part (D16, D20; per acre since 27 Oct).
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
    pub acres_per_week: u32,
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
    /// Centi tractor acres granted (27 Oct; were centi tractor hours).
    pub units: u32,
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
    /// A price an acre for one machine type on one crop family (union, type, family), 27 Oct.
    CropRate(Address, u32, u32),
    Types(Address),
    PrepayDriverBp(Address),
    NextMachine,
    Machine(u64),
    Voucher(Address, BytesN<32>),
    /// Centi acres a machine worked in a week (machine id, week index). **Temporary** storage (C7):
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
pub struct CropRateSet {
    #[topic]
    pub union: Address,
    #[topic]
    pub machine_type: u32,
    pub crop_family: u32,
    /// None: the crop's own price is removed, the type's price applies again.
    pub parts: Option<RateParts>,
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
    pub acres_per_week: u32,
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
    pub units: u32,
}

#[contractevent]
pub struct VoucherClaimed {
    #[topic]
    pub union: Address,
    #[topic]
    pub token_id: BytesN<32>,
    pub machine_id: u64,
    pub owner: Address,
    /// Acres worked, in hundredths of an acre.
    pub acres: u32,
    /// Centi tractor acres they used of her voucher.
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
    pub unused_units: u32,
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

/// The price an acre of `t` on crop family `family`: the crop's own price, else the type's (27 Oct).
fn rate_for(env: &Env, union: &Address, t: u32, family: u32) -> Option<RateParts> {
    env.storage()
        .persistent()
        .get(&DataKey::CropRate(union.clone(), t, family))
        .or_else(|| rate(env, union, t))
}

/// `width` bits of a token id from bit `shift` (FoodToken packTokenId: a big-endian uint256).
fn token_bits(t: &BytesN<32>, shift: u32, width: u32) -> u64 {
    let b = t.to_array();
    let mut v = 0u64;
    for i in 0..width {
        let bit = shift + i;
        if (b[31 - (bit / 8) as usize] >> (bit % 8)) & 1 == 1 {
            v |= 1 << i;
        }
    }
    v
}

/// The crop share's crop family (crop code ÷ 1000, bits 204–223) and its area in centi acres, rounded up (areaM2,
/// bits 148–171; 0 for a token without one).
fn share_of(t: &BytesN<32>) -> (u32, u64) {
    let family = (token_bits(t, 204, 20) / 1000) as u32;
    let m2 = token_bits(t, 148, 24);
    (family, (m2 * 1_000_000 + 40_468_563) / 40_468_564)
}

/// Paid at the job, per acre: diesel plus the union's share of the driver part (D18).
fn prepay_per_acre(r: &RateParts, driver_bp: u32) -> i128 {
    r.diesel + r.driver * i128::from(driver_bp) / 10_000
}

fn total_per_acre(r: &RateParts) -> i128 {
    r.driver + r.diesel + r.maintenance + r.insurance + r.depreciation + r.margin
}

/// The machine type whose acre is the voucher's unit (executive decision 27 Oct; was its hour, 23 Oct).
pub const TRACTOR_TYPE: u32 = 1;

/// Centi tractor acres that `centi` hundredths of an acre by a machine at price `r` use, rounded up.
fn units_for(r: &RateParts, tractor: &RateParts, centi: u32) -> Result<u32, Error> {
    let t = total_per_acre(tractor);
    if t <= 0 {
        return Err(Error::NoRate);
    }
    let n = total_per_acre(r)
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

    /// The union account (its leaders, 3 of 5) sets the price of a machine's acre in parts. The
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
            || total_per_acre(&parts) <= 0
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

    /// A price an acre for `machine_type` on one crop family (27 Oct), e.g. trailer × sugarcane; `None` removes it
    /// (the type's price applies again). The type must have its own price first.
    pub fn set_crop_rate(
        env: Env,
        union: Address,
        machine_type: u32,
        crop_family: u32,
        parts: Option<RateParts>,
    ) -> Result<(), Error> {
        union.require_auth();
        if rate(&env, &union, machine_type).is_none() {
            return Err(Error::NoRate);
        }
        let k = DataKey::CropRate(union.clone(), machine_type, crop_family);
        match &parts {
            Some(p) => {
                if p.driver < 0
                    || p.diesel < 0
                    || p.maintenance < 0
                    || p.insurance < 0
                    || p.depreciation < 0
                    || p.margin < 0
                    || total_per_acre(p) <= 0
                {
                    return Err(Error::BadArg);
                }
                env.storage().persistent().set(&k, p);
                bump(&env, &k);
            }
            None => env.storage().persistent().remove(&k),
        }
        CropRateSet {
            union,
            machine_type,
            crop_family,
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
        acres_per_week: u32,
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
        if acres_per_week == 0 || acres_per_week > 10_000 {
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
                acres_per_week,
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
            acres_per_week,
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
        units: u32,
    ) -> Result<(), Error> {
        require_core(&env)?;
        // Work added to a share already taken up (27 Oct, draw_loan again): onto its open voucher.
        if let Ok(mut v) = load_voucher(&env, &union, &token_id) {
            if v.void || v.member != member {
                return Err(Error::VoucherExists);
            }
            v.units = v.units.checked_add(units).ok_or(Error::VoucherExists)?;
            save_voucher(&env, &union, &token_id, &v);
            VoucherGranted {
                union,
                token_id,
                member,
                units,
            }
            .publish(&env);
            return Ok(());
        }
        save_voucher(
            &env,
            &union,
            &token_id,
            &Voucher {
                member: member.clone(),
                units,
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
            units,
        }
        .publish(&env);
        Ok(())
    }

    /// fund_core at a default: the unclaimed work lapses (they never lapse otherwise, 26 Oct).
    pub fn void(env: Env, union: Address, token_id: BytesN<32>) -> Result<(), Error> {
        require_core(&env)?;
        if let Ok(mut v) = load_voucher(&env, &union, &token_id) {
            if !v.void {
                v.void = true;
                save_voucher(&env, &union, &token_id, &v);
                VoucherVoided {
                    union,
                    token_id,
                    unused_units: v.units - v.used,
                }
                .publish(&env);
            }
        }
        Ok(())
    }

    /// fund_core: she hands back `units` unclaimed centi tractor acres (26 Oct); her debt drops.
    pub fn take_back(
        env: Env,
        union: Address,
        token_id: BytesN<32>,
        units: u32,
    ) -> Result<(), Error> {
        require_core(&env)?;
        let mut v = load_voucher(&env, &union, &token_id)?;
        if v.void || units > v.units - v.used {
            return Err(Error::TooMuchWork);
        }
        v.units -= units;
        save_voucher(&env, &union, &token_id, &v);
        Ok(())
    }

    /// D19: the owner scans her QR after the job and claims the acres worked, `acres` in hundredths of an
    /// acre (27 Oct; were hours), never more than the crop share's own area, using ≤ `max_units` centi tractor
    /// acres, the units she signed for: her authorisation of (union, token_id, max_units) is used once, its
    /// expiry ledger ~48 h out. The price is the machine type's on the share's crop (its own crop price if the
    /// union set one). fund_core pays the owner in full from the voucher's reserve (26 Oct).
    #[allow(clippy::too_many_arguments)]
    pub fn claim(
        env: Env,
        owner: Address,
        machine_id: u64,
        union: Address,
        token_id: BytesN<32>,
        max_units: u32,
        acres: u32,
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
        let (family, area) = share_of(&token_id);
        let r = rate_for(&env, &union, m.machine_type, family).ok_or(Error::NoRate)?;
        let tractor = rate(&env, &union, TRACTOR_TYPE).ok_or(Error::NoRate)?;
        // The acres worked by this machine (hundredths), in tractor-acre units against her voucher; never more than
        // the share's own area (a second pass is a second claim).
        let units = units_for(&r, &tractor, acres)?;
        if acres == 0
            || (area > 0 && u64::from(acres) > area)
            || units > max_units
            || units > v.units - v.used
        {
            return Err(Error::TooMuchWork);
        }
        let wk = DataKey::WeekUse(machine_id, now / WEEK);
        let week_used: u32 = env.storage().temporary().get(&wk).unwrap_or(0);
        // The week counts hundredths of an acre; acres_per_week stays whole acres (L13).
        if week_used + acres > m.acres_per_week.saturating_mul(100) {
            return Err(Error::TooMuchWork);
        }
        // Temporary storage (C7): the count only matters for its week, then it is deleted by
        // the network. It is kept alive a little past the week's end, never less.
        env.storage().temporary().set(&wk, &(week_used + acres));
        env.storage()
            .temporary()
            .extend_ttl(&wk, WEEK_USE_LEDGERS, WEEK_USE_LEDGERS);
        // The owner is paid in full at the claim (26 Oct): the price an acre × the acres worked,
        // from the voucher's reserve; her debt is separate.
        let prepay = total_per_acre(&r) * i128::from(acres) / 100;
        let pending = 0;
        // One claim per signature; the rest of her voucher stays with her (R55).
        v.used += units;
        save_voucher(&env, &union, &token_id, &v);
        FundCoreClient::new(&env, &core_addr(&env)?)
            .pay_voucher(&union, &token_id, &owner, &units, &prepay);
        VoucherClaimed {
            union,
            token_id,
            machine_id,
            owner,
            acres,
            units,
            prepay,
            pending,
        }
        .publish(&env);
        bump_instance(&env);
        Ok((prepay, pending))
    }

    // ---------- views ----------

    /// The senior money one tractor acre (the voucher's unit) needs at the job: the prepay of
    /// machine type 1. Another machine's acre uses fewer or more units at the same prepay ratio.
    pub fn prepay_per_tractor_acre(env: Env, union: Address) -> i128 {
        rate(&env, &union, TRACTOR_TYPE)
            .map(|r| prepay_per_acre(&r, prepay_driver_bp(&env, &union)))
            .unwrap_or(0)
    }

    /// The value of one tractor acre (the voucher's unit, 27 Oct): machine type 1's whole price an acre.
    /// fund_core lends it at the draw and pays owners from it.
    pub fn value_per_tractor_acre(env: Env, union: Address) -> i128 {
        rate(&env, &union, TRACTOR_TYPE)
            .map(|r| total_per_acre(&r))
            .unwrap_or(0)
    }

    pub fn rate(env: Env, union: Address, machine_type: u32) -> Option<RateParts> {
        rate(&env, &union, machine_type)
    }

    /// The price an acre of `machine_type` on crop family `crop_family`: its crop price, else the type's.
    pub fn rate_for_crop(
        env: Env,
        union: Address,
        machine_type: u32,
        crop_family: u32,
    ) -> Option<RateParts> {
        rate_for(&env, &union, machine_type, crop_family)
    }

    /// A crop share's (crop family, area in centi acres) as this contract reads them from its token id.
    pub fn share_of(token_id: BytesN<32>) -> (u32, u64) {
        share_of(&token_id)
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

    /// Hundredths of an acre worked by the machine this week (27 Oct).
    pub fn acres_this_week(env: Env, machine_id: u64) -> u32 {
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
