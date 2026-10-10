# Storage layout (frozen at C4, 10 Oct 2026)

What each contract keeps on-chain, and where. An upgrade swaps the code and keeps all storage,
so the new code must decode every key and value below exactly as stored.

**The guard.** Each contract has a `storage_layout_is_frozen` test. It hashes the XDR of one
sample of every stored key, every stored struct, and every variant of the stored enums
(`Role`, `UnionRole`, `Purpose`, `Tranche`). It does **not** see a change in the type of a
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
- Nothing uses temporary storage. Escrows and cash-outs are persistent, because a lost escrow
  would break the cash-at-hand books.

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

## fund_viewer

| Key | Tier | Value |
|---|---|---|
| `Admin` | instance | Address |
| `Union(union)` | persistent | `UnionRecord{id, display_name, active, created_ts}` |

## fund_core

| Key | Tier | Value |
|---|---|---|
| `Config` | instance | `Config{admin, roles, land_title, nin, fx_pool: Option}` |
| `Paused` | instance | bool |
| `UnionCfg(union)` | persistent | `UnionConfig{max_loan, gate, rate_bp, treasury_fee_bp, rainy_fee_bp, safety_bp, safety_floor, hard_stop, enforce_token_amount}` |
| `JuniorMarket(union)`, `SeniorMarket(union)` | persistent | `Market{cash, index, total_shares, total_borrows, claimable_principal, exists}` |
| `JuniorInv(union, who)`, `SeniorInv(union, who)` | persistent | `Investor{shares, pending, claimable, request_ts, cost, pending_principal_snap}` |
| `Loan(union, loan_id)` | persistent | `Loan` (nila_common): borrower, token_id, principal, rate_bp, drawdown_ts, maturity_ts, principal_paid, interest_paid, interest_accrued, last_accrual_ts, overdue, defaulted, closed, milestone, milestone_digest, digest_ts |
| `LoansOf(union, member)` | persistent | Vec of open loan ids (max 32) |
| `UnionBorrows`, `UnionTreasury`, `UnionRainy` (union) | persistent | i128 |
| `OnHold(union)` | persistent | u32 overdue loans |
| `FoodToken(union, token_id)` | persistent | `FoodToken{member, amount, from_date, loan_id: Option}` |

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

## Layout changes before the freeze

These are not upgrade-compatible with earlier testnet deployments, which were all fresh:

- **C3:** `Loan.escrow_id` and `UnionConfig.escrow_duration` were removed.
- **C4:** `Config.upgrade_delay` was removed. Each contract's own upgrade keys gave way to the
  shared `Upg*` keys.
