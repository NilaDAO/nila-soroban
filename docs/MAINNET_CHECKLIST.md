# Mainnet readiness checklist (C4, 10 Oct 2026)

For the trust's signers and executive decision. **Claude Code never deploys to mainnet.** Every step below
is run by the people who hold the keys. Rehearsed on testnet by `scripts/c4-testnet.sh`, using
a 2-of-3 trust account, a timelocked upgrade, and an upgrade that keeps the contract's state.
The C3 flows are rehearsed by `scripts/c3-testnet.sh`.

Mark each box at deploy time, then keep a copy of this file with the transaction hashes.

## A. Code, before anything is uploaded

- [ ] Build from a **tag** on a reviewed branch, never from a working tree. Record the commit.
- [ ] Toolchain pinned by `rust-toolchain.toml` (Rust 1.94.1, `wasm32v1-none`, soroban-sdk
      27.0.2). Build with `stellar contract build`. Record the sha256 of each wasm; that sha256
      is the on-chain wasm hash.
- [ ] CI is green: fmt, clippy `-D warnings`, all tests, 128 KiB size gate, and the
      **no testnet-short** step. `deploy-testnet.sh` refuses those builds too. Check the wasm
      you upload yourself: `LC_ALL=C grep -a -l testnet-short *.wasm` must print nothing.
- [ ] `storage_layout_is_frozen` passes in all five contracts, with the hashes as pinned in
      `docs/STORAGE_LAYOUT.md`.
- [ ] Reviewer verdicts for C1–C4 are on file (`nila-design/contracts/changelog.md`).
- [ ] **Recommended before caps rise above launch level:** an external audit of fund_core and
      fx_pool.

## B. Accounts and keys

- [ ] **Trust admin account (D2, C11).** It is the admin of all five contracts.
  - At least 3 signers on hardware keys (Ledger supports Stellar), weight 1 each.
  - Low, medium and high thresholds at the majority. Master weight 0.
  - **Nila people hold fewer signatures than the threshold.**
  - Record the signer list. One signature, or any single key, must fail (rehearsed).
- [ ] **Union accounts (D12).** One account per union: 5 signers, threshold 3, master weight 0.
      It appoints its leaders in roles.
- [ ] **nIN issuer = the trust (D7).** The legal side (X14) must be settled first. Then:
  - Flags: auth required, revocable, clawback enabled (R8).
  - **Set `home_domain` and publish `stellar.toml` before the lock.** After the lock the issuer
    can sign nothing.
- [ ] Oracle and attester accounts: keys on hardware or in SSM, never in a repo.
- [ ] Fee-bump sponsor for member transactions (R12), funded.

## C. Deploy order (each step signed by the account named)

1. Deployer: `roles(admin = trust)`.
2. Deployer: `land_title(admin = trust, roles)`.
3. Deployer: `fund_viewer(admin = trust)`.
4. Deployer: `fund_core(admin = trust, roles, land_title, nin_sac)`.
5. Deployer: `fx_pool(admin = trust, roles, land_title, nin_sac, usdc_sac, daily_cap)`.
6. Trust: `fx_pool.set_fund_core(fund_core)`. **Set once.**
7. Trust: `fund_core.set_fx_pool(fx_pool)`. **Set once.**
8. Issuer: deploy the nIN SAC, then `set_admin(fx_pool)` on it.
9. Trust: `fx_pool.activate()`, which authorises the balances of fx_pool and fund_core.
10. Issuer: **lock** with master weight 0 (executive decision, 10 Oct). Without the lock, classic ops still
    mint, authorise and claw back. Check that any issuer transaction now fails.
11. Trust: `roles.set_role` for the oracle and the attester.
12. Each union account: `roles.set_leader`.
13. Trust: `fund_viewer.create_union` for each union.
14. Trust: `fund_core.set_union_config` for each union, at launch terms:
    - `max_loan` ₹10,000
    - `gate` 600
    - `rate_bp` 1250
    - fees 100 + 77
    - **decide `enforce_token_amount`**: it is off while testing.
15. Attester: `land_title.set_title` for each existing Polygon title (F1, the move).

## D. Verify after deploy (read-only)

- [ ] `upgrade_delay` = 172800 (48 h) on all five, and `pending_upgrade` = null.
- [ ] `admin` / `get_config().admin` = the trust account on every contract.
- [ ] The nIN SAC `admin` = fx_pool. The issuer has master weight 0 and no other signers.
- [ ] fx_pool `fund_core` and fund_core `fx_pool` point at each other.
- [ ] `is_paused` = false. `max_scan` = 10,000 nIN. `daily_cap` as set.
- [ ] The on-chain wasm (`stellar contract fetch`) sha256 equals the recorded build hash, for
      each contract.
- [ ] One small end-to-end run per union, with real people:
  - treasury scan, invest scan, draw, repay scan;
  - a cash-out and a pay-out.

  fund_core nIN must equal junior + senior + treasury + rainy, and fx_pool must hold no nIN
  outside open cash-outs.

## E. Operations

- **Events.** RPC keeps events for 7 days only, so an indexer must run from the first ledger
  (C19, Q36 open). Watch at least these:
  - `UpgradeProposed`, `UpgradeDelaySet`, `AdminSet`, `Paused`, `MaxScanSet`, `FxPoolSet`;
  - `EscrowExpired`, `LoanDefaulted`.
- **Pause.** The trust signs `pause`, 2 of 3.
  - fund_core stops every money path, and expiry burns stop with it.
  - fx_pool stops scans, cash-outs and pay-outs. Reclaim of her own nIN stays open.
  - land_title stops title writes.
- **Upgrade.**
  1. Upload the wasm and check the layout guard in that commit.
  2. Trust: `upgrade(hash)`.
  3. Announce it, and let the indexer alert on `UpgradeProposed`.
  4. After 48 h, trust: `execute_upgrade`. `cancel_upgrade` stops it at any point.
  5. Lowering the delay itself also waits 48 h.
- **Caps.** Raise in steps, each evented:
  - fx_pool `set_max_scan` (per scan and per cash-out);
  - per union `max_loan`.
- **TTL and archival.**
  - Contract instances and code stay alive while they are used.
  - Idle persistent entries (an old title, a closed loan) are archived after about 120 days
    and restored automatically, at a fee, the next time a transaction touches them. Nothing
    is lost.
  - Budget for restore fees, or run a monthly `extend` over long-lived keys.
- **Expired cash at hand.** It is covered from the treasury; cash at hand never exceeds the
  treasury (executive decision, 10 Oct).

## F. Open before mainnet (not contract work)

- X14: the trust as issuer and anchor (legal). Q51: what nIN is, legally (counsel).
- C14: rates must stay under the notified maximum.
  - Today `set_union_config` only checks `rate_bp ≤ 100%`.
  - A hard `max_rate` cap in the contract is **not written**. Decide whether one is needed
    before mainnet.
- Q36: the indexer. Q34: which button calls which function. X22: what a pay-out releases.
- T2 work is not in this release:
  - USDC, LP offers and senior payouts (X15, R46–R47);
  - Reflector (R77) and the anchor (SEP-10/12/24/38).

## G. Storage layout

Frozen at C4; see `docs/STORAGE_LAYOUT.md`. A layout change after mainnet needs a migration,
written and rehearsed on testnet first.
