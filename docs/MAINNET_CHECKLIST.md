# Mainnet readiness checklist (C4, 10 Oct 2026)

For the trust's signers and the executive. **Claude Code never deploys to mainnet.** Every step below
is run by the people who hold the keys. Rehearsed on testnet by `scripts/c4-testnet.sh`, using
a 2-of-3 trust account, a timelocked upgrade, and an upgrade that keeps the contract's state.
The C3 flows are rehearsed by `scripts/c3-testnet.sh`.

Mark each box at deploy time, then keep a copy of this file with the transaction hashes.

## A. Code, before anything is uploaded

- [ ] Build from a **tag** on a reviewed branch, never from a working tree. Record the commit.
- [ ] Toolchain pinned by `rust-toolchain.toml` (Rust 1.94.1, `wasm32v1-none`, soroban-sdk
      27.0.2). Build with `stellar contract build`. Record the sha256 of each wasm; that sha256
      is the on-chain wasm hash.
- [ ] **Reproducible build.** Each signer rebuilds from the tag on their own machine, or in a
      pinned container, and gets the same sha256 before signing a deploy or an `upgrade(hash)`.
      Write the build command down once and reuse it. CI's plain `cargo build` is a size gate,
      not the release build.
- [ ] CI is green: fmt, clippy `-D warnings`, all tests, 128 KiB size gate, and the
      **no testnet-short** step. `deploy-testnet.sh` refuses those builds too. Check the wasm
      you upload yourself: `LC_ALL=C grep -a -l testnet-short *.wasm` must print nothing.
- [ ] `storage_layout_is_frozen` passes in all five contracts, with the hashes as pinned in
      `docs/STORAGE_LAYOUT.md`.
- [ ] Reviewer verdicts for C1–C4 are on file (`nila-design/contracts/changelog.md`).
- [ ] External audit of fund_core and fx_pool **before caps rise** above launch level
      (executive decision, 10 Oct). Launch at the ₹10,000 caps without it.

## B. Accounts and keys

