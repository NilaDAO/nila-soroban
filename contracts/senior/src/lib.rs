#![no_std]
//! Senior investors (executive decision 2 Nov): seniors hold USD. Their USDC is the guarantee behind
//! vouchers and stays in this contract; fx_pool mints the same value in nIN at the attester's fix into
//! fund_core's senior pool in their name (`fx_pool.mint_senior_nin`).
//! - Principal comes back at par in USDC (`redeem`, after fund_core's 14-day unbond), pro rata to the
//!   principal fund_core burns; a haircut pays less and the rest stays here.
//! - Interest is earned in rupees and crosses the exchange rate: at an exit it stays owed in fund_core
//!   until a member LP of the union takes those rupees at the office and pays the investor USDC
//!   (`pay_income`). Rupees never leave India.
//! - The vault (nUSD, later) assigns guarantee with USDC it holds itself (`mint_guaranteed`).
//! - Unaudited: deposits stop at the trust's cap (0 until set), the vault's guarantee at its own cap (0
//!   until set), and no call moves USDC out except `redeem` (to the investor, at par). There is no admin
//!   withdrawal; but a 48-hour upgrade (trust, `nila_common::upgrade`) can change anything, so every
//!   `UpgradeProposed` here is watched.
//! - Error codes start at 101, apart from fund_core's and fx_pool's, so an error from a contract it calls
//!   never reads as one of its own.

use nila_common::{bump_instance, upgrade, FundCoreClient, RolesClient, TTL_BUMP, TTL_THRESHOLD};
use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contractmeta,
    contracttype, token::TokenClient, Address, BytesN, Env,
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
    val = "Nila senior: senior USDC, deposits at the fix, exits at par, interest via a union LP"
);

#[cfg(not(feature = "demo-upgrade-delay"))]
pub const UPGRADE_DELAY: u64 = upgrade::DEFAULT_DELAY;
#[cfg(feature = "demo-upgrade-delay")]
pub const UPGRADE_DELAY: u64 = 60;

/// nIN per USDC at 7 decimals, as fx_pool's fix.
pub const RATE_ONE: i128 = 10_000_000;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 101,
    Paused = 102,
    AmountZero = 103,
    /// Not an investor the trust accepted.
    NotAccepted = 104,
    /// The deposit would take the senior USDC above the trust's cap.
    AboveCap = 105,
    /// No fix, or it ran out.
    StaleRate = 106,
    /// The LP pays less than the fix less its fee.
    BelowFix = 107,
    NotLeader = 108,
    /// Not an accepted LP of the union, or its identity is not checked (K1).
    NotLp = 109,
    VaultAlreadySet = 110,
    NoVault = 111,
    /// An investor's residency is set once.
    TagFixed = 112,
    UpgradeNotReady = 113,
    BadDelay = 114,
    Overflow = 115,
    /// fund_core does not know the union: no USDC is taken for it.
    UnknownUnion = 116,
    /// The vault's guarantee would pass its cap.
    AboveGuaranteeCap = 117,
}

/// Where a senior investor's USDC comes from (R24, decision 005): set by the trust at acceptance.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Tag {
    Offshore = 0,
    Domestic = 1,
}

/// The trust's acceptance of a senior investor: never a member. `kyc_ref` points at the KYC record kept
/// off-chain; no personal data on-chain (C17).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Investor {
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
    pub tag: Tag,
}

/// What an investor paid in for a union's senior shares still held: the USDC and the nIN it minted.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Principal {
    pub usdc: i128,
    pub nin: i128,
}

/// fx_pool's INR/USD fix (the same fields as its `RateFix`).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fix {
    pub inr_per_usd: i128,
    pub valid_until: u64,
    pub set_by: Address,
}

/// fx_pool's acceptance of a member LP (the same fields as its `LpMember`).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpOf {
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
}

#[contractclient(name = "FxClient")]
pub trait FxTrait {
    fn rate() -> Option<Fix>;
    fn lp_member(union: Address, lp: Address) -> Option<LpOf>;
    fn id_checked(account: Address) -> bool;
    fn lp_fee() -> u32;
    fn mint_senior_nin(union: Address, investor: Address, nin: i128);
}

