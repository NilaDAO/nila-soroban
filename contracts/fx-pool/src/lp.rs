//! LP positions (executive decision 31 Oct; replaces the 30 Oct pledge and the C5b swap). An LP
//! is a member who owns USDC and moves the union between cash and USDC. Every position is USDC
//! that stays locked until the opposite offer of the same LP closes it; open ended.
//!
//! - An **LP** is a member of the union with a land title, accepted by the trust's anchor (K1, R20),
//!   on a Nila account (nila-account), which carries the lien.
//! - **Collect** (the union has spare cash): the LP takes the rupees. If the LP holds a Bring
//!   position, the Collect closes it first: the union pays its rupees at today's fix plus the
//!   close fee, and the locked USDC goes back to the union. The rest opens (or grows) a Collect
//!   position: the LP's own USDC at today's fix is locked at accept and taken into fx_pool at the
//!   scan, held there for that LP.
//! - **Bring** (the union is short of cash): the LP brings the rupees. If the LP holds a Collect
//!   position, the Bring closes it first: the LP hands over its rupees at today's fix less the
//!   close fee (the union's cost, from its treasury) and gets that USDC back, free. The rest opens
//!   (or grows) a Bring position: the union's own USDC (its treasury backing, never senior or a
//!   leader's swap) goes to the LP's account, locked there. Refused when the union's USDC is short.
//! - **Close fee:** 2% (1% a leg, `lp_fee` × 2) of the rupees closed, whole rupees, in cash.
//! - **Rate risk is the LP's;** the only other end of a position is the **margin**: once fx_pool's
//!   fix is more than 5% from the position's opening rate, anyone may `settle_position`: a Collect
//!   position's USDC becomes the union's, a Bring position's USDC unlocks for the LP; each side
//!   keeps the cash it has. No fee.
//! - One accepted offer per LP at a time (`Busy`), so a position cannot change between accept and
//!   scan. Partial closes are pro rata.
//! - Each accepted offer stores the anchor's SEP-24 transaction id and the SEP-38 quote id;
//!   `accept` needs the anchor's signature over them (C9b), so the anchor records every change.
//! - A no-show costs nothing: the offer expires and what accept reserved goes back.

use super::*;
use soroban_sdk::{xdr::ToXdr, String};

/// What the anchor's swap key signs for one accept (C9b): this prefix, then the XDR of
/// (fx_pool, offer_id, lp, sep24_id, quote_id, usdc, valid_until); usdc = the USDC that moves
/// (`lp_quote`: closed plus opened).
pub const ACCEPT_PREFIX: &[u8] = b"nila-lp-accept-v3";

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
    /// USDC that moves at the scan: closed plus opened (`lp_quote`).
    pub usdc: i128,
    pub inr_per_usd: i128,
    pub sep24_id: String,
    pub quote_id: String,
    /// Fixed at accept (31 Oct; the names are kept so stored offers still read): USDC of the LP's
    /// opposite position this offer closes, USDC it opens, and the close fee in rupees.
    pub from_lp: i128,
    pub from_domestic: i128,
    pub from_offshore: i128,
}

/// What an offer is worth at today's fix (C9, the anchor's SEP-38 price reads it): `usdc` = the
/// rupees at the fix, `fee_inr` = the close fee if it closed a position in full.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Quote {
    pub usdc: i128,
    pub fee_inr: i128,
    pub fee_bp: u32,
    pub inr_per_usd: i128,
    /// The fix's expiry: a quote after it fails at accept with StaleOracle.
    pub valid_until: u64,
}

/// What one LP's accept of an offer does at today's fix (31 Oct): `close_usdc` of the LP's
/// opposite position for `close_inr` rupees (plus or less `fee_inr`), and `open_usdc` for
/// `open_inr` rupees into a position of the offer's kind. `usdc` = close + open (the anchor signs it).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpQuote {
    pub usdc: i128,
    pub close_usdc: i128,
    pub close_inr: i128,
    pub open_usdc: i128,
    pub open_inr: i128,
    pub fee_inr: i128,
    pub inr_per_usd: i128,
    pub valid_until: u64,
}

