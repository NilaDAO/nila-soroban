#![no_std]

pub use nila_common::{Role, UnionRole};

use nila_common::{bump_instance, upgrade};
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
    val = "Nila role registry: leaders, oracle, attester, core, union officers"
);

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotAdmin = 2,
    ZeroAddress = 3,
    UpgradeNotReady = 4,
    /// Above the 30-day maximum.
    BadDelay = 5,
    /// A union setting outside its bounds (wait 1 h – 30 d, session 60–900 s), a session contract
    /// without the core role, or only one of fx_pool and USDC: refused, not clamped.
    BadSetting = 6,
    /// The union is not admitted by the trust (`admit_union`, decision 008): it cannot name leaders,
    /// officers or settings, and `revoke_union` of a union not admitted is refused too.
    NotAdmitted = 7,
}

/// What a union sets for the Nila accounts that name it (executive decision 3 Nov: member-owned
/// accounts; was the per-union account factory). Signed by the union account (3 of 5).
/// nila-account decodes the same type (kept apart from nila-common so the other contracts do not
/// carry its spec).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionSettings {
    /// Recovery wait, seconds (1 hour – 30 days).
    pub wait: u64,
    /// Leader session length, seconds (60–900).
    pub session_len: u64,
    /// The contracts a leader session may call for an account: only contracts the trust gave the
    /// core role (`set_role(.., Core, true)`), i.e. fund_core (executive decision 4 Nov).
    pub session_contracts: Vec<Address>,
    /// The node's SMS key: with one leader it proposes a keypad recovery (none: SMS recovery off).
    pub sms_key: Option<Address>,
    /// The fx_pool that may lock an account's USDC for LP positions, and the USDC (none: no LPs).
    /// Both or neither.
    pub fx_pool: Option<Address>,
    pub usdc: Option<Address>,
}

/// Bounds of a union's account settings (as the account factory had them).
pub const WAIT_MIN: u64 = 3_600;
pub const WAIT_MAX: u64 = 30 * 86_400;
pub const SESSION_MIN: u64 = 60;
pub const SESSION_MAX: u64 = 900;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Role(Address, Role),                    // (account, role) — trust-wide
    Leader(Address, Address),               // (union, account)
    UnionRole(Address, Address, UnionRole), // (union, account, role)
    UnionSettings(Address),                 // union -> UnionSettings (3 Nov)
    Admitted(Address),                      // union -> details_hash (3 Nov, decision 008)
}

#[contractevent]
pub struct RoleUpdated {
    #[topic]
    pub account: Address,
    #[topic]
    pub role: Role,
    pub enabled: bool,
}

#[contractevent]
pub struct UnionRoleUpdated {
    #[topic]
    pub union: Address,
    #[topic]
    pub account: Address,
    pub role: UnionRole,
    pub enabled: bool,
}

#[contractevent]
pub struct LeaderUpdated {
    #[topic]
    pub union: Address,
    #[topic]
    pub account: Address,
    pub enabled: bool,
}

/// A union set its account settings (3 Nov: member-owned accounts).
#[contractevent]
pub struct UnionSettingsSet {
    #[topic]
    pub union: Address,
    pub settings: UnionSettings,
}

/// The trust admitted a union, or admitted it again with new details (decision 008).
/// `details_hash` = sha256(name ‖ 0x00 ‖ geo) of the union account's `nila.name` / `nila.geo` data.
#[contractevent]
pub struct UnionAdmitted {
    #[topic]
    pub union: Address,
    pub details_hash: BytesN<32>,
}

/// The trust revoked a union's admission (decision 008).
#[contractevent]
pub struct UnionRevoked {
    #[topic]
    pub union: Address,
}

/// The trust's admin account changed (C19).
#[contractevent]
pub struct AdminSet {
    pub admin: Address,
}

/// Upgrade timelock at deploy: 48 hours (C15).
#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = upgrade::DEFAULT_DELAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;
#[contract]
pub struct RolesContract;

fn require_admin(env: &Env) -> Result<Address, Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(admin)
}

