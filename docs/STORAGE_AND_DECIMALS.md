# Storage, TTL, and decimals

Tranche 0 design note for the Nila Soroban facility. Matches TAD v5.6 §4.3 and §7.1.

## Decimals

| Asset | Polygon | Stellar |
| --- | --- | --- |
| nIN | ERC-20, 18 decimals | Classic asset + SAC, **7 decimals** |
| Hard currency | USDT | Native USDC (Circle issuer), 7 decimals |

Share-index math is Maker-style RAY (`1e27`). On 18-dec nIN, `amount × RAY` overflows `i128`. At 7 decimals a ₹8.2M book (`8.2e6 × 1e7`) still overflows a naive `i128` product, so every `amount × RAY / index` path goes through `U256` in `nila-math`. Dust-close is `0.1 nIN` = `1_000_000` atomic units. The Tranche-1 per-loan cap is ₹10,000 = `100_000_000_000` atomic units (`DEFAULT_PER_LOAN_CAP`).

Bucket junior/senior **ratio** stays on WAD (`1e18`); that is a ratio, not an amount.

## Storage layout

Fine-grained persistent keys so unrelated unions/investors do not serialize on the same ledger entry.

| Data | Type | Key | TTL |
| --- | --- | --- | --- |
| Admin, token addresses, fees, cap, pause, upgrade delay | instance | `Config`, `Paused`, `PendingUpgrade` | Extended on every mutating call (`TTL_THRESHOLD` 30d → `TTL_BUMP` 120d) |
| Reserve / rate params | persistent | `Reserve(union)`, `RateParams(union)` | Bump on write |
| Markets | persistent | `SeniorMarket(union)`, `JuniorMarket(union, loan_type)` | Bump on write |
| LP positions | persistent | `SeniorInv(union, investor)`, `JuniorInv(union, loan_type, investor)` | Bump on deposit, waterfall, unbond, claim |
| Loans | persistent | `Loan(union, loan_id)` | Bump on lifecycle; archived after close |
| Maturity ledger | persistent | `ScheduledPrincipal(union, ts)`, `MaturedCredit(union)`, … | Bump on report/credit |
| Membership | persistent | `Member(union, account)` | Bump on confirm |
| Cash escrow (fx-pool) | temporary | `Escrow(id)` | TTL aligned with escrow timeout; deleted on expiry |

Anyone can extend TTL via `ExtendFootprintTTLOp`. Expiry is **not** a security control. Escrows store an explicit `expires` timestamp and check it.

## Caps and upgrades

- Launch cap ₹10,000 per loan, readable via `get_config`.
- `upgrade` is admin-gated. `upgrade_delay == 0` applies immediately (Tranche 1). Raising the delay engages the 48-hour timelock (`propose` via `upgrade`, `execute_upgrade` after `eta`).
- Wasm swap keeps address and storage. `__constructor` does not re-run. Schema changes must add `DataKey` variants, never reshape existing ones.

## Authorization substitutions (TAD §7.1)

| Solidity | Soroban |
| --- | --- |
| `transferFrom` | `TokenClient::transfer` under the actor's `require_auth` |
| EIP-712 voucher | `borrower.require_auth()` + `oracle.require_auth()`; host-managed nonces |
| `onlyOwner` / `onlyRole` | stored admin + `RolesClient` |
| ERC-721 `balanceOf` | `MembershipClient::is_member` |
| UUPS | `env.deployer().update_current_contract_wasm` |
| `ReentrancyGuard` | omitted — host rejects re-entry |
| `block.timestamp` | `env.ledger().timestamp()` |
| mappings | persistent keys + `extend_ttl` |
| Core ↔ FX callback | flattened: Core passes `amount`; FX does not read back |
