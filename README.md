# Nila on Stellar

Soroban port of the live Polygon lending facility (`GenericFundCore.sol`) for [SCF #45](https://github.com/NilaDAO). nIN only — the fund never holds USDC.

MIT. Protocol 27 / `soroban-sdk` 27.0.2.

## Contracts

| Crate | Role |
| --- | --- |
| `nila-math` | RAY index, U256 `mul_div`, haircut, simple interest (was `GenericFundMathLib`) |
| `nila-common` | Shared types, TTL policy, cross-contract clients |
| `nila-roles` | One role list: oracle / attester / core (admin), leaders and treasurer / secretary per union (signed by the union account, D12) |
| `nila-membership` | Leader + attester membership, plots, per-cycle reattest, title record hashes; replaces ERC-721 `balanceOf` |
| `nila-fund-core` | Deposits, 14-day unbond, junior-first loans, waterfall, first-loss |
| `nila-fund-viewer` | Union registry, borrower index |
| `nila-fx-pool` | Tranche 0 skeleton: escrow + flattened `resolve_escrow_cash` |

nIN is a **classic Stellar asset** (7 decimals, `AUTH_REQUIRED`), not a custom token contract.

## Quick start

```bash
rustup target add wasm32v1-none
cargo test --workspace
make build    # stellar contract build, or cargo wasm fallback
```

How to test, deploy, and raise caps: [docs/TEST_AND_DEPLOY.md](docs/TEST_AND_DEPLOY.md).

Storage / TTL / decimals: [docs/STORAGE_AND_DECIMALS.md](docs/STORAGE_AND_DECIMALS.md).

Function map vs Solidity: [docs/MIGRATION_MAP.md](docs/MIGRATION_MAP.md).

## Design constraints (from the TAD)

- One `fund_core` serves all unions, keyed by union address.
- Junior is first-loss and first-funded; senior is not a claim token.
- Loan voucher is Stellar `require_auth` on the oracle, not EIP-712.
- Core → FX calls are one-way. FX never reads back into Core.
- Host rejects re-entry; there is no `ReentrancyGuard`.
- Launch cap ₹10,000 / loan; upgradeable in place; 48h timelock when caps rise.

## Status

This tree is the Tranche 0/1 facility: compiling contracts, unit tests, storage design, and a testnet deploy script. Reflector pricing, full FX mint/redeem, CCTP, and the Polygon-book replay are later tranches.

Shareable write-up for Stellar / SCF reviewers (contract IDs, tx hashes, TAD v5.6 map): [docs/STELLAR_TEAM_BRIEF.md](docs/STELLAR_TEAM_BRIEF.md).
