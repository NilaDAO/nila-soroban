# Test and deploy

## Prerequisites

- Rust 1.84+ with `wasm32v1-none` (`rustup target add wasm32v1-none`)
- [stellar-cli](https://developers.stellar.org/docs/tools/cli/install-cli) for WASM optimize + network deploy (`cargo install --locked stellar-cli`)

The workspace `release` profile matches the Soroban size/safety defaults: `opt-level = "z"`, `overflow-checks = true`, `panic = "abort"`, LTO.

## Test

Unit tests run **natively** (not WASM) with `soroban-sdk` testutils. Resource limits are enforced.

```bash
make test
# or
cargo test --workspace
```

What the suite covers:

| Crate | Assertions |
| --- | --- |
| `nila-math` | Share round-trip, index growth, haircut, 12%/yr interest, U256 large book, kinked rate curve |
| `nila-roles` | Oracle/leader flags; unauthenticated setter fails |
| `nila-membership` | Leader+attester confirm, revoke, plot cap |
| `nila-fund-viewer` | Union registry, swap-remove fund types, borrower index |
| `nila-fx-pool` | Escrow resolve by Core, remaining decrements |
| `nila-fund-core` | Membership gates, junior-first funding, fast-draw + repay, fee split when below min rate, junior first-loss, 14-day unbond, pause, ₹10k cap, remove undrawn loan, cash conservation |

Error paths use `try_*` clients (`Ok(Err(Error::…))`). Time is advanced with `env.ledger().set_timestamp`.

Auth: tests currently `mock_all_auths()`. Before mainnet, add `env.auths()` tree assertions on `draw_loan`, `deposit_*`, and `upgrade` (see stellar testing skill). Property tests for the 30 July 2026 Polygon snapshot belong in Tranche 2.

```bash
make clippy
make fmt
```

## Build WASM

```bash
make build
ls -lh target/wasm32v1-none/release/*.wasm
```

Network limit is 128 KiB per contract. CI fails the job if any artifact exceeds it.

## Local network

```bash
stellar container start local
stellar keys generate nila-admin --network local --fund
NETWORK=local SOURCE=nila-admin NIN_SAC=C... USDC_SAC=C... bash scripts/deploy-testnet.sh
```

RPC: `http://localhost:8000/soroban/rpc`. Passphrase: `Standalone Network ; February 2017`.

## Testnet

```bash
stellar keys generate nila-admin --network testnet --fund
# Issue nIN as AUTH_REQUIRED classic asset (7 decimals); take its SAC address.
export NIN_SAC=C...
export USDC_SAC=C...   # Circle USDC on testnet, or a stand-in SAC
bash scripts/deploy-testnet.sh
```

Testnet resets quarterly. Treat `.stellar/testnet-addresses.env` as ephemeral. Rehearse here; Tranche 1 milestones are **mainnet** at the ₹10,000 cap.

A full rehearsal (identities, SACs, deploy, deposit → draw → repay) is:

```bash
bash scripts/testnet-rehearsal.sh
```

Full evidence table (accounts, all five contracts, both SACs, four tx hashes, explorer links): [STELLAR_TEAM_BRIEF.md](STELLAR_TEAM_BRIEF.md).

Last successful run (7 Sep 2026, Test SDF Network):

| Piece | ID |
| --- | --- |
| `fund_core` | [`CAOIXQ…FSHIU`](https://stellar.expert/explorer/testnet/contract/CAOIXQU7JIIKI3ZLTSZOZFRHJQR5HHCEWILPT77S7773ZV7KFP6FSHIU) |
| nIN SAC | [`CA6C4T…F67BG`](https://stellar.expert/explorer/testnet/contract/CA6C4TU624CC3NH4JI22PIPCSJDDOWIGKWUTRGZV6B2HZBSKBGWF67BG) |
| Draw | [`3b6c3d…1b74f`](https://stellar.expert/explorer/testnet/tx/3b6c3d028b48928d6ad0fcee8d4815bd3bba1d8c0ae8a8178958dd14cd71b74f) |
| Repay | [`759034…44965`](https://stellar.expert/explorer/testnet/tx/7590343c3f9e9f1ef73b119843cba39f570ec9474bc6a586040f89c47d644965) |

## Mainnet (Tranche 1)

1. 3-of-5 multisig as `admin` (no timelock while the cap is ₹10,000).
2. Deploy in order: `roles` → `membership` → `viewer` → `fund_core` → `fx_pool`.
3. Wire `set_core`, `set_roles`, `set_fx_pool`.
4. `set_per_loan_cap` = ₹10,000 (`100000000000`).
5. `set_reserve_config` per union before any draw.
6. Confirm members (`confirm_member`) before junior deposits or borrows.
7. Simulate every invoke (`stellar contract invoke ... --send=no`) and record CPU/IO before submitting.

Upgrade: upload WASM (`stellar contract upload`), then `upgrade --new_wasm_hash <hash>`. Address and state are unchanged. Raise `set_upgrade_delay` to 172800 (48h) when caps increase.

## Indexing

Events are the audit log. RPC typically retains ~7 days — run an indexer (or horizon+soroban ingestion) from first mainnet tx. Event names are the `#[contractevent]` structs in each crate.
