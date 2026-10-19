# Nila Soroban port — technical brief (SCF #45)

**For:** Stellar ecosystem engineers and SCF reviewers  
**Date:** 7 September 2026  
**Network:** Test SDF Network ; September 2015  
**TAD:** Nila Stellar Technical Architecture Document v5.6 (15 August 2026), SCF #45 Integration Track  
**Repo:** this tree. Polygon source: [NilaDAO/nila-contracts](https://github.com/NilaDAO/nila-contracts)

This brief is the shareable record of the Tranche 0/1 facility port. Deeper notes: [function map](MIGRATION_MAP.md), [storage / TTL / decimals](STORAGE_AND_DECIMALS.md), [test and deploy](TEST_AND_DEPLOY.md).

---

## 1. Why this exists

**Nila** is a stablecoin lending facility for smallholder agricultural credit in Tamil Nadu. It runs a Raiffeisen credit-union structure — member deposits, joint liability, member first-loss — on stablecoin rails. It has been live on Polygon PoS since December 2025 (`GenericFundCore.sol` at `0x4173BbaF66A4f9A2705d05B800e8602370366756`). SCF #45 (Integration Track) moves the money layer to Stellar.

**nIN** is the fund’s unit of account: an INR-referenced synthetic rupee (₹1 = 1 nIN). The fund holds nIN only. Hard currency never enters `fund_core`.

A **union** is a registered credit-union cooperative. On-chain it is an address key. Markets, membership, loans, and treasury buckets are all `(union, …)` tuples. One `fund_core` serves every union.

This tree ports Polygon `GenericFundCore` (and its roles / viewer / membership gates) to Soroban. It is the Tranche 0/1 facility: compiling contracts, 24 native unit tests, storage/TTL design, and one testnet rehearsal of deposit → junior-first draw → repay. Anchor Platform, Privy, CCTP, Reflector-priced FX, and the Polygon book replay are later TAD work. They are not claimed here.

---

## 2. Contract set vs TAD §4.3 / §7.1

TAD §4.3 specifies six on-chain pieces: `fund_core`, `fund_viewer`, `fx_pool`, `roles`, `membership`, and nIN as a classic asset (no custom token code). §7.1 classifies each Solidity function as **Direct** (same logic), **Adapt** (same shape, one dependency swapped), **Redesign** (Solidity mechanism does not exist on Stellar), or **Drop**.

| TAD name | Crate / artifact | Status this tranche | §7.1 posture |
| --- | --- | --- | --- |
| `fund_core` | `nila-fund-core` → `nila_fund_core.wasm` | Ported. Deposits, 14-day unbond, junior-first loans, waterfall, first-loss, pause, cap, upgrade. | Core table: Direct / Adapt / Redesign as mapped. `drawLoanWithVoucher` is Redesign. `burnEscrowNin` is Drop. |
| `fund_viewer` | `nila-fund-viewer` → `nila_fund_viewer.wasm` | Ported. Union registry, fund-type list (swap-remove), borrower index (cap 128). | Direct for registry + `on_loan_*`. `recoverVoucherSigner` Redesign. `recoverQuote1155Signer` Drop. |
| `roles` | `nila-roles` → `nila_roles.wasm` | Role list (oracle, attester, core) + per-union leaders and officers. | Admin for trust-wide roles; `union.require_auth()` (3 of 5 account signers, D12) for leaders and officers. |
| `membership` | `nila-membership` → `nila_membership.wasm` | Ported. Leader + attester confirm; `is_member` / `plots_of`. Replaces ERC-721 `balanceOf`. | TAD §4.2 / §7 table “Land-title balanceOf gates → membership”. |
| nIN | Classic asset `nIN:ADMIN` + SAC | Issued on testnet. 7 decimals, `AUTH_REQUIRED`. Not a custom contract. | Direct substitution of `NilaNIN` ERC-20. |
| `fx_pool` | `nila-fx-pool` → `nila_fx_pool.wasm` | **Tranche 0 skeleton only.** Constructor, daily-cap storage, temporary escrow, `open_escrow`, flattened `resolve_escrow_cash`. | Full §7.1 FX table (mint/redeem, Reflector, cash-scan, order books) is **not** implemented. |

Libraries (not deployed): `nila-math` (RAY index, U256 `mul_div`, haircut, simple interest — was `GenericFundMathLib` `delegatecall`), `nila-common` (shared types, TTL policy, client traits).

Not ported, per TAD §7.1: `NilaPOLSwap`, `GenericFund1155Module`, `NilaNINV2` token code, OpenZeppelin proxies.

WASM sizes from `stellar contract build` on 7 September 2026 (optimized, protocol-27 profile). Network limit is 128 KiB. CI fails the job if any artifact exceeds it.

| Artifact | Size (optimized) | Original |
| --- | --- | --- |
| `nila_fund_core.wasm` | 51,152 bytes | 59,548 |
| `nila_fund_viewer.wasm` | 9,133 bytes | 10,465 |
| `nila_membership.wasm` | 7,934 bytes | 9,110 |
| `nila_roles.wasm` | 6,979 bytes | 8,036 |
| `nila_fx_pool.wasm` | 6,776 bytes | 7,843 |

---

## 3. Stellar-native substitutions

These are the TAD §4.3 / §7.1 substitutions as implemented. Common list, stated once.

| Solidity | Soroban |
| --- | --- |
| EIP-712 loan voucher | `borrower.require_auth()` + `oracle.require_auth()` on `draw_loan`. Host-managed nonces. Terms packed in `DrawParams`. Oracle must be flagged in `roles`. |
| ERC-721 `balanceOf` (land title) | `MembershipClient::is_member`. Junior deposit and borrow require membership. Senior deposit rejects members (`NftForbidden`). |
| `transferFrom` / ERC-20 | SEP-41 `TokenClient::transfer` under the actor’s `require_auth`. |
| 18-dec nIN × RAY (`1e27`) | Classic nIN at **7 decimals**. Every `amount × RAY / index` path goes through `U256` in `nila-math`. Bucket **ratios** stay on WAD (`1e18`). Dust-close is `0.1 nIN` = `1_000_000` atomic units. |
| `block.timestamp` | `env.ledger().timestamp()`. |
| mappings | Persistent keys + `extend_ttl`. Instance: 30-day threshold → 120-day bump on every mutating call. Escrows are temporary; expiry is an explicit timestamp, not TTL. |
| `ReentrancyGuard` | Omitted. Host rejects re-entry. |
| Core ↔ FX callback (FX reads `seniorPendingPrincipal` back) | Flattened. Core passes `amount`. FX does not read Core. `escrow_id == 0` skips the call. |
| UUPS | `env.deployer().update_current_contract_wasm`. Address and state unchanged. `__constructor` does not re-run. |
| `onlyOwner` / `onlyRole` | Stored admin + `RolesClient`. |

Design constraints carried from the TAD (also in the repo README):

- Junior is first-loss and first-funded. Senior is contract state, not a claim token.
- Launch cap ₹10,000 per loan (`DEFAULT_PER_LOAN_CAP` = `100_000_000_000` atomic units). Upgradeable in place. 48h timelock when caps rise (`set_upgrade_delay` = 172800).
- Testnet rehearsal used delay 0 (immediate upgrade), matching TAD Tranche 1.

---

## 4. How to reproduce

Prerequisites: Rust 1.84+ (`rust-toolchain.toml` pins 1.94.1), `wasm32v1-none`, [stellar-cli](https://developers.stellar.org/docs/tools/cli/install-cli). This rehearsal used **stellar-cli 28.0.0**. Workspace pin is `soroban-sdk` **27.0.2** (lockfile resolved **27.0.6**, protocol 27).

```bash
rustup target add wasm32v1-none
cargo test --workspace          # 24 native unit tests (soroban-sdk testutils)
make clippy                     # cargo clippy --workspace --all-targets -- -D warnings
make build                      # stellar contract build
bash scripts/testnet-rehearsal.sh
```

`scripts/testnet-rehearsal.sh` generates or funds seven identities, issues `nIN:ADMIN` and a stand-in `nUSD:ADMIN` SAC, deploys in order `roles` → `membership` → `viewer` → `fund_core` → `fx_pool`, wires `set_core` / `set_roles` / `set_fund_core` / `set_fx_pool`, confirms borrower + junior LP, mints, then deposit → fast-draw → repay. It writes `.stellar/testnet-addresses.env` (gitignored; ephemeral).

Redeploy of contracts alone (existing nIN/USDC SACs): `NIN_SAC=C… USDC_SAC=C… bash scripts/deploy-testnet.sh`. See [TEST_AND_DEPLOY.md](TEST_AND_DEPLOY.md).

---

## 5. Testnet evidence (7 September 2026)

All IDs below are from one rehearsal on **Test SDF Network ; September 2015**. Testnet resets quarterly. Treat them as ephemeral. Do not treat `nUSD:ADMIN` as Circle USDC.

Explorer: [stellar.expert testnet](https://stellar.expert/explorer/testnet).

### Accounts

| Role | Address | Explorer |
| --- | --- | --- |
| ADMIN | `GBKVMUPZGZNBIRHI63FAQEZCUCWCEPPKVBLVGKVWQAOB2TJYRFUEHNCY` | [account](https://stellar.expert/explorer/testnet/account/GBKVMUPZGZNBIRHI63FAQEZCUCWCEPPKVBLVGKVWQAOB2TJYRFUEHNCY) |
| ORACLE | `GCVPSSNKCF4IC2UDW4ZGGBNAPIAD6TOIKTKQ4XBRGBGP345UKRMJ6V5M` | [account](https://stellar.expert/explorer/testnet/account/GCVPSSNKCF4IC2UDW4ZGGBNAPIAD6TOIKTKQ4XBRGBGP345UKRMJ6V5M) |
| LEADER | `GAV7IIHS2ALW4RFBY3SCJN6SIDO6WWKS7NEHI2ZHDCVNLUZVBG62XUW4` | [account](https://stellar.expert/explorer/testnet/account/GAV7IIHS2ALW4RFBY3SCJN6SIDO6WWKS7NEHI2ZHDCVNLUZVBG62XUW4) |
| UNION | `GCVKJNMWQQFU36ZG5Y2EKLURHPUL5Q7FVN7QLYZ7QL2XEPKG2B2QT2QY` | [account](https://stellar.expert/explorer/testnet/account/GCVKJNMWQQFU36ZG5Y2EKLURHPUL5Q7FVN7QLYZ7QL2XEPKG2B2QT2QY) |
| BORROWER | `GBCRMBC6VMUOLAMOQLFSXQQDTJCXRRLI4PQV3U5Z47WHHGQOIAR5XWME` | [account](https://stellar.expert/explorer/testnet/account/GBCRMBC6VMUOLAMOQLFSXQQDTJCXRRLI4PQV3U5Z47WHHGQOIAR5XWME) |
| JUNIOR | `GADGMACZOM2ZX5XIKYEAPKGQEPTABNQ43DARHXUQNF4ZROEVJH5SUP3T` | [account](https://stellar.expert/explorer/testnet/account/GADGMACZOM2ZX5XIKYEAPKGQEPTABNQ43DARHXUQNF4ZROEVJH5SUP3T) |
| SENIOR | `GDRMKUEZV3Y7WXKBV3LG4LE2QUXGZAY5AAQ43AHBKWHFAS5JKXBFUE2O` | [account](https://stellar.expert/explorer/testnet/account/GDRMKUEZV3Y7WXKBV3LG4LE2QUXGZAY5AAQ43AHBKWHFAS5JKXBFUE2O) |

### Contracts

| Piece | Contract ID | Explorer |
| --- | --- | --- |
| ROLES | `CDU5OEUY5WQST3DB7OXYTFGTEOKMX24YAUY3SKKK4RMGLOODU5AKLUKU` | [contract](https://stellar.expert/explorer/testnet/contract/CDU5OEUY5WQST3DB7OXYTFGTEOKMX24YAUY3SKKK4RMGLOODU5AKLUKU) |
| MEMBERSHIP | `CDPYFOKVEN7XC5BIEWNDAVPLL3B3WP2ZSOFTJ76DS4RHKZIQY3PV5TT5` | [contract](https://stellar.expert/explorer/testnet/contract/CDPYFOKVEN7XC5BIEWNDAVPLL3B3WP2ZSOFTJ76DS4RHKZIQY3PV5TT5) |
| VIEWER | `CCLC4VOYQQVLLDERZQG57RPGRPC3CDA3YFD2JCUP3ALER5HRVKYFOAOJ` | [contract](https://stellar.expert/explorer/testnet/contract/CCLC4VOYQQVLLDERZQG57RPGRPC3CDA3YFD2JCUP3ALER5HRVKYFOAOJ) |
| FUND_CORE | `CAOIXQU7JIIKI3ZLTSZOZFRHJQR5HHCEWILPT77S7773ZV7KFP6FSHIU` | [contract](https://stellar.expert/explorer/testnet/contract/CAOIXQU7JIIKI3ZLTSZOZFRHJQR5HHCEWILPT77S7773ZV7KFP6FSHIU) |
| FX_POOL | `CBXPDQ4RD2F7DNX3DQ7AUQNXZ5Q3HDMNSRHL5MZ2H5ZXETJPOA2GYT56` | [contract](https://stellar.expert/explorer/testnet/contract/CBXPDQ4RD2F7DNX3DQ7AUQNXZ5Q3HDMNSRHL5MZ2H5ZXETJPOA2GYT56) |
| nIN SAC | `CA6C4TU624CC3NH4JI22PIPCSJDDOWIGKWUTRGZV6B2HZBSKBGWF67BG` | [contract](https://stellar.expert/explorer/testnet/contract/CA6C4TU624CC3NH4JI22PIPCSJDDOWIGKWUTRGZV6B2HZBSKBGWF67BG) |
| USDC stand-in SAC | `CAYGE5VWVB3MPNBP5V2AC7HOP3H277V6KFYZUTYVSORJDN22VPAI7MWJ` | [contract](https://stellar.expert/explorer/testnet/contract/CAYGE5VWVB3MPNBP5V2AC7HOP3H277V6KFYZUTYVSORJDN22VPAI7MWJ) |

nIN SAC wraps classic asset `nIN:ADMIN`. The USDC stand-in wraps `nUSD:ADMIN`. It is a test issuer asset, not Circle USDC (`GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN` on mainnet per TAD §4.3).

### Lifecycle transactions

Rehearsal amounts (7 decimals): junior deposit 25 nIN (`250000000`), senior deposit 75 nIN (`750000000`), fast-draw 10 nIN (`100000000`) at `rate_bp = 1200`, `min_rate_bp = 1000`, `fast_draw = true`, `escrow_id = 0`.

Loan keys used by the script:

- `loan_type` = `1100000000000000000000000000000000000000000000000000000000000000`
- `loan_id` = `4200000000000000000000000000000000000000000000000000000000000000`

| Step | Hash | Note | Explorer |
| --- | --- | --- | --- |
| DEPOSIT_JUNIOR | `90253e328a957db774d1b7f647502dd773bc997187b86395a52d4a30e27c5291` | 25 nIN | [tx](https://stellar.expert/explorer/testnet/tx/90253e328a957db774d1b7f647502dd773bc997187b86395a52d4a30e27c5291) |
| DEPOSIT_SENIOR | `9998a17db9e89d7e70704874aa79862b67e1996846e573ec4cf00d420c09f943` | 75 nIN | [tx](https://stellar.expert/explorer/testnet/tx/9998a17db9e89d7e70704874aa79862b67e1996846e573ec4cf00d420c09f943) |
| DRAW | `3b6c3d028b48928d6ad0fcee8d4815bd3bba1d8c0ae8a8178958dd14cd71b74f` | 10 nIN fast-draw, junior-first | [tx](https://stellar.expert/explorer/testnet/tx/3b6c3d028b48928d6ad0fcee8d4815bd3bba1d8c0ae8a8178958dd14cd71b74f) |
| REPAY | `7590343c3f9e9f1ef73b119843cba39f570ec9474bc6a586040f89c47d644965` | `liquidated = true`; ~3 stroops interest in ~10s, inside the 0.1 nIN dust-close | [tx](https://stellar.expert/explorer/testnet/tx/7590343c3f9e9f1ef73b119843cba39f570ec9474bc6a586040f89c47d644965) |

Repay submitted the 10 nIN principal. Accrued interest over ~10s at 12%/yr on 10 nIN is a few stroops. `repay_loan` treats remaining principal + unpaid interest `<= DUST` (0.1 nIN) as fully closed and sets `liquidated = true`.

---

## 6. What was verified on-chain vs unit tests only

### On-chain (this rehearsal)

- Deploy + wire of all five Wasm contracts and both SACs.
- `roles.set_role` (oracle, attester) / `set_leader` (signed by the union); `membership.confirm_member` for borrower and junior LP.
- Classic-asset trustlines + SAC `mint` of nIN.
- `deposit_junior` (25 nIN) and `deposit_senior` (75 nIN).
- `draw_loan` with oracle + borrower auth, `fast_draw = true`, junior-first funding (10 nIN ≤ junior cash, so senior cash untouched).
- `repay_loan` of principal; loan closed via dust threshold (`liquidated = true`).
- Read views after draw: `get_junior_market`, `get_senior_market`, nIN `balance`, `union_borrows`.

`escrow_id` was `0`, so `fund_core` did **not** call `fx_pool.resolve_escrow_cash`. FX was deployed and `set_fund_core` / `set_fx_pool` were invoked. Mint, redeem, Reflector, cash-scan, and order books were not exercised. The nUSD SAC was constructor input only.

### Unit tests only (24 tests, native, `mock_all_auths()`)

| Crate | Count | Assertions |
| --- | --- | --- |
| `nila-math` | 7 | Share round-trip, index growth, haircut, 12%/yr interest, U256 large book (₹8.2M × RAY), kinked rate curve, negative `mul_div` reject |
| `nila-roles` | 5 | Roles and leaders round trip; admin-only `set_role`; `set_leader` needs the union, not the admin |
| `nila-membership` | 9 | Both signatures, roles-based attester, one union per member, set_plots, reattest, set_record, leader-only revoke, plot cap (max 32) |
| `nila-fund-viewer` | 2 | Union registry, swap-remove fund types, borrower index |
| `nila-fx-pool` | 1 | Escrow resolve by Core, remaining decrements |
| `nila-fund-core` | 10 | Membership gates, junior-first funding, fast-draw + repay, fee split when below min rate, junior first-loss, 14-day unbond, pause, ₹10k cap, remove undrawn loan, cash conservation |

Not on testnet this run: default / haircut, 14-day unbond + claim, pause, cap reject, accept-then-draw (non-fast), fee split to treasury/rainy, FX escrow remaining, `env.auths()` tree assertions. Those live in the suite. Before mainnet, add `env.auths()` checks on `draw_loan`, `deposit_*`, and `upgrade`.

---

## 7. Gaps / next

Aligned to TAD §7 and the tranche table (§9). Honest scope for this tree:

| Item | TAD home | Status |
| --- | --- | --- |
| Full `fx_pool` (mint/redeem, ±2% band, daily USDC-out cap enforcement, cash-scan DISBURSE/INVEST/REPAY, redeem-order and cash-offer books) | §5.2, §7.1 FX table | Built: cash scan and cash-out (C3), senior route (C5a), LP Bring/Collect offers with the daily cap (C5b), `quote` (C9); attester fix with a 5% band instead of ±2% |
| Reflector INR/USD (SEP-40), staleness guard, signed-fix override | §4.3, Tranche 2-B | Not integrated |
| CCTP EVM → native Circle USDC | §5.6, Tranche 2-A | Not started |
| Anchor Platform (SEP-1/10/12/24/38), membership at SEP-10 | §5.4, Tranche 1-A | Not this repo: nila-anchor (Nov 7) |
| Privy + sponsored reserves | §5.3, Tranche 1-B/C | Not this repo |
| Mainnet at ₹10,000 cap, 3-of-5 admin, no timelock until caps rise | §4.3, Tranche 1-D | Testnet rehearsal only. TAD: testnet is for the suite, not milestones. |
| Polygon 30 July 2026 book replay (side-by-side waterfall) | Tranche 2-B | Property tests not written |
| Land titles / food tokens stay on Polygon; membership absorbs them later | §4.2, §6 | Attested facts only |
| Indexer (RPC event retention ~7 days) | §4.3 / §7.1 events | Not deployed |

Testnet resets quarterly. The contract IDs and tx hashes in §5 will disappear. Re-run `scripts/testnet-rehearsal.sh` and replace the table.

---

## 8. How Stellar reviewers can inspect

Simulate views against `FUND_CORE` (no send). Passphrase is `Test SDF Network ; September 2015`.

```bash
NETWORK=testnet
CORE=CAOIXQU7JIIKI3ZLTSZOZFRHJQR5HHCEWILPT77S7773ZV7KFP6FSHIU
UNION=GCVKJNMWQQFU36ZG5Y2EKLURHPUL5Q7FVN7QLYZ7QL2XEPKG2B2QT2QY
LOAN_TYPE=1100000000000000000000000000000000000000000000000000000000000000
LOAN_ID=4200000000000000000000000000000000000000000000000000000000000000

stellar contract invoke --id "$CORE" --network "$NETWORK" --send=no -- \
  get_loan --union "$UNION" --loan_id "$LOAN_ID"

stellar contract invoke --id "$CORE" --network "$NETWORK" --send=no -- \
  get_junior_market --union "$UNION" --loan_type "$LOAN_TYPE"

stellar contract invoke --id "$CORE" --network "$NETWORK" --send=no -- \
  get_senior_market --union "$UNION"
```

Expected after the 7 Sep 2026 repay, if the ledger still holds this deploy (values read immediately after `759034…`):

- `get_loan`: `liquidated = true`, `defaulted = false`, `principal = 100000000`, `principal_paid = 100000000`, `interest_accrued = 3`, `interest_paid = 3`, `funded_from_junior = 3`, `funded_from_senior = 0`. The 3-stroop remainder is inside `DUST` (0.1 nIN); dust-close marks the loan fully repaid without zeroing the last 3 stroops of `union_borrows`.
- `get_junior_market`: `cash = 250000000` (25 nIN restored), `total_borrows = 3`, index slightly above RAY from the 3-stroop interest bump (`1000000012000000000000000000`).
- `get_senior_market`: `cash = 750000000` (draw never touched senior).
- `union_borrows`: `3`.

Events are the audit log. On each tx above, look for `#[contractevent]` payloads: `Deposit` on the two deposits, `LoanClaimed` on the draw (`fast_draw = true`), `LoanRepaid` on the repay (`interest_paid` / `principal_paid`). Event names are the structs in `contracts/fund-core/src/events.rs`. RPC typically retains ~7 days; after that, use stellar.expert or an indexer.

Membership and roles are readable the same way: `is_member --union $UNION --account $BORROWER` on `MEMBERSHIP`; `is_oracle --account $ORACLE` on `ROLES`.
