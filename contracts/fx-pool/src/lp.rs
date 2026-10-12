//! LP offers (C5b, Nov 7; executive decision 12 Oct): member LPs swap cash and USDC for the union, for a fee.
//!
//! - An **LP** is a member of the union with a land title, accepted by the trust's anchor after
//!   SEP-12 (R20). She never lends or guarantees.
//! - **Bring** (cash short, R18, R21): the leader posts the rupees needed. An LP accepts at the
//!   quote (the rate is fixed at accept), brings the rupees, and on the leader's scan the union pays
//!   her the USDC plus her fee. The USDC comes from the union's pool: first LP-collected USDC, then
//!   senior USDC above the **voucher floor**, domestic before offshore (D1, R24).
//! - **Collect** (spare cash, R19, R22): the leader posts the spare rupees. An LP accepts and signs a
//!   USDC approve that lapses at the offer's expiry (R23). On the scan the union takes her USDC
//!   (less her fee) and she takes the rupees, which leave the union's cash at hand.
//! - Each accepted offer stores the anchor's SEP-24 transaction id and the SEP-38 quote id (R23).
//! - A no-show costs nothing: the offer expires and any reservation or approve lapses.

use super::*;
use soroban_sdk::String;

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OfferKind {
    Bring = 0,
    Collect = 1,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OfferState {
    Open = 0,
    Accepted = 1,
    Done = 2,
    Cancelled = 3,
    Expired = 4,
}

/// The anchor's acceptance of a member as LP of one union (R20). KYC stays at the anchor.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpMember {
    pub accepted: bool,
    pub kyc_ref: BytesN<32>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Offer {
    pub union: Address,
    pub kind: OfferKind,
    /// Rupees, as nIN units (7 decimals).
    pub inr: i128,
    pub expires: u64,
    pub state: OfferState,
    pub lp: Option<Address>,
    /// USDC paid to her (Bring) or taken from her (Collect), fee included, fixed at accept.
    pub usdc: i128,
    pub inr_per_usd: i128,
    pub sep24_id: String,
    pub quote_id: String,
    /// Where a Bring's USDC is reserved from (R24).
    pub from_lp: i128,
    pub from_domestic: i128,
    pub from_offshore: i128,
}

#[contractevent]
pub struct AnchorSet {
    pub anchor: Address,
}

#[contractevent]
pub struct LpMemberSet {
    #[topic]
    pub union: Address,
    #[topic]
    pub lp: Address,
    pub accepted: bool,
}

#[contractevent]
pub struct LpFeeSet {
    pub fee_bp: u32,
}

#[contractevent]
pub struct VoucherFloorSet {
    #[topic]
    pub union: Address,
    pub usdc: i128,
}

#[contractevent]
pub struct OfferPosted {
    #[topic]
    pub union: Address,
    #[topic]
    pub offer_id: u64,
    pub kind: OfferKind,
    pub inr: i128,
    pub expires: u64,
}

#[contractevent]
pub struct OfferAccepted {
    #[topic]
    pub union: Address,
    #[topic]
    pub offer_id: u64,
    pub lp: Address,
    pub usdc: i128,
    pub inr_per_usd: i128,
    pub sep24_id: String,
    pub quote_id: String,
}

#[contractevent]
pub struct OfferConfirmed {
    #[topic]
    pub union: Address,
    #[topic]
    pub offer_id: u64,
    pub kind: OfferKind,
    pub lp: Address,
    pub inr: i128,
    pub usdc: i128,
    pub from_lp: i128,
    pub from_domestic: i128,
    pub from_offshore: i128,
}

#[contractevent]
pub struct OfferClosed {
    #[topic]
    pub union: Address,
    #[topic]
    pub offer_id: u64,
    pub state: OfferState,
}

/// At most this many open offers per union.
pub const MAX_OPEN_OFFERS: u32 = 64;
/// Average ledger close, for the Collect approve's expiry ledger.
const LEDGER_SECONDS: u64 = 5;

fn p_get(env: &Env, k: &DataKey) -> i128 {
    env.storage().persistent().get(k).unwrap_or(0)
}

fn p_set(env: &Env, k: &DataKey, v: i128) {
    env.storage().persistent().set(k, &v);
    bump(env, k);
}

fn voucher_floor(env: &Env, union: &Address) -> i128 {
    p_get(env, &DataKey::VoucherFloor(union.clone()))
}

fn senior_total(env: &Env, union: &Address) -> i128 {
    p_get(env, &DataKey::SeniorUsdc(union.clone(), Tag::Domestic))
        + p_get(env, &DataKey::SeniorUsdc(union.clone(), Tag::Offshore))
}

fn load_offer(env: &Env, id: u64) -> Result<Offer, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Offer(id))
        .ok_or(Error::NotFound)
}

