//! LP offers as a pledge (executive decision 30 Oct, replaces the C5b swap): an LP is a member who
//! owns USDC and holds the union's cash for a while, for a fee. Her USDC never changes owner.
//!
//! - An **LP** is a member of the union with a land title, accepted by the trust's anchor (K1, R20),
//!   on a Nila account (nila-account), which carries the lien.
//! - **Collect** (the union has spare cash): the leader posts the rupees. An LP accepts at the fix:
//!   her account locks USDC worth 110% of them (`lock`, refused above her free USDC). On the
//!   leader's scan she takes the rupees, which leave the union's cash at hand, and her position
//!   grows (owed rupees, locked USDC). No USDC moves.
//! - **Bring** (the union wants its cash back): only an LP whose position owes the union, at most
//!   what she owes. She brings the rupees less her 1% fee (the union's cost, covered from its
//!   treasury); on the scan they join the cash at hand (a 7-day escrow), her position shrinks and
//!   her account unlocks the USDC pro rata (all of it at the close). No pool USDC reaches her.
//! - **Call and claim:** a leader calls a position; after 7 days' notice the union's multisig
//!   claims through her account (`nila-account.claim`): USDC worth what she owes at today's fix,
//!   at most the locked, to the union's account; the rest unlocks.
//! - Each accepted offer stores the anchor's SEP-24 transaction id and the SEP-38 quote id;
//!   `accept` needs the anchor's signature over them (C9b), so the anchor records every change.
//! - A no-show costs nothing: the offer expires and a Collect's lock is released.

use super::*;
use soroban_sdk::{xdr::ToXdr, String};

/// What the anchor's swap key signs for one accept (C9b): this prefix, then the XDR of
/// (fx_pool, offer_id, lp, sep24_id, quote_id, usdc, valid_until); usdc = USDC to lock (Collect) or to unlock (Bring).
pub const ACCEPT_PREFIX: &[u8] = b"nila-lp-accept-v2";

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
    /// USDC locked in her account (Collect, at accept) or unlocked (Bring, at the scan).
    pub usdc: i128,
    pub inr_per_usd: i128,
    pub sep24_id: String,
    pub quote_id: String,
    /// Unused since the pledge (30 Oct; were a swap's USDC sources): kept so stored offers still read.
    pub from_lp: i128,
    pub from_domestic: i128,
    pub from_offshore: i128,
}

/// What an offer locks or unlocks at today's fix (C9, the anchor's SEP-38 price reads it).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Quote {
    /// Collect: USDC her account locks (110% of the rupees at the fix). Bring: the rupees' USDC
    /// value at the fix (what unlocks depends on her position: `bring_unlock`).
    pub usdc: i128,
    /// Bring: her fee in rupees (nIN units), off the cash she brings; Collect: 0.
    pub fee_inr: i128,
    pub fee_bp: u32,
    pub inr_per_usd: i128,
    /// The fix's expiry: a quote after it fails at accept with StaleOracle.
    pub valid_until: u64,
}

/// An LP's holding of the union's cash (30 Oct): rupees owed, the USDC locked in her account for
/// them, since when, and when a leader called it back.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub owed: i128,
    pub locked: i128,
    pub opened: u64,
    pub called_at: Option<u64>,
}

/// What an LP's Nila account does for fx_pool (nila-account): the lien on her own USDC.
#[allow(dead_code)]
#[soroban_sdk::contractclient(name = "LienClient")]
pub trait LienTrait {
    fn lock(env: Env, amount: i128) -> i128;
    fn unlock(env: Env, amount: i128) -> i128;
}

#[contractevent]
pub struct PositionChanged {
    #[topic]
    pub union: Address,
    #[topic]
    pub lp: Address,
    pub owed: i128,
    pub locked: i128,
}

#[contractevent]
pub struct PositionCalled {
    #[topic]
    pub union: Address,
    #[topic]
    pub lp: Address,
    pub owed: i128,
    pub claimable_from: u64,
}

#[contractevent]
pub struct PositionClaimed {
    #[topic]
    pub union: Address,
    #[topic]
    pub lp: Address,
    pub usdc: i128,
    pub inr: i128,
    pub inr_per_usd: i128,
    pub unlocked: i128,
}

