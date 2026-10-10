//! One timelocked upgrade path for every Nila contract (R75, C15, C19).
//!
//! The calling contract checks its admin first; these functions only keep the clock.
//! - An upgrade is proposed, then executed once the delay has passed, or cancelled.
//! - Raising the delay applies at once. Lowering it is filed and applies only when the same
//!   value is set again after the current delay has passed, so a key that gets stolen
//!   cannot shorten the window it is watched through.
//! - Every step is evented.

// The macro-generated code for `UpgradeKey` trips this lint on its deliberate prefix.
#![allow(clippy::enum_variant_names)]

use soroban_sdk::{contractevent, contracttype, BytesN, Env};

/// 48 hours at deploy (C15).
pub const DEFAULT_DELAY: u64 = 48 * 3_600;

/// Prefixed so they never collide with a contract's own instance keys.
#[contracttype]
#[derive(Clone)]
pub enum UpgradeKey {
    UpgDelay,
    UpgPending,
    UpgPendingDelay,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub wasm_hash: BytesN<32>,
    pub eta: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDelay {
    pub delay: u64,
    pub eta: u64,
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
pub struct UpgradeCancelled {
    pub wasm_hash: BytesN<32>,
}

/// `eta` is when a lowered delay may be applied; 0 once the delay is in force.
#[contractevent]
pub struct UpgradeDelaySet {
    pub delay: u64,
    pub eta: u64,
}

pub fn init(env: &Env, delay: u64) {
    env.storage().instance().set(&UpgradeKey::UpgDelay, &delay);
}

pub fn delay(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&UpgradeKey::UpgDelay)
        .unwrap_or(DEFAULT_DELAY)
}

pub fn pending(env: &Env) -> Option<PendingUpgrade> {
    env.storage().instance().get(&UpgradeKey::UpgPending)
}

pub fn pending_delay(env: &Env) -> Option<PendingDelay> {
    env.storage().instance().get(&UpgradeKey::UpgPendingDelay)
}

pub fn set_delay(env: &Env, new_delay: u64) {
    let s = env.storage().instance();
    let cur = delay(env);
    if new_delay < cur {
        let now = env.ledger().timestamp();
        match pending_delay(env) {
            Some(p) if p.delay == new_delay && now >= p.eta => {
                s.remove(&UpgradeKey::UpgPendingDelay);
            }
            _ => {
                let eta = now.saturating_add(cur);
                s.set(
                    &UpgradeKey::UpgPendingDelay,
                    &PendingDelay {
                        delay: new_delay,
                        eta,
                    },
                );
                UpgradeDelaySet {
                    delay: new_delay,
                    eta,
                }
                .publish(env);
                return;
            }
        }
    }
    s.set(&UpgradeKey::UpgDelay, &new_delay);
    UpgradeDelaySet {
        delay: new_delay,
        eta: 0,
    }
    .publish(env);
}

/// Files the upgrade; with a zero delay it applies at once.
pub fn propose(env: &Env, wasm_hash: BytesN<32>) {
    let d = delay(env);
    if d == 0 {
        apply(env, wasm_hash);
        return;
    }
    let eta = env.ledger().timestamp().saturating_add(d);
    env.storage().instance().set(
        &UpgradeKey::UpgPending,
        &PendingUpgrade {
            wasm_hash: wasm_hash.clone(),
            eta,
        },
    );
    UpgradeProposed { wasm_hash, eta }.publish(env);
}

/// False when nothing is filed or its delay has not passed.
pub fn execute(env: &Env) -> bool {
    match pending(env) {
        Some(p) if env.ledger().timestamp() >= p.eta => {
            env.storage().instance().remove(&UpgradeKey::UpgPending);
            apply(env, p.wasm_hash);
            true
        }
        _ => false,
    }
}

pub fn cancel(env: &Env) {
    if let Some(p) = pending(env) {
        env.storage().instance().remove(&UpgradeKey::UpgPending);
        UpgradeCancelled {
            wasm_hash: p.wasm_hash,
        }
        .publish(env);
    }
}

fn apply(env: &Env, wasm_hash: BytesN<32>) {
    env.deployer()
        .update_current_contract_wasm(wasm_hash.clone());
    UpgradeApplied { wasm_hash }.publish(env);
}