/// The LP's Nila account (contracts/account): it pays from its free USDC, so an LP with an open lien
/// (a Bring position) can still pay.
#[contractclient(name = "LpAccountClient")]
pub trait LpAccountTrait {
    fn send_usdc(to: Address, amount: i128);
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Usdc,
    FundCore,
    FxPool,
    Roles,
    Paused,
    /// The most senior USDC this contract takes in, in all (the trust; 0 until set).
    Cap,
    /// Senior USDC held here for investors' principal: deposits less what `redeem` paid out or wrote off.
    Total,
    /// The vault (nUSD, later), set once.
    Vault,
    /// The most guarantee the vault may assign, in USDC (the trust; 0 until set).
    GuaranteeCap,
    /// Guarantee the vault assigned so far, in USDC.
    Guaranteed,
    Investor(Address),
    /// (union, investor).
    Principal(Address, Address),
    /// USDC taken in for a union under one residency, less what was paid back or written off (R24).
    UnionUsdc(Address, Tag),
    /// USDC of a union's investors left here after a haircut (2 Nov): out of `Total`, kept for the loss
    /// cover that comes with nUSD; no call moves it yet.
    HaircutUsdc(Address),
}

#[contractevent]
pub struct AdminSet {
    pub admin: Address,
}

#[contractevent]
pub struct PausedSet {
    pub paused: bool,
}

#[contractevent]
pub struct CapSet {
    pub usdc: i128,
}

#[contractevent]
pub struct GuaranteeCapSet {
    pub usdc: i128,
}

#[contractevent]
pub struct InvestorSet {
    #[topic]
    pub investor: Address,
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
    pub tag: Tag,
}

#[contractevent]
pub struct VaultSet {
    pub vault: Address,
}

#[contractevent]
pub struct Deposited {
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
pub struct Redeemed {
    #[topic]
    pub union: Address,
    #[topic]
    pub investor: Address,
    pub usdc: i128,
    pub principal: i128,
    pub interest: i128,
    /// USDC of this exit's principal left here after a haircut.
    pub haircut: i128,
}

#[contractevent]
pub struct IncomePaid {
    #[topic]
    pub union: Address,
    #[topic]
    pub investor: Address,
    pub leader: Address,
    pub lp: Address,
    pub inr: i128,
    pub usdc: i128,
    pub cash_settled: i128,
}

#[contractevent]
pub struct Guaranteed {
    #[topic]
    pub union: Address,
    pub usdc: i128,
    pub nin: i128,
    pub inr_per_usd: i128,
}

fn get(env: &Env, k: &DataKey) -> Address {
    env.storage().instance().get(k).unwrap()
}

fn i_get(env: &Env, k: &DataKey) -> i128 {
    env.storage().instance().get(k).unwrap_or(0)
}

fn p_get(env: &Env, k: &DataKey) -> i128 {
    env.storage().persistent().get(k).unwrap_or(0)
}

fn p_set(env: &Env, k: &DataKey, v: i128) {
    env.storage().persistent().set(k, &v);
    bump(env, k);
}

fn bump(env: &Env, k: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(k, TTL_THRESHOLD, TTL_BUMP);
}

fn require_admin(env: &Env) -> Result<Address, Error> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotAdmin)?;
    admin.require_auth();
    Ok(admin)
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

fn fx(env: &Env) -> FxClient<'_> {
    FxClient::new(env, &get(env, &DataKey::FxPool))
}

fn fresh_fix(env: &Env) -> Result<Fix, Error> {
    let f = fx(env).rate().ok_or(Error::StaleRate)?;
    if env.ledger().timestamp() > f.valid_until || f.inr_per_usd <= 0 {
        return Err(Error::StaleRate);
    }
    Ok(f)
}

