#![no_std]

use nila_common::bump_instance;
use nila_math::preview_rate_bp;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contractmeta, contracttype, vec, Address,
    BytesN, Env, String, Vec,
};

contractmeta!(key = "binver", val = "0.1.0");
contractmeta!(key = "desc", val = "Nila union registry and borrower index");

const MAX_FUND_TYPES: u32 = 32;
const MAX_LOANS_PER_BORROWER: u32 = 128;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    NotCore = 2,
    NotFound = 3,
    AlreadyExists = 4,
    CapExceeded = 5,
    Inactive = 6,
    BadIndex = 7,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Core,
    Union(Address),
    FundTypes(Address),
    BorrowerLoans(Address, Address), // (union, borrower)
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionRecord {
    pub id: Address,
    pub display_name: String,
    pub active: bool,
    pub created_ts: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundType {
    pub loan_type: BytesN<32>,
    pub display_name: String,
}

#[contractevent]
pub struct UnionCreated {
    #[topic]
    pub union: Address,
}

#[contractevent]
pub struct FundTypeAdded {
    #[topic]
    pub union: Address,
    pub loan_type: BytesN<32>,
}

#[contract]
pub struct FundViewerContract;

fn require_admin(env: &Env) -> Result<(), Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(())
}

fn require_core(env: &Env) -> Result<Address, Error> {
    let core: Address = env
        .storage()
        .instance()
        .get(&DataKey::Core)
        .ok_or(Error::NotCore)?;
    core.require_auth();
    Ok(core)
}

#[contractimpl]
impl FundViewerContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        bump_instance(&env);
    }

    pub fn set_core(env: Env, core: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Core, &core);
        bump_instance(&env);
        Ok(())
    }

    pub fn create_union(env: Env, union: Address, display_name: String) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::Union(union.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::AlreadyExists);
        }
        let rec = UnionRecord {
            id: union.clone(),
            display_name,
            active: true,
            created_ts: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&key, &rec);
        env.storage().persistent().extend_ttl(
            &key,
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
        );
        UnionCreated { union }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn activate_union(env: Env, union: Address, active: bool) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::Union(union);
        let mut rec: UnionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::NotFound)?;
        rec.active = active;
        env.storage().persistent().set(&key, &rec);
        bump_instance(&env);
        Ok(())
    }

    pub fn add_fund_type(
        env: Env,
        union: Address,
        loan_type: BytesN<32>,
        display_name: String,
    ) -> Result<(), Error> {
        require_admin(&env)?;
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Union(union.clone()))
        {
            return Err(Error::NotFound);
        }
        let key = DataKey::FundTypes(union.clone());
        let mut types: Vec<FundType> = env.storage().persistent().get(&key).unwrap_or(vec![&env]);
        if types.len() >= MAX_FUND_TYPES {
            return Err(Error::CapExceeded);
        }
        for t in types.iter() {
            if t.loan_type == loan_type {
                return Err(Error::AlreadyExists);
            }
        }
        types.push_back(FundType {
            loan_type: loan_type.clone(),
            display_name,
        });
        env.storage().persistent().set(&key, &types);
        env.storage().persistent().extend_ttl(
            &key,
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
        );
        FundTypeAdded { union, loan_type }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Swap-remove so fundIds stay in sync (TAD §7.1 fix vs Solidity).
    pub fn remove_fund_type(env: Env, union: Address, index: u32) -> Result<(), Error> {
        require_admin(&env)?;
        let key = DataKey::FundTypes(union);
        let mut types: Vec<FundType> = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::NotFound)?;
        if index >= types.len() {
            return Err(Error::BadIndex);
        }
        let last = types.len() - 1;
        if index != last {
            let swapped = types.get(last).unwrap();
            types.set(index, swapped);
        }
        types.pop_back();
        env.storage().persistent().set(&key, &types);
        bump_instance(&env);
        Ok(())
    }

    pub fn on_loan_created(
        env: Env,
        union: Address,
        loan_id: BytesN<32>,
        borrower: Address,
    ) -> Result<(), Error> {
        require_core(&env)?;
        let key = DataKey::BorrowerLoans(union, borrower);
        let mut ids: Vec<BytesN<32>> = env.storage().persistent().get(&key).unwrap_or(vec![&env]);
        if ids.len() >= MAX_LOANS_PER_BORROWER {
            return Err(Error::CapExceeded);
        }
        ids.push_back(loan_id);
        env.storage().persistent().set(&key, &ids);
        env.storage().persistent().extend_ttl(
            &key,
            nila_common::TTL_THRESHOLD,
            nila_common::TTL_BUMP,
        );
        bump_instance(&env);
        Ok(())
    }

    pub fn on_loan_closed(
        env: Env,
        union: Address,
        loan_id: BytesN<32>,
        borrower: Address,
    ) -> Result<(), Error> {
        require_core(&env)?;
        let key = DataKey::BorrowerLoans(union, borrower);
        let mut ids: Vec<BytesN<32>> = env.storage().persistent().get(&key).unwrap_or(vec![&env]);
        if let Some(i) = (0..ids.len()).find(|&i| ids.get(i).unwrap() == loan_id) {
            let last = ids.len() - 1;
            if i != last {
                let swapped = ids.get(last).unwrap();
                ids.set(i, swapped);
            }
            ids.pop_back();
            env.storage().persistent().set(&key, &ids);
        }
        bump_instance(&env);
        Ok(())
    }

    pub fn get_union(env: Env, union: Address) -> Option<UnionRecord> {
        env.storage().persistent().get(&DataKey::Union(union))
    }

    pub fn get_fund_types(env: Env, union: Address) -> Vec<FundType> {
        env.storage()
            .persistent()
            .get(&DataKey::FundTypes(union))
            .unwrap_or(vec![&env])
    }

    pub fn get_loans_by_borrower(env: Env, union: Address, borrower: Address) -> Vec<BytesN<32>> {
        env.storage()
            .persistent()
            .get(&DataKey::BorrowerLoans(union, borrower))
            .unwrap_or(vec![&env])
    }

    pub fn preview_rate_bp(env: Env, params: nila_math::RateParams, util_bp: u32) -> u32 {
        let _ = env;
        preview_rate_bp(&params, util_bp)
    }
}

#[cfg(test)]
mod test;
