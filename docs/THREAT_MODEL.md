# Threat model — Nila on Stellar mainnet

SCF #45 Tranche 2, deliverable D (row 22). Covers the contracts at tag `mainnet-2026-11-06` and the keys and services
around them; §3.11 and the lane-B notes (25 Nov) cover `main` as it goes to mainnet with the lane-B upgrade. **State as read on mainnet, 19 Nov 2026** (Horizon and contract reads; how to repeat them: §6). Alert
rules for every risk below: [`ALERTS.md`](ALERTS.md). Events: [`EVENT_SCHEMA.md`](EVENT_SCHEMA.md), kept by the
indexer at [indexer.nila.land](https://indexer.nila.land/coverage).

## 1. What is protected, and from whom

| Asset | Where it sits |
|---|---|
| Members' and the union's money (nIN) | `fund_core` (junior, senior's share, treasury, reserve, machine owners); members never hold nIN |
| LP and senior USDC | `fx_pool` (LP offers in escrow), `senior` (senior principal; cap 0 today) |
| Who may act | `roles`: the trust admin, the attester and oracle roles, each union's leaders and settings |
| Member accounts | One smart account per member (`account`, made by `account_deployer`); her key signs debt and value out |
| Land titles and food tokens | `land_title`, `fund_core` (attested crop allowance), `crop_calendar` (draw windows) |

**Trust boundary, decided 5 Nov ("members trust their union"):** the contracts do not guard members against their
own admitted union's settings (fx_pool, SMS key, recovery wait, session length). They guard against key thieves,
against the code of LP and member accounts, and against outsiders. A union that turns on its members is a governance
failure handled by the trust (`roles.revoke_union`), not a contract one. This is stated, not hidden: §3.2.

## 2. Keys and who holds them

| Key | Holds | State on mainnet (19 Nov) | If stolen |
|---|---|---|---|
| **Trust admin** `GC2V…5U3E` | Admin of all eight contracts: upgrade, pause, caps, roles, the Reflector oracle (no rate override since 23 Nov) | Native **3-of-5** (5 signers weight 1, thresholds 3/3/3; Carst's Ledger is one of the five) | §3.1 |
| **Union account** (Mother Theresa `GCDS…EXW`) | Leaders, union settings, machine prices | Native **2-of-3** (3 leaders weight 1, thresholds 2, master 0) | §3.2 |
| **Leader** (one G key each, Privy) | Draws and desk work for members in a session, cash scans, offers, one half of a recovery | Single key per leader | §3.3 |
| **Member** (Privy Ed25519, or a P-256 device key on a keypad phone) | Her account: debt, investment, value out | Single key; recoverable only by §3.4 | §3.4 |
| **Union SMS key** `GCEG…SC3G` | The other half of a recovery | Hot, on the node | §3.4 |
| **Attester** `GDHZ…XUNH` | Titles, food tokens (allowances), the INR/USD fix | Hot, on the node; 48 XLM | §3.5, §3.6 |
| **Oracle** `GDHJ…RKON` | End-of-season dates (`report_maturity`) | Hot, on the node; 20 XLM | §3.6 |
| **Anchor swap key** `GCZX…MS2` | Accepts LP offers in `fx_pool` | Hot, on the anchor's business server (apart from the anchor account key) | §3.7 |
| **nIN issuer** `GABG…IG5A` | Classic issuance of nIN | **Not locked**: master weight 1 on a Ledger (index 2); flags auth required, revocable, clawback. Lock waits on counsel (X14) | §3.1 |
| **Node sponsor** `GDZS…EYQL` | XLM top-ups for leaders and ID-checked accounts (≤ 5 XLM, ≤ 3 a day per account) | Hot, on the node; 69 XLM | Loses its XLM only |

## 3. Threats

Each: what an attacker gains, what stops or limits it in code, what is **not** covered, and the alert rule (`A…`).

### 3.1 Admin-key compromise and malicious upgrade

- **Gain:** with 3 of the 5 trust keys: upgrade any contract to new code, pause, change caps, roles and the rate.
- **Limits:** every upgrade is filed, then executed no sooner than the delay (**48 h on all eight contracts**, read
  19 Nov; no pending upgrade); `cancel_upgrade` stops it; lowering the delay itself waits out the current delay
  (`nila_common::upgrade`). Every step is evented (`upgrade_proposed`, `upgrade_applied`, `upgrade_cancelled`,
  `upgrade_delay_set`) with the new wasm hash. The rate cap in code is 12.5 % (`MAX_RATE_BP`); changing it needs an
  upgrade. Signers are on hardware keys, kept apart (decision 10 Oct).
- **Not covered:** a 3-of-5 quorum acting together; losing the quorum freezes administration (no contract
  `set_admin` except on `roles`; decided 10 Oct: signer hygiene instead). The **unlocked nIN issuer**: its Ledger key
  could still issue or claw back nIN by classic operations, outside `fx_pool`. Until the lock, any issuer operation
  is an alert.
- **Alerts:** A1 (upgrade filed), A2 (delay changed), A3 (admin or role change), A4 (issuer operation),
  A5 (signers or thresholds of the trust account changed).

### 3.2 Union-operator compromise (2 of the union's 3 keys)

- **Gain:** appoint leaders (`roles.set_leader`), change the union's settings (SMS key, recovery wait 1 h – 30 days,
  session 60–900 s, fx_pool, USDC), change machine prices.
- **Limits:** settings only within the code's bounds (`WAIT_MIN`/`WAIT_MAX`, `SESSION_MIN`/`SESSION_MAX`);
  `session_contracts` must hold the core role, so a session can only reach `fund_core` (read 19 Nov: only
  `fund_core`, session 300 s, wait 4 h). The trust can revoke the union at once (`roles.revoke_union`): its leaders
  lose their rights and its members' accounts lose sessions and recovery until it is admitted again.
- **Not covered:** by design (5 Nov), a union's own board can set short waits and its own SMS key, which together
  with a leader is enough to recover (take over) a member's account after the wait. Members are told whom they trust.
- **Alerts:** A6 (union settings changed), A7 (leader added or removed), A8 (union admitted or revoked).

### 3.3 Leader-key compromise

- **Gain:** inside a member's session (opened by her own signed QR, at most the union's session length): `draw_loan`,
  `request_unbond`, `claim_junior_cash`, `return_voucher`, and nothing else on her account. Outside a session: cash
  scans and LP offers for the union, half of a recovery.
- **Limits:** a session needs the member's signature each time; the loan cash is junior money in `fund_core`, paid
  out over the desk; one loan per food token, in its crop-calendar window; the union's lendable cash above its
  reserve; `fx_pool` caps leader swaps and USDC into the treasury at `max_scan` (**10,000 nIN**, read 19 Nov).
- **Not covered:** a leader colluding with the SMS key (§3.4). Rupee cash at the desk is off-chain: a cash scan mints
  what the leader says was taken in.
- **Alerts:** A9 (recovery proposed), A10 (cash scan or desk payout above a threshold), A7.

### 3.4 Member-key loss or theft, and recovery

- **Gain (thief):** what she can do: take up debt in a session she opens, move her investment out.
- **Limits:** one recovery path (4 Nov): `recover_by_sms` needs **both** the union's SMS key and one leader; it
  takes effect only after the union's wait (4 h today) and freezes the account meanwhile; it cannot be replaced or
  cancelled, so the wait is not restarted by a thief. The member alone moves her account to new code, never during a
  recovery, a session or a lien.
- **Not covered:** the SMS key is hot on the node: a node compromise plus one leader key can recover any member
  account of the union after 4 h. Detection is the defence: every `recovery_proposed` alerts, and the 4 h is the
  window to react (`roles.revoke_union` stops the union's recoveries at once).
- **Alerts:** A9, A11 (signer changed on a member account).

### 3.5 Attester failure or compromise

- **Failure:** no new titles, food tokens or rate fixes. Loans already drawn are unaffected; new draws wait.
- **Compromise — titles and food tokens:** it can attest a title or a crop allowance that is not real. A draw still
  needs the member's signature and a leader, and lending stays within the union's cash; `enforce_token_amount` is
  off (the allowance is a recommendation, 10 Oct).
- **Compromise — rate:** a fix at most every 12 hours, within 3 % of Reflector's latest price (or of the last fix
  while Reflector is unset), valid at most 2 days (`fx_pool.set_rate`). Once Reflector is set the fix is only the
  fallback while Reflector is stale. There is no trust override on mainnet builds (23 Nov).
- **Alerts:** A12 (rate fix more than 2 % from the reference), A13 (rate older than 36 h), A14 (attester XLM low).

### 3.6 Satellite-oracle failure or compromise (end of season)

- **Failure:** no end of season reported → no due date → **no default** and no overdue flag (`due_ts` is `None`).
  Members are never defaulted by a missing report; repayment works as usual.
- **Compromise:** `report_maturity` is set once per loan, not before the draw and not in the future. The earliest
  due date it can cause is draw + 90 + 21 days (`MIN_TERM`, `REPAY_WINDOW`); `mark_default` then needs that date
  passed. It cannot shorten a term below 111 days.
- **Alerts:** A15 (end of season reported sooner than any crop grows), A16 (loan defaulted).

### 3.7 Rate staleness (Chainlink today, Reflector after T2-B)

- **Today:** the node's beat posts Chainlink USD/INR (read on Polygon) as the attester's fix every 12 h; the fix
  is valid 2 days. A stale or missing fix makes rate-priced calls refuse (USDC into the treasury, swaps, LP accept,
  senior mint) rather than use an old rate. Read 19 Nov: 93.448 INR/USD, valid to 21 Nov.
- **After T2-B (23 Nov):** Reflector's forex price (SEP-40, `prices`, the last hour) is the rate while at most
  15 minutes old; the attester's fix stands in while it is stale. **Circuit breaker:** a latest price more than 3 %
  from Reflector's own last-hour average stops every rate-priced call (`SuspiciousRate`) until it settles; no
  fallback and no override then. Not covered by the breaker: a feed that is consistently wrong (no second source).
- **Alerts:** A13, A12.

### 3.8 Anchor settlement failure (LP offers, SEP-24)

- **Limits:** since the LP pledge (30 Oct) an LP's USDC stays in her own account under a lien; `fx_pool` takes it
  only when an accepted offer is confirmed. A union's cash above its treasury waits in a cash escrow that ends after
  **7 days** (`ESCROW_TERM`, then `burn_expired_escrow`, covered from the treasury); an LP offer ends at the expiry
  its leader set, after which anyone may call `expire_offer`. The swap key only accepts; the anchor account key stays
  with the trust operator.
- **Not covered yet:** a daily limit on USDC out. fx_pool's `daily_cap` (2,500 USDC, read 19 Nov) is stored and
  evented but no longer enforced since the LP pledge (30 Oct). Decided 19 Nov: `senior` gets its own daily cap with an
  admin call to change it (`set_daily_cap`, trust; `redeem` beyond it refused until the next UTC day), and fx_pool's
  `daily_cap` stays for LP offers and is enforced again; both in the lane-B upgrade.
  Exposure today is nil (`senior.cap` 0); the cap must be live before `senior.cap` is raised. Until then A20 watches
  the USDC that leaves per day.
- **Disputes:** when the two sides disagree whether rupees changed hands, the union's board decides off-chain and
  executes with the existing calls (`cancel_offer`, `settle_position`) — or an on-chain `resolve_offer` (decision
  Q5, open).
- **Alerts:** A17 (escrow within 24 h of expiry), A18 (escrow expired), A19 (anchor `stellar.toml` or health down),
  A20 (USDC out in a day, senior exits and LP offers, above 80 % of 2,500).

### 3.9 Contract bug with real money

- **Limits:** caps (`max_scan` 10,000 nIN on leader swaps and USDC into the treasury; `senior.cap` **0**: no senior
  deposit is taken);
  pause on `fund_core` (every money path, also `mark_default`), `fx_pool`, `senior`, `land_title`, `machinery`; all
  read **unpaused** 19 Nov. 48 h upgrade to fix. CI: tests, clippy, a 128 KiB size gate, a frozen storage layout.
- **Not covered:** no external audit yet; the caps are the control until it is done (decision 10 Oct), and they
  rise only on evidence.
- **Alerts:** A21 (any pause), A22 (any cap changed), A23 (fund_core's nIN ≠ the sum of its books).

### 3.10 The indexer and the node

- **Indexer down:** events older than 7 days are lost to it; the gap is recorded and shown (`/coverage`). A24.
- **Node down:** no attester, oracle, rate or top-ups; members' and leaders' own calls go to the chain directly.

### 3.11 The guarantee pool (`senior`, lane B)

What it is: [`SENIOR_GUARANTEE_POOL.md`](SENIOR_GUARANTEE_POOL.md). Senior's USDC never lends; it leaves the pool only
by a loss cover or an investor's exit.

- **Draining by covers.** *Gain:* with 3 of the 5 trust keys, USDC out of the pool to a union. *Limits:* a cover pays
  at most the voucher loss fund_core booked for that union (`voucher_loss`, written only by a real default after
  `maturityTs` + 21 days), at most the ceiling proposed and at most what the pool holds (`NoCover` otherwise); it waits
  **48 h** after `propose_cover`, visible to all, and `cancel_cover` stops it; the USDC goes to the union's treasury
  through fx_pool, never to an address of the trust's choosing. *Not covered:* the quorum acting together with a union
  and the attester to make loans that are meant to default: the voucher loss would then be real on the books. The
  limits are the attested acres (vouchers per acre), the 21-day wait and the alerts; the cap (0 until the audit) bounds
  what there is to take.
- **Running ahead of a loss.** An investor who sees a cover coming cannot leave first: `request_redeem` starts **14
  days**, longer than the cover's 48 h, and the waiting shares are paid at the index of the day they are withdrawn, so
  they carry every loss covered meanwhile. Exits are also held to the daily cap (**2,500 USDC**, `set_daily_cap`).
- **Rate.** The fee and a cover convert rupees and USDC at fx_pool's rate: Reflector with its ±3 % breaker (§3.7). A
  wrong rate moves value between senior and junior; a stopped rate stops the Collect that carries the fee and the cover.
- **The fee.** Senior earns the voucher interest (`guarantee_fee`, 100 %); the trust can lower it
  (`set_guarantee_fee`, an event). It is paid only out of an LP's Collect; with no Collect it waits in fund_core, owed.
- **Who invests.** Only addresses the trust accepted after KYC (`set_investor`, 3-of-5) can deposit, up to the cap.
- **More loss than the pool.** The rest stays booked as owed by senior (`voucher_loss`); junior does not carry it.
- **Not covered:** no alert compares senior's USDC balance with its books yet (the rehearsal checks it; an A23-style
  rule for `senior` is to add). No audit.
- **Alerts:** A26 (cover proposed, paid or cancelled: page), A27 (voucher loss waiting), A20 (USDC out in a day,
  covers and exits), A22 (cap, daily cap or fee share changed), A21 (pause).

## 4. Cap and timelock schedule

| Control | Now (19 Nov) | Next step | Who |
|---|---|---|---|
| Upgrade delay | 48 h, all eight contracts | stays | — |
| `max_scan` (leader swaps, USDC into treasury) | 10,000 nIN | ₹5 lakh (decided 23 Nov), by the trust's `call` after the lane-B upgrade | trust 3-of-5 |
| Daily cap, senior USDC out | none | `senior.daily_cap`, 2,500 USDC, admin `set_daily_cap`: **built** (lane B), live after the upgrade | trust |
| Daily cap, LP offers (`fx_pool.daily_cap`) | 2,500 USDC stored, not enforced since 30 Oct | enforced again on LP confirms: **built** (lane B), live after the upgrade | trust |
| `senior.cap` | 0 | after the audit | trust |
| Loss cover (`senior.propose_cover` → `execute_cover`) | — (lane B) | 48 h delay, ≤ booked voucher loss and ≤ the pool (§3.11) | trust 3-of-5 |
| Rate band (attester) | 5 % | stays | trust |
| nIN issuer | **unlocked** (Ledger) | lock (master weight 0) after counsel X14 | issuer Ledger |

## 5. Open items

1. Lock the nIN issuer (X14).
2. The daily caps: built in lane B (§3.8), live after the upgrade.
3. External audit of `fund_core`, `fx_pool` and `senior` before any cap rises.
4. Dispute path: built in lane B (union 2-of-3 on-chain).
5. Reflector: built in lane B, live after the upgrade and `set_reflector`.
6. One union is checked by the state rules (A17, A23); a second union is added to the indexer's `INDEXER_UNIONS` when
   it is admitted.
7. An alert that `senior`'s USDC equals its books (§3.11).
8. **Leader OTP** (nila-soroban #29, not on mainnet): the union's SMS key would also open members' sessions
   (`start_session_by_sms`), so §3.4 must cover it before it ships; who holds that key decides how much it protects.

## 6. How to check this file

Reads used (no key needed): Horizon `GET /accounts/<G…>` for signers and thresholds; `stellar contract invoke
--send=no` for `upgrade_delay`, `pending_upgrade`, `is_paused`, `max_scan`, `daily_cap`, `rate` (`fx_pool`), `cap`,
`total`, `paused`, `guarantee_cap` (`senior`), `get_config` (`fund_core`), `union_settings` (`roles`). Contract ids:
[indexer.nila.land/contracts](https://indexer.nila.land/contracts).