fn to_nin(usdc: i128, f: &Fix) -> Result<i128, Error> {
    let nin = usdc.checked_mul(f.inr_per_usd).ok_or(Error::Overflow)? / RATE_ONE;
    if nin <= 0 {
        return Err(Error::AmountZero);
    }
    Ok(nin)
}

fn load_investor(env: &Env, who: &Address) -> Option<Investor> {
    env.storage()
        .persistent()
        .get(&DataKey::Investor(who.clone()))
}

fn load_principal(env: &Env, k: &DataKey) -> Principal {
    env.storage()
        .persistent()
        .get(k)
        .unwrap_or(Principal { usdc: 0, nin: 0 })
}

#[contract]
pub struct SeniorContract;

#[contractimpl]
impl SeniorContract {
    pub fn __constructor(
        env: Env,
        admin: Address,
        usdc: Address,
        fund_core: Address,
        fx_pool: Address,
        roles: Address,
    ) {
        let s = env.storage().instance();
        s.set(&DataKey::Admin, &admin);
        s.set(&DataKey::Usdc, &usdc);
        s.set(&DataKey::FundCore, &fund_core);
        s.set(&DataKey::FxPool, &fx_pool);
        s.set(&DataKey::Roles, &roles);
        upgrade::init(&env, UPGRADE_DELAY);
        bump_instance(&env);
    }

