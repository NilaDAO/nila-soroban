# Storage layout (frozen at C4, 10 Oct 2026)

What each contract keeps on-chain, and where. An upgrade swaps the code and keeps all storage,
so the new code must decode every key and value below exactly as stored.

**The guard.** Each contract has a `storage_layout_is_frozen` test. It hashes the XDR of one
sample of every stored key, every stored struct, and every variant of the stored enums
(`Role`, `UnionRole`, `Purpose`, `Tranche`, `Tag`, `OfferKind`, `OfferState`). It does **not** see a change in the type of a
plain stored value (an `i128` becoming a `u64`, a `Vec`'s element type, a `bool` flag). Treat
the value columns below as frozen too, and review them by hand in any upgrade. Renaming, reordering, adding or removing a field or a
`DataKey` variant changes the hash and fails CI. When it fails, do one of two things:

- **Keep the shape.** Put new data under a **new** key (a new `DataKey` variant at the end is
  safe), or in a new struct stored under a new key.
- **Migrate.** Write a migration that runs in the upgraded code. Read the old shape under a
  versioned key, write the new one, then re-pin the hash in the same commit. Write down why.

Every contract also holds the shared upgrade keys from `nila_common::upgrade`, in instance
storage:
- `UpgDelay` (u64);
- `UpgPending` (`PendingUpgrade{wasm_hash, eta}`);
- `UpgPendingDelay` (`PendingDelay{delay, eta}`).

## TTL policy (`nila_common`)

- **Instance** storage, which also covers the contract's wasm code, is extended to 120 days
  whenever it falls below 30. This happens on every write call (`bump_instance`).
- **Persistent** entries are extended the same way when they are written, and also when the
  contract reads them on paths that bump: titles, loans, investors and markets.
- An entry left idle beyond its TTL is **archived, not deleted**. Since Protocol 23, the next
  transaction that touches it restores it automatically; the restore costs a fee. See the
  checklist §E.
- Temporary storage holds only counters that matter for a short window: fx_pool's USDC paid out per
  day (`DayOut`) and machinery's hours per machine per week (`WeekUse`). The network deletes them when
  they expire, with no restore, so nothing that holds money or prevents a double payment is temporary. Escrows and cash-outs are persistent, because a lost escrow would break the cash-at-hand
  books.

## roles

| Key | Tier | Value |
|---|---|---|
| `Admin` | instance | Address (the trust account) |
| `Role(account, Role)` | persistent | bool (oracle, attester, core) |
| `Leader(union, account)` | persistent | bool |
| `UnionRole(union, account, UnionRole)` | persistent | bool (treasurer, secretary) |

## land_title

| Key | Tier | Value |
|---|---|---|
| `Admin`, `Roles`, `Paused` | instance | Address, Address, bool |
| `Title(u64 polygon_token_id)` | persistent | `Title{holder, union, polygon_holder: BytesN<20>, record_hash, metadata_hash, updated_ts}` |
| `Count(union, holder)` | persistent | u32 titles held from that union (membership) |
| `AnyCount(holder)` | persistent | u32 titles held in any union (a member somewhere: never a senior investor) |

## fund_viewer

| Key | Tier | Value |
|---|---|---|
| `Admin` | instance | Address |
| `Union(union)` | persistent | `UnionRecord{id, display_name, active, created_ts}` |

## fund_core

| Key | Tier | Value |
|---|---|---|
| `Config` | instance | `Config{admin, roles, land_title, nin, fx_pool: Option, machinery: Option}` |
| `Paused` | instance | bool |
| `UnionCfg(union)` | persistent | `UnionConfig{max_loan, gate, rate_bp, treasury_fee_bp, rainy_fee_bp, safety_bp, safety_floor, hard_stop, enforce_token_amount}` |
| `JuniorMarket(union)`, `SeniorMarket(union)` | persistent | `Market{cash, index, total_shares, total_borrows, claimable_principal, exists}` |
| `JuniorInv(union, who)`, `SeniorInv(union, who)` | persistent | `Investor{shares, pending, claimable, request_ts, cost, pending_principal_snap}` |
| `Milestone(union, token_id)` | persistent | `Milestone{milestone, digest, ts}`: written only when the oracle reports one |
| `LoansOf(union, member)` | persistent | Vec of open loans' token ids (max 32) |
| `Union(union)` | persistent | `UnionBook{borrows, treasury, rainy, earmark, on_hold}`: one entry for the union's counters (C6) |
| `FoodToken(union, token_id)` | persistent | `FoodToken{member, amount, hours, from_date, loan: LoanState}`. Amount and hours are the plan's recommendations; from_date is the opening of its window since 25 Oct, attested: a week before its crop's sowing window in that season; a token attested before keeps its SOS or six weeks before its last date. **The loan lives here** (C6): `LoanState{principal, rate_bp, drawdown_ts, maturity_ts, principal_paid, interest_paid, interest_accrued, last_accrual_ts, flags}`, flags = overdue 1, defaulted 2, closed 4, drawn 8. The token id is the loan id. |
| `FoodTokenFinal(union, token_id)` | persistent | `FoodTokenFinal{final_date, lapsed}` (25 Oct): the last draw date (`draw_loan` refuses after it, `TooLate`) and whether the token lapsed unused (`lapse_food_token`, anyone, after the last date). A token without it has no last date (attested before 25 Oct). |
| `Vouchers(union, token_id)` | persistent | `VoucherBook{hours, per_hour, value, repaid, interest_accrued, interest_paid, last_accrual_ts, reserve, fee_due}` (26 Oct: the voucher's value is senior's loan from the draw, with interest; `reserve` pays owners in full at the claim, also after repayment; `fee_due` is 5% of handed-back hours, the union treasury's) |
| `OwnerBalance(union, owner)` | persistent | i128 owner's balance to collect (no longer credited since 26 Oct: owners are paid at the claim) |
| `CropCalendar` | instance | Address of the crop_calendar contract (25 Oct) |

## crop_calendar (25 Oct)

| Key | Tier | Value |
|---|---|---|
| `Admin` | instance | Address (the trust) |
| `Windows(family)` | persistent | `Vec<Window{first, last}>`: a crop family's sowing windows, months 1–12 (`first > last` runs over the new year); never overlapping |
| `Roles` | instance | Address of the roles contract (26 Oct): who leads a union, for `set_price` |
| `Price(union, family)` | persistent | `Price{paise_per_kg, at}`: the union's local selling price of a crop family, set by a leader (26 Oct) |
| upgrade keys | instance | `nila_common::upgrade` (48-hour timelock) |

## machinery

| Key | Tier | Value |
|---|---|---|
| `Admin`, `Roles`, `LandTitle`, `FundCore`, `Paused`, `NextMachine` | instance | Address ×4, bool, u64 |
| `Rate(union, machine_type)` | persistent | `RateParts{driver, diesel, maintenance, insurance, depreciation, margin}` **per acre** (27 Oct; was per hour), set by the union account (R57) |
| `CropRate(union, machine_type, crop_family)` | persistent | `RateParts` per acre for one crop family (27 Oct; optional, else `Rate`) |
| `Types(union)` | persistent | Vec<u32> machine types with a rate (max 16) |
| `PrepayDriverBp(union)` | persistent | u32 share of the driver part paid at the job (X32) |
| `Machine(id)` | persistent | `Machine{union, owner, machine_type, acres_per_week, photos_hash, confirmed, on_hold, hold_reason}` |
| `Voucher(union, token_id)` | persistent | `Voucher{member, units, used, void, code_hash: Option, code_max, code_expires}` (the three code fields unused since the signed claim, 23 Oct; kept for the layout) |
| `WeekUse(machine_id, week)` | **temporary** | u32 centi acres worked that week; lives ~8 days, then the network deletes it (C7) |

## fx_pool

| Key | Tier | Value |
|---|---|---|
| `Admin`, `Roles`, `LandTitle`, `Usdc`, `Nin`, `FundCore` | instance | Address |
| `DailyCap`, `MaxScan` | instance | i128 |
| `Paused` | instance | bool |
| `NextEscrow` | instance | u64 |
| `Escrow(id)` | persistent | `Escrow{union, member, purpose, amount, remaining, expires}` |
| `EscrowsOf(union)` | persistent | Vec of open escrow ids, oldest first (max 64) |
| `CashAtHand(union)` | persistent | i128 |
| `Scan(receipt_hash)` | persistent | bool (a receipt is used once) |
| `CashOut(code_hash)` | persistent | `CashOut{member, union, amount, expires}` |
| `CodeUsed(code_hash)` | persistent | bool (a code is closed for good) |
| `SeniorInvestor(investor)` | persistent | `SeniorInvestor{accepted, kyc_ref}`: a senior investor the trust accepted (never a member; not an LP) |
| `Rate` | instance | `RateFix{inr_per_usd, valid_until, set_by}` (R77) |
| `SeniorUsdc(union, Tag)` | persistent | i128 USDC taken in, per tag offshore / domestic (R24) |
| `VoucherFloor(union)` | persistent | i128 senior USDC kept back as the voucher guarantee |
| `Anchor` | instance | Address (the trust's anchor account) |
| `LpOf(union, lp)` | persistent | `LpMember{accepted, kyc_ref}`: a member LP the anchor accepted (R20) |
| `LpUsdc(union)` | persistent | i128 USDC LPs brought in through Collect offers |
| `LpFeeBp`, `NextOffer` | instance | u32, u64 |
| `Offer(id)` | persistent | `Offer{union, kind, inr, expires, state, lp, usdc, inr_per_usd, sep24_id, quote_id, from_lp, from_domestic, from_offshore}` |
| `OffersOf(union)` | persistent | Vec of open offer ids (max 64) |
| `DayOut(day)` | **temporary** | i128 USDC paid out that day, against the daily cap |
| `RateBandBp` | instance | u32 how far the attester's fix may move (default 500 = 5%) |
| `SwapKey` | instance | BytesN<32> the anchor's ed25519 swap key; `accept` needs its signature (C9b) |

## Layout changes before the freeze

These are not upgrade-compatible with earlier testnet deployments, which were all fresh:

- **C3:** `Loan.escrow_id` and `UnionConfig.escrow_duration` were removed.
- **C4:** `Config.upgrade_delay` was removed. Each contract's own upgrade keys gave way to the
  shared `Upg*` keys.
- **C5 (12 Oct):** `Config.machinery` and `FoodToken.hours` were added to stored structs, and fx_pool,
  fund_core, land_title (`AnyCount`, no back-fill) and the new machinery contract gained keys. C4
  testnet deployments cannot be upgraded to C5; **redeploy all of them**. An in-place land_title upgrade
  would read every existing holder as holding no title anywhere, which would let members in as senior.
- **C6 (12 Oct, executive decision):** less rent per call. The loan moved into its food token entry: no separate
  `Loan(union, loan_id)`, no copy of the borrower or token id, three flags packed into one number, and
  the token id is the loan id. Five union counters became one `Union` entry. Milestones moved to their
  own entry, written only when reported. Redeploy; no upgrade from C5.
- **C8 (16 Oct, the move):** fx_pool gained two keys, `Moved(BytesN<32>)` (persistent, a Polygon burn
  already minted on Stellar) and `MoveBudget` (instance, i128: what the attester may still mint for the move). Appended only: existing keys and values encode as before, so a C7 fx_pool
  can be upgraded in place. The frozen-layout hash changed only because the sample list grew.
- **Acre vouchers (27 Oct, executive decision):** machinery counts **acres**, not hours: `Machine.hours_per_week` →
  `acres_per_week`, `Voucher.hours` → `units` (centi tractor acres), `WeekUse` counts centi acres, `Rate` is per acre,
  and the new key `CropRate(union, type, family)`. Field names are part of the stored encoding, so a machinery holding
  machines or vouchers **cannot be upgraded in place: redeploy** (testnet only; nothing on mainnet). fund_core's
  layout is unchanged (it calls `value_per_tractor_acre`).
- **C9b (19 Oct, the anchor's signed accept):** fx_pool gained one key, `SwapKey` (instance, BytesN<32>: the
  anchor's ed25519 swap key that signs each LP accept, C16). Appended only; a C8 fx_pool can be upgraded in
  place. `accept` takes three more arguments (`usdc`, `valid_until`, `sig`): callers change, storage does not.
  The frozen-layout hash changed only because the sample list grew.
- **Final date (25 Oct, last draw date and lapse):** fund_core gained one key, `FoodTokenFinal(union, token_id)`
  (persistent). Appended only: `FoodToken` entries keep their layout (its `from_date` stays and now holds the window's opening), so a
  deployed fund_core can be upgraded in place. A field added to `FoodToken` would have broken decoding of the food
  tokens already stored, which is why the last date has its own key. `food_token` returns `FoodTokenView` (the stored
  fact plus `final_date`, `lapsed`).
- **Crop calendar (25 Oct, same day):** `attest_food_token` takes neither date any more: fund_core reads the window
  (opening, last date) from the new `crop_calendar` contract by the token id's crop code and sowing date. fund_core
  gained one instance key, `CropCalendar` (the calendar's address, set by the admin with `set_crop_calendar`), apart
  from `Config` so the stored config keeps its layout. Until it is set, and the families' windows are set, every
  attestation is refused (`BadDates`). The frozen-layout hash changed only because the sample list grew.

## account (26 Oct): every Nila account (members, leaders, machine owners, LPs)

| Key | Tier | Value |
|---|---|---|
| `Factory` | instance | Address of the union's account factory (settings) |
| `Union` | instance | Address of the account's union (its leaders may run sessions and recover) |
| `Signer` | instance | `Signer::Ed25519(BytesN<32>)` (the Privy Stellar wallet, smartphones) or `Signer::P256(BytesN<65>)` (the keypad's device key) |
| `Pending` | instance | `Pending{signer, at}`: a proposed recovery and when it may take effect |
| `Session` | instance | u64: until when a leader of the union may sign for the account |

29 Oct (key backup, K1a): no stored type changed. A present `Pending` now **freezes** the account: `__check_auth`
refuses every signature (`Frozen`, error 8), `start_session` and `set_signer` are refused, and a proposal removes
`Session`. `cancel_recovery` takes the union's multisig account (the factory's `union`), no longer the account's
signer. Running accounts get the code through the factory's timelocked `propose_account_wasm` / `apply_account_wasm`,
then `upgrade` on each account.

## account_factory (26 Oct): one per union, admin = the union's multisig account

| Key | Tier | Value |
|---|---|---|
| `Union` | instance | Address: the union account (admin; may propose an account's recovery) |
| `Roles` | instance | Address of roles (who leads the union) |
| `SmsKey` | instance | Address of the node's SMS key: with one leader, proposes a new key after the member's SMS link (OTP recovery) |
| `Wait` | instance | u64 seconds, 1 h – 30 days: the recovery wait |
| `SessionLen` | instance | u64 seconds, 1–15 min |
| `SessionContracts` | instance | `Vec<Address>`: what a leader session may call (fund_core, machinery) |
| `AccountWasm` / `PendingWasm` | instance | the account code, and a proposed one with its time |
| upgrade keys | instance | `nila_common::upgrade` (48-hour timelock) |