- [ ] **Trust admin account (D2, C11).** It is the admin of all five contracts.
  - At least 3 signers on hardware keys (Ledger supports Stellar), weight 1 each.
  - Low, medium and high thresholds at the majority. Master weight 0.
  - **Nila people hold fewer signatures than the threshold** (C11, I6). ContractsCompliance K1
    plans 3-of-5 with three Nila operating signers; that breaks C11 unless X3 changes. **Open
    for executive decision (§F).**
  - Record the signer list. One signature and a plain key must fail (rehearsed in
    `c4-testnet.sh` with 2-of-3, including the trust's own master key).
- [ ] **Union accounts (D12).** One account per union: 5 signers, threshold 3, master weight 0.
      It appoints its leaders in roles.
- [ ] **nIN issuer = the trust (D7).** The legal side (X14) must be settled first. Then:
  - Flags: auth required, revocable, clawback enabled (R8).
  - **Set `home_domain` and publish `stellar.toml` before the lock.** After the lock the issuer
    can sign nothing.
- [ ] Oracle and attester accounts: keys on hardware or in SSM, never in a repo.
- [ ] Fee-bump sponsor for member transactions (R12), funded.

## C. Deploy order (each step signed by the account named)

0. Issuer: set the flags (auth required, revocable, clawback), set `home_domain`, and deploy
   the nIN SAC. Its address is a constructor argument below.
1. Deployer: `roles(admin = trust)`.
2. Deployer: `land_title(admin = trust, roles)`.
3. Deployer: `fund_viewer(admin = trust)`.
4. Deployer: `fund_core(admin = trust, roles, land_title, nin_sac)`.
5. Deployer: `fx_pool(admin = trust, roles, land_title, nin_sac, usdc_sac, daily_cap)`.
6. Trust: `fx_pool.set_fund_core(fund_core)`. **Set once.**
7. Trust: `fund_core.set_fx_pool(fx_pool)`. **Set once.**
8. Issuer: `set_admin(fx_pool)` on the nIN SAC.
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
    - `enforce_token_amount` **off** (executive decision, 10 Oct): the attested amount mimics the field's
      yield, not a loan limit.
15. Attester: `land_title.set_title` for each existing Polygon title (F1, the move).

## D. Verify after deploy (read-only)

- [ ] `upgrade_delay` = 172800 (48 h) on all five; `pending_upgrade` and `pending_delay` = null.
      The delay is stored at the constructor and survives upgrades, so check it on the
      deployed contract itself.
- [ ] `admin` (roles, land_title, fund_viewer, fx_pool) and `get_config().admin` (fund_core) =
      the trust account.
- [ ] The nIN SAC `admin` = fx_pool. The issuer has master weight 0 and no other signers.
- [ ] fx_pool `fund_core` and fund_core `fx_pool` point at each other.
- [ ] `is_paused` = false. `max_scan` = 10,000 nIN. `daily_cap` = 2,500 USDC: it caps the USDC
      Bring offers pay out in a day.
- [ ] LP offers: `anchor` = the trust's anchor account (nila-anchor), `lp_fee` = 100 (1%),
      `voucher_floor` per union as decided.
- [ ] The on-chain wasm (`stellar contract fetch`) sha256 equals the recorded build hash, for
      each contract.
- [ ] One small end-to-end run per union, with real people:
  - treasury scan, invest scan, draw, repay scan;
  - a cash-out and a pay-out.

  fund_core nIN must equal junior + senior + treasury + rainy, and fx_pool must hold no nIN
  outside open cash-outs.

## E. Operations

- **Events.** RPC keeps events for 7 days only, so an indexer must run from the first ledger
  (C19, Q36 open). Topics on-chain are snake_case. Alert on at least these:
  - every upgrade step: `upgrade_proposed`, `upgrade_applied`, `upgrade_cancelled`,
    `upgrade_delay_set`;
  - admin and roles: `admin_set`, `role_updated`, `leader_updated`, `union_role_updated`;
  - caps and wiring: `max_scan_set`, `daily_cap_set`, `fx_pool_set`, `fund_core_set`,
    `activated`, `union_config_set`;
  - state: `paused`, `escrow_expired`, `loan_defaulted`.
- **Pause.** The trust signs `pause`.
  - **fund_core** stops every money path, and expiry burns stop with it. The public
    `mark_default` stops too (D-d), so a long pause delays defaults.
  - **fx_pool** stops scans, cash-outs, pay-outs and expiry burns. Reclaim of her own nIN
    stays open.
  - **land_title** stops title writes (`set_title`, `set_record`, `burn_title`).
  - **roles and fund_viewer have no pause**; they move no money.
- **Upgrade.**
  1. Upload the wasm and check the layout guard in that commit.
  2. Trust: `upgrade(hash)`. Filing again replaces the earlier filing; nothing ever applies
     in the same call.
  3. Announce it, and let the indexer alert on `UpgradeProposed`.
  4. After 48 h, trust: `execute_upgrade`. `cancel_upgrade` stops it at any point.
  5. Lowering the delay waits out the current delay, and must then be applied within 7 days.
     Any raise or `cancel_upgrade` voids a filed lowering. The delay is never above 30 days.
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
- C14: the hard maximum rate in code is **12.5%** (`MAX_RATE_BP = 1250`, executive decision 10 Oct);
  `set_union_config` refuses more. The legal maximum notified under TN Money-Lenders Act s.7
  is still unsourced (legal memo Q8; counsel to produce the G.O.). Raise the cap only by a
  timelocked upgrade once it is confirmed.
- **Signer roster (X3, C11).** Decided (executive decision, 10 Oct): 3-of-5 with at most two Nila signers.
  The names are still open (X3).
- **Recovery.**
  - The issuer is locked and fx_pool is the nIN SAC admin, so replacing fx_pool as SAC admin
    needs an fx_pool upgrade (48 h).
  - Only roles has `set_admin`. Replacing the trust account on the other four contracts needs
    an upgrade of each.
  - Losing the trust quorum freezes all of it. Decided (executive decision, 10 Oct): signer hygiene, with
    more signers than the threshold, hardware keys kept apart and a written key-loss
    procedure. No contract `set_admin`.
- Q36: the indexer. Q34: which button calls which function. X22: what a pay-out releases.
- In this release (Nov 7): USDC through the senior route and the LP offers (C5b, C9); the anchor
  (SEP-1/10/12/24/38) is the trust's own, in nila-anchor.
- T2 work is not in this release: senior payouts (X15, R46–R47) and Reflector (R77).

## G. Storage layout

Frozen at C4; see `docs/STORAGE_LAYOUT.md`. A layout change after mainnet needs a migration,
written and rehearsed on testnet first.