/// An LP's position in a union (31 Oct): its kind (the offer that opened it), the USDC locked
/// (in fx_pool for a Collect, in the LP's account for a Bring), the rupees it stood for at
/// opening (so its opening rate), and since when.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpPosition {
    pub kind: OfferKind,
    pub usdc: i128,
    pub inr: i128,
    pub opened: u64,
}

/// What an LP's Nila account does for fx_pool (nila-account): the lien, and paying locked USDC in.
#[allow(dead_code)]
#[soroban_sdk::contractclient(name = "LienClient")]
pub trait LienTrait {
    fn lock(env: Env, amount: i128) -> i128;
    fn unlock(env: Env, amount: i128) -> i128;
    fn pay(env: Env, amount: i128) -> i128;
}

#[contractevent]
pub struct PositionChanged {
    #[topic]
    pub union: Address,
    #[topic]
    pub lp: Address,
    pub kind: OfferKind,
    pub usdc: i128,
    pub inr: i128,
}

/// A position settled past the margin (31 Oct): its USDC went to the union (Collect) or unlocked
/// for the LP (Bring).
#[contractevent]
pub struct PositionSettled {
    #[topic]
    pub union: Address,
    #[topic]
    pub lp: Address,
    pub kind: OfferKind,
    pub usdc: i128,
    pub inr: i128,
    pub inr_per_usd: i128,
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
/// A position may be settled once the fix is more than this far from its opening rate (31 Oct).
pub const MARGIN_BP: i128 = 500;

fn p_get(env: &Env, k: &DataKey) -> i128 {
    env.storage().persistent().get(k).unwrap_or(0)
}
fn p_set(env: &Env, k: &DataKey, v: i128) {
    env.storage().persistent().set(k, &v);
    bump(env, k);
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
    // Accepted, not scanned: a Collect's new lock goes, a Bring's reserved union USDC comes back.
    if o.state == OfferState::Accepted && o.from_domestic > 0 {
        match o.kind {
            OfferKind::Collect => {
                if let Some(lp) = &o.lp {
                    LienClient::new(env, lp).unlock(&o.from_domestic);
                }
            }
            OfferKind::Bring => add_usdc(
                env,
                &DataKey::TreasuryUsdc(o.union.clone()),
                o.from_domestic,
            ),
        }
    }
    if let Some(lp) = &o.lp {
        env.storage()
            .persistent()
            .remove(&DataKey::Busy(o.union.clone(), lp.clone()));
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

fn add_usdc(env: &Env, k: &DataKey, d: i128) {
    p_set(env, k, p_get(env, k) + d);
}

fn fresh_rate(env: &Env) -> Result<RateFix, Error> {
    let rate: RateFix = env
        .storage()
        .instance()
        .get(&DataKey::Rate)
        .ok_or(Error::StaleOracle)?;
    if env.ledger().timestamp() > rate.valid_until {
        return Err(Error::StaleOracle);
    }
    Ok(rate)
}

/// Whole rupees, rounded down: cash is notes.
fn whole(inr: i128) -> i128 {
    inr / NIN_UNIT * NIN_UNIT
}

fn to_usdc(inr: i128, rate: i128) -> Result<i128, Error> {
    inr.checked_mul(RATE_ONE)
        .map(|v| v / rate)
        .ok_or(Error::CapExceeded)
}

fn load_position(env: &Env, union: &Address, lp: &Address) -> Option<LpPosition> {
    env.storage()
        .persistent()
        .get(&DataKey::LpPos(union.clone(), lp.clone()))
}

/// What `lp` accepting a `kind` offer of `inr` rupees does at today's fix (same maths in `accept`
/// and `lp_quote`): close the LP's opposite position first, the rest opens.
fn plan(
    env: &Env,
    union: &Address,
    lp: &Address,
    kind: OfferKind,
    inr: i128,
) -> Result<LpQuote, Error> {
    let rate = fresh_rate(env)?;
    let r = rate.inr_per_usd;
    let fbp = 2 * i128::from(lp_fee_bp(env));
    // The opposite position's USDC and its rupees at today's fix.
    let opp = match load_position(env, union, lp) {
        Some(p) if p.kind != kind => p.usdc,
        _ => 0,
    };
    let worth = opp * r / RATE_ONE;
    let (close_inr, fee_inr) = match kind {
        // The union pays the rupees closed plus the fee, out of `inr`.
        OfferKind::Collect => {
            let fee = whole(inr * fbp / (10_000 + fbp));
            if inr - fee <= worth {
                (inr - fee, fee)
            } else {
                (
                    worth,
                    core::cmp::min(whole(worth * fbp / 10_000), inr - worth),
                )
            }
        }
        // The LP hands over the rupees closed less the fee.
        OfferKind::Bring => {
            let c = core::cmp::min(inr, worth);
            (c, whole(c * fbp / 10_000))
        }
    };
    let close_usdc = if close_inr >= worth {
        opp
    } else {
        to_usdc(close_inr, r)?
    };
    let open_inr = match kind {
        OfferKind::Collect => inr - close_inr - fee_inr,
        OfferKind::Bring => inr - close_inr,
    };
    let open_usdc = to_usdc(open_inr, r)?;
    Ok(LpQuote {
        usdc: close_usdc + open_usdc,
        close_usdc,
        close_inr,
        open_usdc,
        open_inr,
        fee_inr,
        inr_per_usd: r,
        valid_until: rate.valid_until,
    })
}

/// Closes `close` USDC of the LP's position (pro rata) and opens `open` USDC for `open_inr`
/// rupees in a position of `kind`.
fn move_position(
    env: &Env,
    union: &Address,
    lp: &Address,
    kind: OfferKind,
    close: i128,
    open: i128,
    open_inr: i128,
) {
    let k = DataKey::LpPos(union.clone(), lp.clone());
    let mut p = load_position(env, union, lp);
    if let Some(q) = p.as_mut() {
        if close > 0 {
            q.inr -= q.inr * close / q.usdc;
            q.usdc -= close;
        }
        if q.usdc <= 0 {
            p = None;
        }
    }
    if open > 0 {
        p = Some(match p {
            Some(mut q) => {
                q.usdc += open;
                q.inr += open_inr;
                q
            }
            None => LpPosition {
                kind,
                usdc: open,
                inr: open_inr,
                opened: env.ledger().timestamp(),
            },
        });
    }
    let (pk, usdc, inr) = match &p {
        Some(q) => {
            env.storage().persistent().set(&k, q);
            bump(env, &k);
            (q.kind, q.usdc, q.inr)
        }
        None => {
            env.storage().persistent().remove(&k);
            (kind, 0, 0)
        }
    };
    PositionChanged {
        union: union.clone(),
        lp: lp.clone(),
        kind: pk,
        usdc,
        inr,
    }
    .publish(env);
}

fn lp_fee_bp(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::LpFeeBp)
        .unwrap_or(DEFAULT_LP_FEE_BP)
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

    // ---------- offers ----------

    /// L11 #short / A7 (Bring) or C4 (Collect): a leader posts rupees, open until accepted or cancelled.
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
        // No fixed period (31 Oct): an offer may stay open until an LP accepts it or a leader cancels it
        // (u64::MAX); only a time already past is refused.
        if expires <= env.ledger().timestamp() {
            return Err(Error::BadExpiry);
        }
        let id = open_offer(&env, &union, kind, inr, expires)?;
        bump_instance(&env);
        Ok(id)
    }

    /// A2: an LP of the union accepts at the attester's fix (the rate is fixed now), with the anchor's
    /// signed quote (C9b): NotAnchor without a swap key, QuoteExpired after `valid_until`,
    /// QuoteChanged when today's `lp_quote` no longer gives `usdc`; a bad signature traps (host
    /// error, no contract error code). A Collect locks the LP's new USDC in the LP's account; a
    /// Bring reserves the union's USDC it opens (NoUsdc when short). One accepted offer per LP.
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
        let busy = DataKey::Busy(o.union.clone(), lp.clone());
        if env.storage().persistent().has(&busy) {
            return Err(Error::OfferState);
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
        let q = plan(&env, &o.union, &lp, o.kind, o.inr)?;
        if q.usdc != usdc {
            return Err(Error::QuoteChanged);
        }
        if q.open_usdc > 0 {
            match o.kind {
                // The LP's account locks it; refused above its free USDC.
                OfferKind::Collect => {
                    LienClient::new(&env, &lp).lock(&q.open_usdc);
                }
                // The union's own USDC, reserved until the scan.
                OfferKind::Bring => {
                    let k = DataKey::TreasuryUsdc(o.union.clone());
                    if p_get(&env, &k) < q.open_usdc {
                        return Err(Error::NoUsdc);
                    }
                    add_usdc(&env, &k, -q.open_usdc);
                }
            }
        }
        env.storage().persistent().set(&busy, &offer_id);
        bump(&env, &busy);
        o.usdc = usdc;
        o.from_lp = q.close_usdc;
        o.from_domestic = q.open_usdc;
        o.from_offshore = q.fee_inr;
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
            usdc,
            inr_per_usd: q.inr_per_usd,
            sep24_id,
            quote_id,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(usdc)
    }

    /// A1b / A7b: the leader scanned and counted; everything moves as fixed at accept.
    /// Collect: the rupees leave the cash at hand; the LP's account pays the closed and the new
    /// USDC into fx_pool (closed → the union's, new → held for the LP). Bring: the rupees less the
    /// fee join the cash at hand (a 7-day escrow; the fee from the treasury); the closed USDC goes
    /// back to the LP free, the reserved union USDC to the LP's account, locked.
    pub fn confirm(env: Env, leader: Address, offer_id: u64) -> Result<i128, Error> {
        require_not_paused(&env)?;
        let mut o = load_offer(&env, offer_id)?;
        require_leader(&env, &o.union, &leader)?;
        if o.state != OfferState::Accepted || env.ledger().timestamp() >= o.expires {
            return Err(Error::OfferState);
        }
        let lp = o.lp.clone().ok_or(Error::OfferState)?;
        let (close, open, fee) = (o.from_lp, o.from_domestic, o.from_offshore);
        let me = env.current_contract_address();
        let usdc = TokenClient::new(&env, &get::<Address>(&env, &DataKey::Usdc));
        let held = DataKey::LpLocked(o.union.clone());
        match o.kind {
            OfferKind::Collect => {
                settle(&env, &o.union, o.inr);
                if close + open > 0 {
                    LienClient::new(&env, &lp).pay(&(close + open));
                }
                add_usdc(&env, &DataKey::TreasuryUsdc(o.union.clone()), close);
                add_usdc(&env, &held, open);
            }
            OfferKind::Bring => {
                let cash = o.inr - fee;
                check_room(&env, &o.union, cash)?;
                open_escrow(&env, &o.union, &lp, Purpose::Bring, cash);
                if fee > 0 {
                    FundCoreClient::new(&env, &core_addr(&env)?)
                        .cover_expired_escrow(&o.union, &fee);
                }
                if close + open > 0 {
                    usdc.transfer(&me, &lp, &(close + open));
                }
                add_usdc(&env, &held, -close);
                if open > 0 {
                    LienClient::new(&env, &lp).lock(&open);
                }
            }
        }
        // The opened rupees at the accept's fix: the position's opening rate.
        move_position(
            &env,
            &o.union,
            &lp,
            o.kind,
            close,
            open,
            open * o.inr_per_usd / RATE_ONE,
        );
        env.storage()
            .persistent()
            .remove(&DataKey::Busy(o.union.clone(), lp.clone()));
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
            from_lp: close,
            from_domestic: open,
            from_offshore: fee,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(o.usdc)
    }

    /// The leader withdraws an open or accepted offer; what accept reserved goes back.
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

    /// Anyone, after expiry: a no-show costs nothing; what accept reserved goes back.
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

    /// Anyone, once fx_pool's fix is more than 5% from a position's opening rate (31 Oct; InMargin
    /// before): a Collect position's USDC becomes the union's, a Bring position's unlocks for the
    /// LP. Each side keeps its cash; no fee. Not while the LP has an accepted offer.
    pub fn settle_position(env: Env, union: Address, lp: Address) -> Result<i128, Error> {
        require_not_paused(&env)?;
        let p = load_position(&env, &union, &lp).ok_or(Error::NoPosition)?;
        if env
            .storage()
            .persistent()
            .has(&DataKey::Busy(union.clone(), lp.clone()))
        {
            return Err(Error::OfferState);
        }
        let r = fresh_rate(&env)?.inr_per_usd;
        let r0 = p.inr * RATE_ONE / p.usdc;
        if (r - r0).abs() * 10_000 <= MARGIN_BP * r0 {
            return Err(Error::InMargin);
        }
        match p.kind {
            OfferKind::Collect => {
                add_usdc(&env, &DataKey::LpLocked(union.clone()), -p.usdc);
                add_usdc(&env, &DataKey::TreasuryUsdc(union.clone()), p.usdc);
            }
            OfferKind::Bring => {
                LienClient::new(&env, &lp).unlock(&p.usdc);
            }
        }
        env.storage()
            .persistent()
            .remove(&DataKey::LpPos(union.clone(), lp.clone()));
        PositionSettled {
            union,
            lp,
            kind: p.kind,
            usdc: p.usdc,
            inr: p.inr,
            inr_per_usd: r,
        }
        .publish(&env);
        bump_instance(&env);
        Ok(p.usdc)
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

    /// C9: what an offer's rupees are worth at today's fix and the close fee on them (SEP-38 reads
    /// it). What one LP's accept does: `lp_quote`.
    pub fn quote(env: Env, union: Address, kind: OfferKind, inr: i128) -> Result<Quote, Error> {
        let _ = (union, kind);
        check_amount(&env, inr)?;
        let rate = fresh_rate(&env)?;
        let fee_bp = 2 * lp_fee_bp(&env);
        Ok(Quote {
            usdc: to_usdc(inr, rate.inr_per_usd)?,
            fee_inr: whole(inr * i128::from(fee_bp) / 10_000),
            fee_bp,
            inr_per_usd: rate.inr_per_usd,
            valid_until: rate.valid_until,
        })
    }

    /// What `lp` accepting a `kind` offer of `inr` rupees does at today's fix: the USDC it closes
    /// and opens, and the close fee (the anchor signs `usdc`).
    pub fn lp_quote(
        env: Env,
        union: Address,
        lp: Address,
        kind: OfferKind,
        inr: i128,
    ) -> Result<LpQuote, Error> {
        check_amount(&env, inr)?;
        plan(&env, &union, &lp, kind, inr)
    }

    pub fn position(env: Env, union: Address, lp: Address) -> Option<LpPosition> {
        load_position(&env, &union, &lp)
    }

    /// USDC held in fx_pool for the union's LPs' Collect positions (31 Oct; before, the C5b
    /// swaps' LP USDC, which nothing adds to now).
    pub fn lp_usdc(env: Env, union: Address) -> i128 {
        p_get(&env, &DataKey::LpLocked(union))
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
