#![no_std]

use nila_common::bump_instance;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, Address, Env,
};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(
    key = "desc",
    val = "Nila FX pool skeleton — USDC in, nIN mint into fund"
);

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
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Usdc,
    Nin,
    FundCore,
    DailyCap,
    WindowStart,
    WindowOut,
    Escrow(u64),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub union: Address,
    pub remaining: i128,
    pub expires: u64,
}

#[contractevent]
pub struct EscrowResolved {
    #[topic]
    pub escrow_id: u64,
    pub amount: i128,
}

#[contractevent]
pub struct FundCoreSet {
    pub fund_core: Address,
}

#[contract]
pub struct FxPoolContract;

fn require_admin(env: &Env) -> Result<(), Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(())
}

#[contractimpl]
impl FxPoolContract {
    pub fn __constructor(env: Env, admin: Address, usdc: Address, nin: Address, daily_cap: i128) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Usdc, &usdc);
        env.storage().instance().set(&DataKey::Nin, &nin);
        env.storage().instance().set(&DataKey::DailyCap, &daily_cap);
        env.storage().instance().set(&DataKey::WindowStart, &0u64);
        env.storage().instance().set(&DataKey::WindowOut, &0i128);
        bump_instance(&env);
    }

    pub fn set_fund_core(env: Env, fund_core: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::FundCore, &fund_core);
        FundCoreSet { fund_core }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn set_daily_cap(env: Env, cap: i128) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::DailyCap, &cap);
        bump_instance(&env);
        Ok(())
    }

    /// Called by fund_core after disbursement. No read-back into Core (TAD §7.1).
    pub fn resolve_escrow_cash(env: Env, escrow_id: u64, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::AmountZero);
        }
        let core: Address = env
            .storage()
            .instance()
            .get(&DataKey::FundCore)
            .ok_or(Error::NotCore)?;
        core.require_auth();
        let key = DataKey::Escrow(escrow_id);
        if let Some(mut e) = env.storage().temporary().get::<_, Escrow>(&key) {
            e.remaining = if amount >= e.remaining {
                0
            } else {
                e.remaining - amount
            };
            env.storage().temporary().set(&key, &e);
        }
        EscrowResolved { escrow_id, amount }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn open_escrow(
        env: Env,
        escrow_id: u64,
        union: Address,
        amount: i128,
        ttl_seconds: u64,
    ) -> Result<(), Error> {
        require_admin(&env)?;
        if amount <= 0 {
            return Err(Error::AmountZero);
        }
        let esc = Escrow {
            union,
            remaining: amount,
            expires: env.ledger().timestamp().saturating_add(ttl_seconds),
        };
        let key = DataKey::Escrow(escrow_id);
        env.storage().temporary().set(&key, &esc);
        // ~5s ledgers; align TTL with escrow timeout (TAD §4.3).
        let ledgers = ((ttl_seconds / 5) as u32).max(nila_common::DAY_IN_LEDGERS);
        env.storage().temporary().extend_ttl(&key, ledgers, ledgers);
        bump_instance(&env);
        Ok(())
    }

    pub fn get_escrow(env: Env, escrow_id: u64) -> Option<Escrow> {
        env.storage().temporary().get(&DataKey::Escrow(escrow_id))
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
}

#[cfg(test)]
mod test;