#[contractevent]
pub struct SwapKeySet {
    pub key: BytesN<32>,
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

/// The anchor checked (or withdrew) an account's identity (K1, 22 Oct).
#[contractevent]
pub struct IdCheckedSet {
    #[topic]
    pub account: Address,
    pub checked: bool,
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

/// Rupees added to an open offer no LP has accepted yet (25 Oct): one offer grows instead of a new one each time.
#[contractevent]
pub struct OfferMerged {
    #[topic]
    pub union: Address,
    #[topic]
    pub offer_id: u64,
    pub added: i128,
    pub inr: i128,
    pub expires: u64,
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
/// A Collect locks 110% of the rupees' USDC at the fix (executive decision 30 Oct, Q3; below 105%
/// nothing happens by itself: a leader may call).
pub const LOCK_MARGIN_BP: i128 = 11_000;
/// Notice after a call before the union may claim (Q4).
pub const CALL_NOTICE: u64 = 7 * DAY;

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

fn close(env: &Env, id: u64, o: &mut Offer, state: OfferState) {
    // A Collect accepted but not handed over: her lock goes.
    if o.kind == OfferKind::Collect && o.state == OfferState::Accepted && o.usdc > 0 {
        if let Some(lp) = &o.lp {
            LienClient::new(env, lp).unlock(&o.usdc);
        }
    }
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

fn lp_fee_bp(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::LpFeeBp)
        .unwrap_or(DEFAULT_LP_FEE_BP)
}

/// What `inr` rupees lock (Collect) or are worth (Bring) at the attester's fix, and a Bring's
/// rupee fee (same maths in `accept` and `quote`).
fn price(env: &Env, kind: OfferKind, inr: i128) -> Result<Quote, Error> {
    let rate: RateFix = env
        .storage()
        .instance()
        .get(&DataKey::Rate)
        .ok_or(Error::StaleOracle)?;
    if env.ledger().timestamp() > rate.valid_until {
        return Err(Error::StaleOracle);
    }
    let base = inr
        .checked_mul(RATE_ONE)
        .map(|v| v / rate.inr_per_usd)
        .ok_or(Error::CapExceeded)?;
    let fee_bp = lp_fee_bp(env);
    let (usdc, fee_inr) = match kind {
        // Rounded up: the lock never sits below 110%.
        OfferKind::Collect => ((base * LOCK_MARGIN_BP + 9_999) / 10_000, 0),
        OfferKind::Bring => (base, inr * i128::from(fee_bp) / 10_000),
    };
    Ok(Quote {
        usdc,
        fee_inr,
        fee_bp,
        inr_per_usd: rate.inr_per_usd,
        valid_until: rate.valid_until,
    })
}

fn load_position(env: &Env, union: &Address, lp: &Address) -> Option<Position> {
    env.storage()
        .persistent()
        .get(&DataKey::Position(union.clone(), lp.clone()))
}

fn save_position(env: &Env, union: &Address, lp: &Address, p: &Position) {
    let k = DataKey::Position(union.clone(), lp.clone());
    let lk = DataKey::LpsOwing(union.clone());
    let mut lps: Vec<Address> = env.storage().persistent().get(&lk).unwrap_or(vec![env]);
    let at = lps.first_index_of(lp);
    if p.owed <= 0 {
        env.storage().persistent().remove(&k);
        if let Some(i) = at {
            lps.remove(i);
        }
    } else {
        env.storage().persistent().set(&k, p);
        bump(env, &k);
        if at.is_none() {
            lps.push_back(lp.clone());
        }
    }
    env.storage().persistent().set(&lk, &lps);
    bump(env, &lk);
    PositionChanged {
        union: union.clone(),
        lp: lp.clone(),
        owed: core::cmp::max(p.owed, 0),
        locked: if p.owed <= 0 { 0 } else { p.locked },
    }
    .publish(env);
}

fn add_lp_owed(env: &Env, union: &Address, d: i128) {
    let k = DataKey::LpOwed(union.clone());
    p_set(env, &k, p_get(env, &k) + d);
}

/// USDC a Bring of `inr` unlocks: pro rata, all at the close.
fn unlock_for(p: &Position, inr: i128) -> i128 {
    if inr >= p.owed {
        p.locked
    } else {
        p.locked * inr / p.owed
    }
}

/// USDC the union may claim from a called position after the notice: what she owes at today's
/// fix (rounded up), at most the locked; 0 before.
fn claimable(env: &Env, p: &Position) -> Result<i128, Error> {
    match p.called_at {
        Some(t) if env.ledger().timestamp() >= t + CALL_NOTICE => {}
        _ => return Ok(0),
    }
    let rate: RateFix = env
        .storage()
        .instance()
        .get(&DataKey::Rate)
        .ok_or(Error::StaleOracle)?;
    if env.ledger().timestamp() > rate.valid_until {
        return Err(Error::StaleOracle);
    }
    let need = (p.owed * RATE_ONE + rate.inr_per_usd - 1) / rate.inr_per_usd;
    Ok(core::cmp::min(need, p.locked))
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
            && !env
                .storage()
                .persistent()
                .has(&DataKey::IdChecked(lp.clone()))
        {
            return Err(Error::NotIdChecked);
        }
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

    /// C9b: the anchor's swap key (ed25519), apart from the anchor account that accepts LPs.
    /// Rotating it voids every signature not yet used.
    pub fn set_swap_key(env: Env, key: BytesN<32>) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::SwapKey, &key);
        SwapKeySet { key }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    /// The LP's fee on each swap, at most 5%.
    /// K1 (22 Oct): the anchor marks an account's identity as checked (or withdraws it), with the reference to its
    /// SEP-12 record. An LP needs it before `set_lp`; the app unlocks a leader's USDC (A9) on it.
    pub fn set_id_checked(
        env: Env,
        account: Address,
        checked: bool,
        kyc_ref: BytesN<32>,
    ) -> Result<(), Error> {
        let anchor: Address = env
            .storage()
            .instance()
            .get(&DataKey::Anchor)
            .ok_or(Error::NotAnchor)?;
        anchor.require_auth();
        let k = DataKey::IdChecked(account.clone());
        if checked {
            env.storage().persistent().set(&k, &kyc_ref);
            bump(&env, &k);
        } else {
            env.storage().persistent().remove(&k);
        }
        IdCheckedSet { account, checked }.publish(&env);
        bump_instance(&env);
        Ok(())
    }

