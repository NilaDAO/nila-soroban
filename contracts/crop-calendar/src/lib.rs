#![no_std]
//! The crop calendar (executive decision 25 Oct): per crop family, its sowing windows in the
//! year, as month ranges set by the trust from the agronomy catalogue (Tamil Nadu seasons).
//! fund_core reads it at attest: a crop share is usable from a week before the sowing window
//! that holds its planned sowing up to the window's last day.

use nila_common::{bump_instance, upgrade, TTL_BUMP, TTL_THRESHOLD};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address,
    BytesN, Env, Vec,
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
    val = "Nila crop calendar: sowing windows per crop family"
);

const DAY: u64 = 86_400;
/// A share opens this long before its sowing window (25 Oct: one week).
pub const LEAD: u64 = 7 * DAY;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    UpgradeNotReady = 2,
    BadDelay = 3,
    /// A month outside 1–12, or two windows that share a month.
    BadWindow = 4,
}

/// One sowing window: from the first day of `first` to the last day of `last` (months 1–12;
/// `first` > `last` runs over the new year, e.g. December to February).
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Window {
    pub first: u32,
    pub last: u32,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Windows(u32), // crop family → its windows
}

#[contractevent]
pub struct WindowsSet {
    #[topic]
    pub family: u32,
    pub windows: Vec<Window>,
}

/// The trust's admin account changed.
#[contractevent]
pub struct AdminSet {
    pub admin: Address,
}

#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = upgrade::DEFAULT_DELAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;

fn require_admin(env: &Env) -> Result<Address, Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(admin)
}

/// Days since 1970-01-01 of a civil date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Year and month of a day count since 1970-01-01.
fn year_month(days: i64) -> (i64, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m)
}

fn covers(w: &Window, m: u32) -> bool {
    if w.first <= w.last {
        w.first <= m && m <= w.last
    } else {
        m >= w.first || m <= w.last
    }
}

/// The window of a sowing on `sos` among `windows`: (opening, last second of its last day).
pub fn window_of(windows: &Vec<Window>, sos: u64) -> Option<(u64, u64)> {
    let (y, m) = year_month((sos / DAY) as i64);
    let w = windows.iter().find(|w| covers(w, m))?;
    let wraps = w.first > w.last;
    let y0 = if wraps && m <= w.last { y - 1 } else { y };
    let y1 = if wraps && m >= w.first { y + 1 } else { y };
    let start = days_from_civil(y0, w.first, 1) as u64 * DAY;
    let (ny, nm) = if w.last == 12 {
        (y1 + 1, 1)
    } else {
        (y1, w.last + 1)
    };
    let end = days_from_civil(ny, nm, 1) as u64 * DAY - 1;
    Some((start.saturating_sub(LEAD), end))
}

#[contract]
pub struct CropCalendarContract;

#[contractimpl]
impl CropCalendarContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        upgrade::init(&env, UPGRADE_DELAY);
        bump_instance(&env);
    }

    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        AdminSet { admin: new_admin }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// A crop family's sowing windows (the trust, from the catalogue); empty removes them.
    pub fn set_windows(env: Env, family: u32, windows: Vec<Window>) -> Result<(), Error> {
        require_admin(&env)?;
        let mut used = 0u32;
        for w in windows.iter() {
            if !(1..=12).contains(&w.first) || !(1..=12).contains(&w.last) {
                return Err(Error::BadWindow);
            }
            for m in 1..=12u32 {
                if covers(&w, m) {
                    if used & (1 << m) != 0 {
                        return Err(Error::BadWindow);
                    }
                    used |= 1 << m;
                }
            }
        }
        let key = DataKey::Windows(family);
        if windows.is_empty() {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &windows);
            env.storage()
                .persistent()
                .extend_ttl(&key, TTL_THRESHOLD, TTL_BUMP);
        }
        WindowsSet { family, windows }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn windows(env: Env, family: u32) -> Vec<Window> {
        let key = DataKey::Windows(family);
        let store = env.storage().persistent();
        match store.get::<_, Vec<Window>>(&key) {
            Some(w) => {
                store.extend_ttl(&key, TTL_THRESHOLD, TTL_BUMP);
                w
            }
            None => Vec::new(&env),
        }
    }

    /// The window of a crop share: crop code (family × 1000 + variety) and planned sowing (unix).
    /// None when its family has no window holding that month.
    pub fn window(env: Env, crop_code: u32, sos: u64) -> Option<(u64, u64)> {
        window_of(&Self::windows(env, crop_code / 1000), sos)
    }

    pub fn admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    // ---------- upgrade (admin, 48-hour timelock, nila_common::upgrade) ----------

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