/// A revoked flag is removed rather than stored as false, so it costs no rent.
fn store_flag(env: &Env, key: &DataKey, enabled: bool) {
    let store = env.storage().persistent();
    if enabled {
        store.set(key, &true);
        store.extend_ttl(key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
    } else {
        store.remove(key);
    }
}

/// A set flag is extended on every read: a leader appointed once and only checked
/// afterwards must not be archived after 120 days.
fn read_flag(env: &Env, key: &DataKey) -> bool {
    let store = env.storage().persistent();
    if store.has(key) {
        store.extend_ttl(key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
        true
    } else {
        false
    }
}

/// The union's admission hash, TTL extended on read like the flags; None if never admitted or revoked.
fn admission(env: &Env, union: &Address) -> Option<BytesN<32>> {
    let key = DataKey::Admitted(union.clone());
    let store = env.storage().persistent();
    let h: Option<BytesN<32>> = store.get(&key);
    if h.is_some() {
        store.extend_ttl(&key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
    }
    h
}

fn require_admitted(env: &Env, union: &Address) -> Result<(), Error> {
    if admission(env, union).is_none() {
        return Err(Error::NotAdmitted);
    }
    Ok(())
}

#[contractimpl]
impl RolesContract {
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

    /// Trust-wide role (oracle, attester, core). Admin only (R4, R75).
    pub fn set_role(env: Env, account: Address, role: Role, enabled: bool) -> Result<(), Error> {
        require_admin(&env)?;
        store_flag(&env, &DataKey::Role(account.clone(), role), enabled);
        RoleUpdated {
            account,
            role,
            enabled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The trust admits a union (decision 008): admin only (the trust's 3 of 5), never the union
    /// itself. Admitting again replaces the details hash. Until admitted, a union cannot name
    /// leaders, officers or settings.
    pub fn admit_union(env: Env, union: Address, details_hash: BytesN<32>) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::Admitted(union.clone());
        let store = env.storage().persistent();
        store.set(&key, &details_hash);
        store.extend_ttl(&key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
        UnionAdmitted {
            union,
            details_hash,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The trust revokes a union's admission (admin only). The union's leaders, officers and
    /// settings stay stored, but while it is not admitted `is_leader`, `has_union_role` and
    /// `union_settings` answer as if they were absent (false / None): its leaders lose their
    /// rights and its members' accounts lose sessions and recovery at once. Admitting it again
    /// restores them as they were.
    pub fn revoke_union(env: Env, union: Address) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::Admitted(union.clone());
        let store = env.storage().persistent();
        if !store.has(&key) {
            return Err(Error::NotAdmitted);
        }
        store.remove(&key);
        UnionRevoked { union }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The details hash the union was admitted with; None if never admitted or revoked.
    pub fn union_admission(env: Env, union: Address) -> Option<BytesN<32>> {
        admission(&env, &union)
    }

    pub fn is_union(env: Env, union: Address) -> bool {
        admission(&env, &union).is_some()
    }

    /// Leaders of a union, all equal. Signed by the union account itself, whose
    /// 5 signers / threshold 3 make `require_auth` the 3 of 5 (D12, R4, R5).
    pub fn set_leader(
        env: Env,
        union: Address,
        account: Address,
        enabled: bool,
    ) -> Result<(), Error> {
        union.require_auth();
        require_admitted(&env, &union)?;
        store_flag(
            &env,
            &DataKey::Leader(union.clone(), account.clone()),
            enabled,
        );
        LeaderUpdated {
            union,
            account,
            enabled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Union officers (treasurer, secretary). Signed by the union account (D12, R4).
    pub fn set_union_role(
        env: Env,
        union: Address,
        account: Address,
        role: UnionRole,
        enabled: bool,
    ) -> Result<(), Error> {
        union.require_auth();
        require_admitted(&env, &union)?;
        store_flag(
            &env,
            &DataKey::UnionRole(union.clone(), account.clone(), role),
            enabled,
        );
        UnionRoleUpdated {
            union,
            account,
            role,
            enabled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The settings the union's accounts read (recovery wait, session, SMS key, fx_pool and USDC).
    /// Signed by the union account (3 of 5). Refused (`BadSetting`): values out of range, a session
    /// contract the trust has not given the core role (review 5 Nov: a union could otherwise let
    /// leaders sign XLM or fx_pool calls for its members), or fx_pool without USDC or the reverse.
    pub fn set_union_settings(
        env: Env,
        union: Address,
        settings: UnionSettings,
    ) -> Result<(), Error> {
        union.require_auth();
        require_admitted(&env, &union)?;
        if !(WAIT_MIN..=WAIT_MAX).contains(&settings.wait)
            || !(SESSION_MIN..=SESSION_MAX).contains(&settings.session_len)
            || settings.fx_pool.is_some() != settings.usdc.is_some()
            || settings
                .session_contracts
                .iter()
                .any(|c| !read_flag(&env, &DataKey::Role(c, Role::Core)))
        {
            return Err(Error::BadSetting);
        }
        let key = DataKey::UnionSettings(union.clone());
        let store = env.storage().persistent();
        store.set(&key, &settings);
        store.extend_ttl(&key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
        UnionSettingsSet { union, settings }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// None until the union sets them, or while the union is not admitted (revoked): an account
    /// of that union then gets no recovery or session.
    pub fn union_settings(env: Env, union: Address) -> Option<UnionSettings> {
        admission(&env, &union)?;
        let key = DataKey::UnionSettings(union);
        let store = env.storage().persistent();
        let s: Option<UnionSettings> = store.get(&key);
        if s.is_some() {
            store.extend_ttl(&key, nila_common::TTL_THRESHOLD, nila_common::TTL_BUMP);
        }
        s
    }

    pub fn has_role(env: Env, account: Address, role: Role) -> bool {
        read_flag(&env, &DataKey::Role(account, role))
    }

    /// False while the union is not admitted (revoked), whatever is stored.
    pub fn has_union_role(env: Env, union: Address, account: Address, role: UnionRole) -> bool {
        admission(&env, &union).is_some()
            && read_flag(&env, &DataKey::UnionRole(union, account, role))
    }

    /// Kept for fund_core's `RolesClient`; same as `has_role(account, Oracle)`.
    pub fn is_oracle(env: Env, account: Address) -> bool {
        read_flag(&env, &DataKey::Role(account, Role::Oracle))
    }

    /// False while the union is not admitted (revoked), whatever is stored.
    pub fn is_leader(env: Env, union: Address, account: Address) -> bool {
        admission(&env, &union).is_some() && read_flag(&env, &DataKey::Leader(union, account))
    }

    pub fn admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    // ---------- upgrade (admin, 48-hour timelock, nila_common::upgrade) ----------

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

    pub fn upgrade_delay(env: Env) -> u64 {
        upgrade::delay(&env)
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
}

#[cfg(test)]
mod test;
