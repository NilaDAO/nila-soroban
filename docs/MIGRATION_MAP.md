# Function-level migration map

Source: TAD v5.6 §7.1. Solidity: `GenericFundCore.sol` (0.8.25, Polygon). Target: `nila-fund-core`.

Legend: **Direct** = same logic. **Adapt** = same shape, one dependency swapped. **Redesign** = Solidity mechanism does not exist on Stellar. **Drop** = not ported.

## GenericFundCore → `fund_core`

| Solidity | Soroban | Note |
| --- | --- | --- |
| `initialize`, `pause`/`unpause`, setters, `setFeeBps` | Direct | `__constructor` + instance storage; admin `require_auth` |
| `setReserveConfigForUnion`, `setBucketThresholds`, `setRateParams` | Direct | Persistent `(union[, loan_type])` keys |
| `depositSenior`, `depositJunior` | Adapt | `membership.is_member`; 7-dec nIN; `U256` share math |
| `requestUnbondSenior/Junior`, `claimSenior/Junior` | Direct | 14-day gate on ledger timestamp |
| `_redeemClaimable` | Adapt | Snap-vs-live payout; i128 + U256 |
| `drawLoanWithVoucher` | Redesign | Oracle `require_auth` (host nonces); terms packed in `DrawParams`; flattened FX call |
| `AcceptLoan` | Adapt | Same flattened FX call |
| `repayLoan` | Adapt | Also callable by FX on cash-scan REPAY; dust = 0.1 nIN at 7 dec |
| `markDefault` | Adapt | `haircut_index` via U256 |
| `reportMilestone`, `removeLoan`, `addJuniorCash`, `withdrawUnionTreasury` | Direct | The deposit direction (`deposit=true`, her own nIN) is `leader_deposit_treasury` (C10) |
| `creditEscrowNin`, `burnTreasuryForExpiry` | Direct | Caller = FX contract `require_auth` |
| `burnEscrowNin` | Drop | Unused by current FX pool |
| Views | Direct | Written explicitly — no auto-getters |
| Events | Direct | `#[contractevent]` / indexer (RPC retention ~7 days) |

## GenericFundViewer → `fund_viewer`

| Solidity | Soroban | Note |
| --- | --- | --- |
| `CreateUnion`, `ActivateUnion`, `AddFundType`, `RemoveFundType` | Direct | `remove_fund_type` swap-removes so ids stay in sync |
| `onLoanCreated`, `onLoanClosed` | Direct | Per-borrower `Vec`, cap 128 |
| `recoverVoucherSigner` | Redesign | Replaced by Stellar auth in Core |
| `recoverQuote1155Signer` | Drop | ERC-1155 module not ported |

## NilaFxPool → `fx_pool` (skeleton this tranche)

Mint/redeem, Reflector TWAP, order books, and cash-scan are Tranche 2. This crate ships constructor, daily-cap storage, temporary escrow, and `resolve_escrow_cash` (no read-back into Core).

Not ported: `NilaPOLSwap`, `GenericFund1155Module`, `NilaNINV2` token code, OpenZeppelin proxies, `GenericFundMathLib` as `delegatecall` (now `nila-math`).
