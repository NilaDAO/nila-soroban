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
- Temporary storage holds only fx_pool's USDC paid out per day (`DayOut`), which matters for one
  day. Escrows and cash-outs are persistent, because a lost escrow would break the cash-at-hand
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
| `Loan(union, loan_id)` | persistent | `Loan` (nila_common): borrower, token_id, principal, rate_bp, drawdown_ts, maturity_ts, principal_paid, interest_paid, interest_accrued, last_accrual_ts, overdue, defaulted, closed, milestone, milestone_digest, digest_ts |
| `LoansOf(union, member)` | persistent | Vec of open loan ids (max 32) |
| `UnionBorrows`, `UnionTreasury`, `UnionRainy` (union) | persistent | i128 |
| `OnHold(union)` | persistent | u32 overdue loans |
| `FoodToken(union, token_id)` | persistent | `FoodToken{member, amount, hours, from_date, loan_id: Option}`: amount and hours are the plan's recommendations, from_date the season's SOS (N5) |
| `Vouchers(union, loan_id)` | persistent | `VoucherBook{hours, earmark, earmark_per_hour, senior_out, dues: Vec<Due{owner, amount}>}` (C5) |
| `SeniorEarmark(union)` | persistent | i128 senior cash held back for unclaimed voucher hours |
| `OwnerBalance(union, owner)` | persistent | i128 owner's settled pending parts, ready to collect |

## machinery

| Key | Tier | Value |
|---|---|---|
| `Admin`, `Roles`, `LandTitle`, `FundCore`, `Paused`, `NextMachine` | instance | Address ×4, bool, u64 |
| `Rate(union, machine_type)` | persistent | `RateParts{driver, diesel, maintenance, insurance, depreciation, margin}` per hour, set by the union account (R57) |
| `Types(union)` | persistent | Vec<u32> machine types with a rate (max 16) |
| `PrepayDriverBp(union)` | persistent | u32 share of the driver part paid at the job (X32) |
| `Machine(id)` | persistent | `Machine{union, owner, machine_type, hours_per_week, photos_hash, confirmed, on_hold, hold_reason}` |
| `Voucher(union, loan_id)` | persistent | `Voucher{member, hours, used, void, code_hash: Option, code_max, code_expires}` |
| `WeekUse(machine_id, week)` | persistent | u32 hours worked that week |

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

## Layout changes before the freeze

These are not upgrade-compatible with earlier testnet deployments, which were all fresh:

- **C3:** `Loan.escrow_id` and `UnionConfig.escrow_duration` were removed.
- **C4:** `Config.upgrade_delay` was removed. Each contract's own upgrade keys gave way to the
  shared `Upg*` keys.
- **C5 (12 Oct):** `Config.machinery` and `FoodToken.hours` were added to stored structs, and fx_pool,
  fund_core and the new machinery contract gained keys. C4 testnet deployments cannot be upgraded to
  C5; redeploy.