    pub fn id_checked(env: Env, account: Address) -> bool {
        env.storage().persistent().has(&DataKey::IdChecked(account))
    }

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
        let id = open_offer(&env, &union, kind, inr, expires)?;
        bump_instance(&env);
        Ok(id)
    }

    /// A2: an LP of the union accepts at the attester's fix (the rate is fixed now), with the anchor's
    /// signed quote (C9b): NotAnchor without a swap key, QuoteExpired after `valid_until`,
    /// QuoteChanged when today's price no longer gives `usdc`; a bad signature traps (host error,
    /// no contract error code). A Bring
    /// reserves the union's USDC; a Collect has her approve her USDC until the offer expires.
    #[allow(clippy::too_many_arguments)]
    pub fn accept(
        env: Env,
        lp: Address,
        offer_id: u64,
        sep24_id: String,
        quote_id: String,
        usdc: i128,
        valid_until: u64,
        sig: BytesN<64>,
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
        // C16: only a swap the anchor recorded, at the USDC it quoted, before the quote runs out.
        let key: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::SwapKey)
            .ok_or(Error::NotAnchor)?;
        if now > valid_until {
            return Err(Error::QuoteExpired);
        }
        let mut msg = Bytes::from_slice(&env, ACCEPT_PREFIX);
        msg.append(
            &(
                env.current_contract_address(),
                offer_id,
                lp.clone(),
                sep24_id.clone(),
                quote_id.clone(),
                usdc,
                valid_until,
            )
                .to_xdr(&env),
        );
        env.crypto().ed25519_verify(&key, &msg, &sig);
        let q = price(&env, o.kind, o.inr)?;
        match o.kind {
            OfferKind::Collect => {
                if q.usdc != usdc {
                    return Err(Error::QuoteChanged);
                }
                // Her account locks it; refused above her free USDC. Nothing leaves her account.
                LienClient::new(&env, &lp).lock(&usdc);
            }
            OfferKind::Bring => {
                // Only an LP who holds the union's cash brings it back, at most what she owes.
                let p = load_position(&env, &o.union, &lp).ok_or(Error::NoPosition)?;
                if p.owed < o.inr {
                    return Err(Error::NoPosition);
                }
                if unlock_for(&p, o.inr) != usdc {
                    return Err(Error::QuoteChanged);
                }
            }
        }
        o.usdc = usdc;
        o.state = OfferState::Accepted;
        o.lp = Some(lp.clone());
        o.inr_per_usd = q.inr_per_usd;
        o.sep24_id = sep24_id.clone();
        o.quote_id = quote_id.clone();
        save_offer(&env, offer_id, &o);
        OfferAccepted {
            union: o.union.clone(),
            offer_id,
            lp,
            usdc: o.usdc,
            inr_per_usd: q.inr_per_usd,
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
        let mut p = load_position(&env, &o.union, &lp).unwrap_or(Position {
            owed: 0,
            locked: 0,
            opened: env.ledger().timestamp(),
            called_at: None,
        });
        match o.kind {
            OfferKind::Collect => {
                // She takes the rupees: they leave the cash at hand; her lock (made at accept) stays.
                settle(&env, &o.union, o.inr);
                p.owed += o.inr;
                p.locked += o.usdc;
                add_lp_owed(&env, &o.union, o.inr);
            }
            OfferKind::Bring => {
                if p.owed < o.inr {
                    return Err(Error::NoPosition);
                }
                // She brings the rupees less her fee; the fee is the union's cost, from its treasury.
                let fee = price(&env, OfferKind::Bring, o.inr)?.fee_inr;
                let cash = o.inr - fee;
                check_room(&env, &o.union, cash)?;
                open_escrow(&env, &o.union, &lp, Purpose::Bring, cash);
                if fee > 0 {
                    FundCoreClient::new(&env, &core_addr(&env)?)
                        .cover_expired_escrow(&o.union, &fee);
                }
                let unlock = unlock_for(&p, o.inr);
                p.owed -= o.inr;
                p.locked -= unlock;
                add_lp_owed(&env, &o.union, -o.inr);
                o.usdc = unlock;
                if unlock > 0 {
                    LienClient::new(&env, &lp).unlock(&unlock);
                }
            }
        }
        save_position(&env, &o.union, &lp, &p);
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

    /// A leader asks an LP for the union's cash back: after 7 days' notice the union may claim.
    pub fn call_position(
        env: Env,
        leader: Address,
        union: Address,
        lp: Address,
    ) -> Result<u64, Error> {
        require_not_paused(&env)?;
        require_leader(&env, &union, &leader)?;
        let mut p = load_position(&env, &union, &lp).ok_or(Error::NoPosition)?;
        let now = env.ledger().timestamp();
        if p.called_at.is_none() {
            p.called_at = Some(now);
            let k = DataKey::Position(union.clone(), lp.clone());
            env.storage().persistent().set(&k, &p);
            bump(&env, &k);
        }
        let from = p.called_at.unwrap_or(now) + CALL_NOTICE;
        PositionCalled {
            union,
            lp,
            owed: p.owed,
            claimable_from: from,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(from)
    }

    /// Her account books the union's claim (nila-account.claim, started by the union's multisig): it
    /// moved `usdc` to the union's account. Clears the rupees that USDC is worth at today's fix;
    /// returns the USDC her account unlocks (the rest, once nothing is owed). Her account calls
    /// this, never the other way round (no re-entry), so fx_pool never moves her USDC.
    pub fn book_claim(env: Env, union: Address, lp: Address, usdc: i128) -> Result<i128, Error> {
        lp.require_auth();
        let mut p = load_position(&env, &union, &lp).ok_or(Error::NoPosition)?;
        let can = claimable(&env, &p)?;
        if usdc <= 0 || usdc > can {
            return Err(Error::NotClaimable);
        }
        let rate: RateFix = env
            .storage()
            .instance()
            .get(&DataKey::Rate)
            .ok_or(Error::StaleOracle)?;
        // `claimable` rounds up, so USDC that covers what she owes clears all of it.
        let need = (p.owed * RATE_ONE + rate.inr_per_usd - 1) / rate.inr_per_usd;
        let cleared = if usdc >= need {
            p.owed
        } else {
            usdc * rate.inr_per_usd / RATE_ONE
        };
        p.owed -= cleared;
        p.locked -= usdc;
        add_lp_owed(&env, &union, -cleared);
        let unlocked = if p.owed <= 0 { p.locked } else { 0 };
        save_position(&env, &union, &lp, &p);
        PositionClaimed {
            union,
            lp,
            usdc,
            inr: cleared,
            inr_per_usd: rate.inr_per_usd,
            unlocked,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(unlocked)
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

    /// Legacy (C5b swaps, before 30 Oct): USDC LPs paid in through Collect swaps; nothing adds to it now.
    pub fn lp_usdc(env: Env, union: Address) -> i128 {
        p_get(&env, &DataKey::LpUsdc(union))
    }

    pub fn voucher_floor(env: Env, union: Address) -> i128 {
        voucher_floor(&env, &union)
    }

    /// C9: what a Collect of `inr` rupees locks, or a Bring is worth and its rupee fee, at today's
    /// fix (SEP-38 reads it). The same price `accept` fixes while the fix and the fee stay.
    pub fn quote(env: Env, union: Address, kind: OfferKind, inr: i128) -> Result<Quote, Error> {
        let _ = union;
        check_amount(&env, inr)?;
        price(&env, kind, inr)
    }

    /// USDC a Bring of `inr` would unlock for her now (the anchor's Bring quote).
    pub fn bring_unlock(env: Env, union: Address, lp: Address, inr: i128) -> Result<i128, Error> {
        let p = load_position(&env, &union, &lp).ok_or(Error::NoPosition)?;
        if inr <= 0 || p.owed < inr {
            return Err(Error::NoPosition);
        }
        Ok(unlock_for(&p, inr))
    }

    pub fn position(env: Env, union: Address, lp: Address) -> Option<Position> {
        load_position(&env, &union, &lp)
    }

    /// The union's LPs who hold its cash (each one's `position` says how much).
    pub fn lps_owing(env: Env, union: Address) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::LpsOwing(union))
            .unwrap_or(vec![&env])
    }

    /// Rupees the union's LPs hold (its cash out with them).
    pub fn lp_owed(env: Env, union: Address) -> i128 {
        p_get(&env, &DataKey::LpOwed(union))
    }

    pub fn claimable(env: Env, union: Address, lp: Address) -> Result<i128, Error> {
        match load_position(&env, &union, &lp) {
            Some(p) => claimable(&env, &p),
            None => Ok(0),
        }
    }

    pub fn swap_key(env: Env) -> Option<BytesN<32>> {
        env.storage().instance().get(&DataKey::SwapKey)
    }

    pub fn lp_fee(env: Env) -> u32 {
        lp_fee_bp(&env)
    }

    pub fn anchor(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Anchor)
    }
}

/// An open offer of the union's rupees (post_offer, and the cash scan's own Collect over the treasury, 24 Oct).
/// Merged (25 Oct): while the union has an open offer of the same kind no LP has accepted yet, the rupees are added
/// to it (no size limit since 30 Oct) and its expiry moved to the later one, instead of a new
/// offer; the id returned is that offer's.
pub(crate) fn open_offer(
    env: &Env,
    union: &Address,
    kind: OfferKind,
    inr: i128,
    expires: u64,
) -> Result<u64, Error> {
    let mut ids = offers_of(env, union);
    let now = env.ledger().timestamp();
    for oid in ids.iter() {
        if let Ok(mut o) = load_offer(env, oid) {
            if o.kind == kind && o.state == OfferState::Open && o.expires > now {
                o.inr += inr;
                o.expires = core::cmp::max(o.expires, expires);
                save_offer(env, oid, &o);
                OfferMerged {
                    union: union.clone(),
                    offer_id: oid,
                    added: inr,
                    inr: o.inr,
                    expires: o.expires,
                }
                .publish(env);
                return Ok(oid);
            }
        }
    }
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
    let empty = String::from_str(env, "");
    save_offer(
        env,
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
    bump(env, &k);
    OfferPosted {
        union: union.clone(),
        offer_id: id,
        kind,
        inr,
        expires,
    }
    .publish(env);
    Ok(id)
}