fn save_offer(env: &Env, id: u64, o: &Offer) {
    let k = DataKey::Offer(id);
    env.storage().persistent().set(&k, o);
    bump(env, &k);
}

fn offers_of(env: &Env, union: &Address) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::OffersOf(union.clone()))
        .unwrap_or(vec![env])
}

fn drop_open(env: &Env, union: &Address, id: u64) {
    let mut ids = offers_of(env, union);
    if let Some(i) = ids.first_index_of(id) {
        ids.remove(i);
    }
    let k = DataKey::OffersOf(union.clone());
    env.storage().persistent().set(&k, &ids);
    bump(env, &k);
}

/// Hands a Bring's reserved USDC back to the pool it came from.
fn release(env: &Env, o: &Offer) {
    if o.kind != OfferKind::Bring || o.state != OfferState::Accepted {
        return;
    }
    let u = &o.union;
    let lk = DataKey::LpUsdc(u.clone());
    p_set(env, &lk, p_get(env, &lk) + o.from_lp);
    let dk = DataKey::SeniorUsdc(u.clone(), Tag::Domestic);
    p_set(env, &dk, p_get(env, &dk) + o.from_domestic);
    let ok = DataKey::SeniorUsdc(u.clone(), Tag::Offshore);
    p_set(env, &ok, p_get(env, &ok) + o.from_offshore);
}

fn close(env: &Env, id: u64, o: &mut Offer, state: OfferState) {
    release(env, o);
    o.state = state;
    save_offer(env, id, o);
    drop_open(env, &o.union, id);
    OfferClosed {
        union: o.union.clone(),
        offer_id: id,
        state,
    }
    .publish(env);
}

/// USDC paid out today (temporary storage: it only matters for the day).
fn day_out(env: &Env) -> i128 {
    env.storage()
        .temporary()
        .get(&DataKey::DayOut(env.ledger().timestamp() / DAY))
        .unwrap_or(0)
}

/// The daily cap on USDC out (R76, C15): 2,500 USDC at launch.
fn check_day_out(env: &Env, usdc: i128) -> Result<(), Error> {
    let cap: i128 = env
        .storage()
        .instance()
        .get(&DataKey::DailyCap)
        .unwrap_or(0);
    if day_out(env) + usdc > cap {
        return Err(Error::CapExceeded);
    }
    Ok(())
}

fn is_lp(env: &Env, union: &Address, lp: &Address) -> bool {
    env.storage()
        .persistent()
        .get::<_, LpMember>(&DataKey::LpOf(union.clone(), lp.clone()))
        .map(|m| m.accepted)
        .unwrap_or(false)
}

#[contractimpl]
impl FxPoolContract {
    // ---------- set-up (admin, anchor) ----------