    // ---------- the trust (admin) ----------

    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        AdminSet { admin: new_admin }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Stops deposits, exits, income payouts and the vault's guarantee.
    pub fn set_paused(env: Env, paused: bool) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &paused);
        PausedSet { paused }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The most senior USDC this contract takes in, in all (unaudited: 0 = no deposits until the trust sets it).
    pub fn set_cap(env: Env, usdc: i128) -> Result<(), Error> {
        require_admin(&env)?;
        if usdc < 0 {
            return Err(Error::AmountZero);
        }
        env.storage().instance().set(&DataKey::Cap, &usdc);
        CapSet { usdc }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The most guarantee the vault may assign, in USDC (0 = none until the trust sets it, after nUSD's audit).
    pub fn set_guarantee_cap(env: Env, usdc: i128) -> Result<(), Error> {
        require_admin(&env)?;
        if usdc < 0 {
            return Err(Error::AmountZero);
        }
        env.storage().instance().set(&DataKey::GuaranteeCap, &usdc);
        GuaranteeCapSet { usdc }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The trust accepts or drops a senior investor (KYC off-chain); the residency is set once.
    pub fn set_investor(
        env: Env,
        investor: Address,
        accepted: bool,
        kyc_ref: BytesN<32>,
        tag: Tag,
    ) -> Result<(), Error> {
        require_admin(&env)?;
        if load_investor(&env, &investor).is_some_and(|i| i.tag != tag) {
            return Err(Error::TagFixed);
        }
        let k = DataKey::Investor(investor.clone());
        env.storage().persistent().set(
            &k,
            &Investor {
                accepted,
                kyc_ref: kyc_ref.clone(),
                tag,
            },
        );
        bump(&env, &k);
        InvestorSet {
            investor,
            accepted,
            kyc_ref,
            tag,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The vault (nUSD, later), set once.
    pub fn set_vault(env: Env, vault: Address) -> Result<(), Error> {
        require_admin(&env)?;
        if env.storage().instance().has(&DataKey::Vault) {
            return Err(Error::VaultAlreadySet);
        }
        env.storage().instance().set(&DataKey::Vault, &vault);
        VaultSet { vault }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- investors ----------

    /// An accepted investor puts `usdc` behind a union's vouchers: it stays here, and the same value in nIN
    /// at the day's fix goes into the union's senior pool in their name. Returns the nIN.
    pub fn deposit(env: Env, investor: Address, union: Address, usdc: i128) -> Result<i128, Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        if usdc <= 0 {
            return Err(Error::AmountZero);
        }
        let inv = load_investor(&env, &investor)
            .filter(|i| i.accepted)
            .ok_or(Error::NotAccepted)?;
        let total = i_get(&env, &DataKey::Total)
            .checked_add(usdc)
            .ok_or(Error::Overflow)?;
        if total > i_get(&env, &DataKey::Cap) {
            return Err(Error::AboveCap);
        }
        if !FundCoreClient::new(&env, &get(&env, &DataKey::FundCore)).union_exists(&union) {
            return Err(Error::UnknownUnion);
        }
        let f = fresh_fix(&env)?;
        let nin = to_nin(usdc, &f)?;
        TokenClient::new(&env, &get(&env, &DataKey::Usdc)).transfer(
            &investor,
            env.current_contract_address(),
            &usdc,
        );
        env.storage().instance().set(&DataKey::Total, &total);
        let pk = DataKey::Principal(union.clone(), investor.clone());
        let mut p = load_principal(&env, &pk);
        p.usdc += usdc;
        p.nin += nin;
        env.storage().persistent().set(&pk, &p);
        bump(&env, &pk);
        let uk = DataKey::UnionUsdc(union.clone(), inv.tag);
        p_set(&env, &uk, p_get(&env, &uk) + usdc);
        fx(&env).mint_senior_nin(&union, &investor, &nin);
        Deposited {
            union,
            investor,
            tag: inv.tag,
            usdc,
            nin,
            inr_per_usd: f.inr_per_usd,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(nin)
    }

    /// After fund_core's 14-day unbond: up to `max_shares` (0: all claimable). fund_core burns the principal
    /// and keeps the interest owed to the investor; the USDC paid in for that principal comes back at par,
    /// never more than recorded. Returns the USDC paid.
    pub fn redeem(
        env: Env,
        investor: Address,
        union: Address,
        max_shares: i128,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        investor.require_auth();
        let (principal, interest, cost) = FundCoreClient::new(&env, &get(&env, &DataKey::FundCore))
            .claim_senior(&union, &investor, &max_shares);
        let pk = DataKey::Principal(union.clone(), investor.clone());
        let mut p = load_principal(&env, &pk);
        let (mut usdc, mut haircut) = (0, 0);
        if p.nin > 0 {
            let cost = core::cmp::min(cost, p.nin);
            let off = p.usdc * cost / p.nin;
            // A haircut (principal below cost) pays less; the rest stays here, counted apart.
            usdc = core::cmp::min(p.usdc * core::cmp::min(principal, cost) / p.nin, off);
            haircut = off - usdc;
            p.usdc -= off;
            p.nin -= cost;
            if p.nin == 0 {
                env.storage().persistent().remove(&pk);
            } else {
                env.storage().persistent().set(&pk, &p);
                bump(&env, &pk);
            }
        }
        if haircut > 0 {
            // Out of `Total` (it no longer counts against the cap) into the union's haircut figure: it stays in
            // this contract, and no call moves it until the loss cover comes with nUSD.
            let hk = DataKey::HaircutUsdc(union.clone());
            p_set(&env, &hk, p_get(&env, &hk) + haircut);
        }
        if usdc + haircut > 0 {
            let tag = load_investor(&env, &investor).map_or(Tag::Offshore, |i| i.tag);
            let uk = DataKey::UnionUsdc(union.clone(), tag);
            p_set(&env, &uk, p_get(&env, &uk) - usdc - haircut);
            let total = i_get(&env, &DataKey::Total) - usdc - haircut;
            env.storage().instance().set(&DataKey::Total, &total);
        }
        if usdc > 0 {
            TokenClient::new(&env, &get(&env, &DataKey::Usdc)).transfer(
                &env.current_contract_address(),
                &investor,
                &usdc,
            );
        }
        Redeemed {
            union,
            investor,
            usdc,
            principal,
            interest,
            haircut,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(usdc)
    }

    /// An investor's owed interest (rupees, from their exit) paid in USDC by a member LP of the union: the LP
    /// takes `inr` rupees at the office and pays `usdc` straight to the investor, at least the fix less the
    /// LP fee. A leader of the union (who hands over the notes) and the LP sign. fund_core burns the nIN and
    /// books the rupees off cash at hand. The LP's Nila account pays from its free USDC (`send_usdc`), so an
    /// open lien does not stop it. No USDC of this contract moves. Returns what cash at hand covered.
    #[allow(clippy::too_many_arguments)]
    pub fn pay_income(
        env: Env,
        union: Address,
        investor: Address,
        leader: Address,
        lp: Address,
        inr: i128,
        usdc: i128,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        leader.require_auth();
        if !RolesClient::new(&env, &get(&env, &DataKey::Roles)).is_leader(&union, &leader) {
            return Err(Error::NotLeader);
        }
        lp.require_auth();
        let fx = fx(&env);
        if !fx.lp_member(&union, &lp).is_some_and(|m| m.accepted) || !fx.id_checked(&lp) {
            return Err(Error::NotLp);
        }
        if inr <= 0 || usdc <= 0 {
            return Err(Error::AmountZero);
        }
        let f = fresh_fix(&env)?;
        let fair = inr.checked_mul(RATE_ONE).ok_or(Error::Overflow)? / f.inr_per_usd;
        if usdc < fair * i128::from(10_000 - fx.lp_fee().min(10_000)) / 10_000 {
            return Err(Error::BelowFix);
        }
        let settled = FundCoreClient::new(&env, &get(&env, &DataKey::FundCore))
            .take_senior_income(&union, &investor, &inr);
        LpAccountClient::new(&env, &lp).send_usdc(&investor, &usdc);
        IncomePaid {
            union,
            investor,
            leader,
            lp,
            inr,
            usdc,
            cash_settled: settled,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(settled)
    }

    /// The vault assigns guarantee to a union with USDC it holds itself: senior nIN at the fix in the vault's
    /// name; no USDC moves here. Returns the nIN.
    pub fn mint_guaranteed(env: Env, union: Address, usdc: i128) -> Result<i128, Error> {
        require_not_paused(&env)?;
        let vault: Address = env
            .storage()
            .instance()
            .get(&DataKey::Vault)
            .ok_or(Error::NoVault)?;
        vault.require_auth();
        if usdc <= 0 {
            return Err(Error::AmountZero);
        }
        let done = i_get(&env, &DataKey::Guaranteed)
            .checked_add(usdc)
            .ok_or(Error::Overflow)?;
        if done > i_get(&env, &DataKey::GuaranteeCap) {
            return Err(Error::AboveGuaranteeCap);
        }
        let f = fresh_fix(&env)?;
        let nin = to_nin(usdc, &f)?;
        env.storage().instance().set(&DataKey::Guaranteed, &done);
        fx(&env).mint_senior_nin(&union, &vault, &nin);
        Guaranteed {
            union,
            usdc,
            nin,
            inr_per_usd: f.inr_per_usd,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(nin)
    }

    // ---------- views ----------

    pub fn investor(env: Env, investor: Address) -> Option<Investor> {
        load_investor(&env, &investor)
    }

    pub fn principal(env: Env, union: Address, investor: Address) -> Principal {
        load_principal(&env, &DataKey::Principal(union, investor))
    }

    pub fn union_usdc(env: Env, union: Address, tag: Tag) -> i128 {
        p_get(&env, &DataKey::UnionUsdc(union, tag))
    }

    pub fn haircut_usdc(env: Env, union: Address) -> i128 {
        p_get(&env, &DataKey::HaircutUsdc(union))
    }

    pub fn guarantee_cap(env: Env) -> i128 {
        i_get(&env, &DataKey::GuaranteeCap)
    }

    pub fn guaranteed(env: Env) -> i128 {
        i_get(&env, &DataKey::Guaranteed)
    }

    pub fn total(env: Env) -> i128 {
        i_get(&env, &DataKey::Total)
    }

    pub fn cap(env: Env) -> i128 {
        i_get(&env, &DataKey::Cap)
    }

    pub fn vault(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Vault)
    }

    pub fn admin(env: Env) -> Address {
        get(&env, &DataKey::Admin)
    }

    pub fn paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
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