    /// The trust's anchor account, which accepts LPs after SEP-12.
    pub fn set_anchor(env: Env, anchor: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Anchor, &anchor);
        AnchorSet { anchor }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The anchor accepts or drops a member as LP of her union (R20): she must hold a title
    /// the union minted.
    pub fn set_lp(
        env: Env,
        union: Address,
        lp: Address,
        accepted: bool,
        kyc_ref: BytesN<32>,
    ) -> Result<(), Error> {
        let anchor: Address = env
            .storage()
            .instance()
            .get(&DataKey::Anchor)
            .ok_or(Error::NotAnchor)?;
        anchor.require_auth();
        if accepted
            && !LandTitleClient::new(&env, &get(&env, &DataKey::LandTitle)).holds_title(&union, &lp)
        {
            return Err(Error::NotMember);
        }
        let k = DataKey::LpOf(union.clone(), lp.clone());
        env.storage()
            .persistent()
            .set(&k, &LpMember { accepted, kyc_ref });
        bump(&env, &k);
        LpMemberSet {
            union,
            lp,
            accepted,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The LP's fee on each swap, at most 5%.
    pub fn set_lp_fee(env: Env, fee_bp: u32) -> Result<(), Error> {
        require_admin(&env)?;
        if fee_bp > 500 {
            return Err(Error::BadRate);
        }
        env.storage().instance().set(&DataKey::LpFeeBp, &fee_bp);
        LpFeeSet { fee_bp }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// Senior USDC kept back for a union's vouchers; a Bring never takes it below this.
    pub fn set_voucher_floor(env: Env, union: Address, usdc: i128) -> Result<(), Error> {
        require_admin(&env)?;
        if usdc < 0 {
            return Err(Error::AmountZero);
        }
        p_set(&env, &DataKey::VoucherFloor(union.clone()), usdc);
        VoucherFloorSet { union, usdc }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    // ---------- offers ----------

    /// L11 #short / A7 (Bring) or C4 (Collect): a leader posts rupees, at most 7 days out.
    pub fn post_offer(
        env: Env,
        leader: Address,
        union: Address,
        kind: OfferKind,
        inr: i128,
        expires: u64,
    ) -> Result<u64, Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &leader)?;
        check_amount(&env, inr)?;
        let now = env.ledger().timestamp();
        if expires <= now || expires > now + MAX_CASH_OUT_TERM {
            return Err(Error::BadExpiry);
        }
        let mut ids = offers_of(&env, &union);
        if ids.len() >= MAX_OPEN_OFFERS {
            return Err(Error::TooManyOffers);
        }
        let id = env
            .storage()
            .instance()
            .get(&DataKey::NextOffer)
            .unwrap_or(0u64)
            + 1;
        env.storage().instance().set(&DataKey::NextOffer, &id);
        let empty = String::from_str(&env, "");
        save_offer(
            &env,
            id,
            &Offer {
                union: union.clone(),
                kind,
                inr,
                expires,
                state: OfferState::Open,
                lp: None,
                usdc: 0,
                inr_per_usd: 0,
                sep24_id: empty.clone(),
                quote_id: empty,
                from_lp: 0,
                from_domestic: 0,
                from_offshore: 0,
            },
        );
        ids.push_back(id);
        let k = DataKey::OffersOf(union.clone());
        env.storage().persistent().set(&k, &ids);
        bump(&env, &k);
        OfferPosted {
            union,
            offer_id: id,
            kind,
            inr,
            expires,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(id)
    }

    /// A2: an LP of the union accepts at the attester's fix (the rate is fixed now). A Bring
    /// reserves the union's USDC; a Collect has her approve her USDC until the offer expires.
    pub fn accept(
        env: Env,
        lp: Address,
        offer_id: u64,
        sep24_id: String,
        quote_id: String,
    ) -> Result<i128, Error> {
        require_not_paused(&env)?;
        lp.require_auth();
        let mut o = load_offer(&env, offer_id)?;
        let now = env.ledger().timestamp();
        if o.state != OfferState::Open || now >= o.expires {
            return Err(Error::OfferState);
        }
        if !is_lp(&env, &o.union, &lp) {
            return Err(Error::NotLp);
        }
        let rate: RateFix = env
            .storage()
            .instance()
            .get(&DataKey::Rate)
            .ok_or(Error::StaleOracle)?;
        if now > rate.valid_until {
            return Err(Error::StaleOracle);
        }
        let base = o
            .inr
            .checked_mul(RATE_ONE)
            .map(|v| v / rate.inr_per_usd)
            .ok_or(Error::CapExceeded)?;
        let fee_bp: u32 = env
            .storage()
            .instance()
            .get(&DataKey::LpFeeBp)
            .unwrap_or(DEFAULT_LP_FEE_BP);
        let fee = base * i128::from(fee_bp) / 10_000;
        let usdc_addr: Address = get(&env, &DataKey::Usdc);
        match o.kind {
            OfferKind::Bring => {
                let usdc = base + fee;
                // LP-collected USDC first, then senior above the floor, domestic before offshore.
                let u = o.union.clone();
                let lk = DataKey::LpUsdc(u.clone());
                let from_lp = core::cmp::min(usdc, p_get(&env, &lk));
                // The floor is the admin's, but never below the USDC value of senior money
                // fund_core holds back for unclaimed voucher hours.
                let held = FundCoreClient::new(&env, &core_addr(&env)?).senior_earmark(&u);
                let held_usdc = held * RATE_ONE / rate.inr_per_usd;
                let floor = core::cmp::max(voucher_floor(&env, &u), held_usdc);
                let senior_free = core::cmp::max(0, senior_total(&env, &u) - floor);
                check_day_out(&env, usdc)?;
                let need = usdc - from_lp;
                if need > senior_free {
                    return Err(if senior_total(&env, &u) >= need {
                        Error::BelowVoucherFloor
                    } else {
                        Error::NoUsdc
                    });
                }
                let dk = DataKey::SeniorUsdc(u.clone(), Tag::Domestic);
                let from_domestic = core::cmp::min(need, p_get(&env, &dk));
                let from_offshore = need - from_domestic;
                let ok = DataKey::SeniorUsdc(u, Tag::Offshore);
                p_set(&env, &lk, p_get(&env, &lk) - from_lp);
                p_set(&env, &dk, p_get(&env, &dk) - from_domestic);
                p_set(&env, &ok, p_get(&env, &ok) - from_offshore);
                o.usdc = usdc;
                o.from_lp = from_lp;
                o.from_domestic = from_domestic;
                o.from_offshore = from_offshore;
            }
            OfferKind::Collect => {
                let usdc = base - fee;
                let ledgers = ((o.expires - now) / LEDGER_SECONDS + 1) as u32;
                TokenClient::new(&env, &usdc_addr).approve(
                    &lp,
                    &env.current_contract_address(),
                    &usdc,
                    &(env.ledger().sequence() + ledgers),
                );
                o.usdc = usdc;
            }
        }
        o.state = OfferState::Accepted;
        o.lp = Some(lp.clone());
        o.inr_per_usd = rate.inr_per_usd;
        o.sep24_id = sep24_id.clone();
        o.quote_id = quote_id.clone();
        save_offer(&env, offer_id, &o);
        OfferAccepted {
            union: o.union.clone(),
            offer_id,
            lp,
            usdc: o.usdc,
            inr_per_usd: rate.inr_per_usd,
            sep24_id,
            quote_id,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(o.usdc)
    }

    /// A1b / A7b: the leader scanned and counted. Bring pays her the USDC; Collect takes her USDC
    /// into the union's pool and the rupees leave its cash at hand.
    pub fn confirm(env: Env, leader: Address, offer_id: u64) -> Result<i128, Error> {
        require_not_paused(&env)?;
        let mut o = load_offer(&env, offer_id)?;
        require_leader(&env, &o.union, &leader)?;
        if o.state != OfferState::Accepted || env.ledger().timestamp() >= o.expires {
            return Err(Error::OfferState);
        }
        let lp = o.lp.clone().ok_or(Error::OfferState)?;
        let me = env.current_contract_address();
        let usdc = TokenClient::new(&env, &get(&env, &DataKey::Usdc));
        match o.kind {
            OfferKind::Bring => {
                check_day_out(&env, o.usdc)?;
                let k = DataKey::DayOut(env.ledger().timestamp() / DAY);
                env.storage().temporary().set(&k, &(day_out(&env) + o.usdc));
                env.storage().temporary().extend_ttl(
                    &k,
                    nila_common::DAY_IN_LEDGERS,
                    nila_common::DAY_IN_LEDGERS * 2,
                );
                usdc.transfer(&me, &lp, &o.usdc)
            }
            OfferKind::Collect => {
                usdc.transfer_from(&me, &lp, &me, &o.usdc);
                let lk = DataKey::LpUsdc(o.union.clone());
                p_set(&env, &lk, p_get(&env, &lk) + o.usdc);
                settle(&env, &o.union, o.inr);
            }
        }
        o.state = OfferState::Done;
        save_offer(&env, offer_id, &o);
        drop_open(&env, &o.union, offer_id);
        OfferConfirmed {
            union: o.union.clone(),
            offer_id,
            kind: o.kind,
            lp,
            inr: o.inr,
            usdc: o.usdc,
            from_lp: o.from_lp,
            from_domestic: o.from_domestic,
            from_offshore: o.from_offshore,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(o.usdc)
    }

    /// The leader withdraws an open or accepted offer; a Bring's reservation goes back.
    pub fn cancel_offer(env: Env, leader: Address, offer_id: u64) -> Result<(), Error> {
        let mut o = load_offer(&env, offer_id)?;
        require_leader(&env, &o.union, &leader)?;
        if o.state != OfferState::Open && o.state != OfferState::Accepted {
            return Err(Error::OfferState);
        }
        close(&env, offer_id, &mut o, OfferState::Cancelled);
        bump_instance(&env);
        Ok(())
    }

    /// Anyone, after expiry: a no-show costs nothing; the reservation or approve lapses.
    pub fn expire_offer(env: Env, offer_id: u64) -> Result<(), Error> {
        let mut o = load_offer(&env, offer_id)?;
        if o.state != OfferState::Open && o.state != OfferState::Accepted {
            return Err(Error::OfferState);
        }
        if env.ledger().timestamp() < o.expires {
            return Err(Error::NotExpired);
        }
        close(&env, offer_id, &mut o, OfferState::Expired);
        bump_instance(&env);
        Ok(())
    }

    // ---------- views ----------

    pub fn offer(env: Env, offer_id: u64) -> Option<Offer> {
        env.storage().persistent().get(&DataKey::Offer(offer_id))
    }

    pub fn offers_of(env: Env, union: Address) -> Vec<u64> {
        offers_of(&env, &union)
    }

    pub fn lp_member(env: Env, union: Address, lp: Address) -> Option<LpMember> {
        env.storage().persistent().get(&DataKey::LpOf(union, lp))
    }

    /// USDC LPs brought into the union (domestic).
    pub fn lp_usdc(env: Env, union: Address) -> i128 {
        p_get(&env, &DataKey::LpUsdc(union))
    }

    pub fn voucher_floor(env: Env, union: Address) -> i128 {
        voucher_floor(&env, &union)
    }

    pub fn anchor(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Anchor)
    }
}
