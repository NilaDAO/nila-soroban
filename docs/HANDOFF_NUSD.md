# Handoff: nUSD, the senior USDC vault (Stellar lane)

For Claude Code, working in `nila-soroban`. Written 1 Nov 2026.

## 0. Read first

- Base branch: `t1/contracts/owner-split` (commit `0611abd`, on top of `main` `d4da3bb`). Branch from it: `t2/contracts/nusd-vault`.
- Repo docs: `docs/STORAGE_LAYOUT.md`, `docs/MAINNET_CHECKLIST.md`, `docs/TEST_AND_DEPLOY.md`.
- Code you will touch: `contracts/fund-core/src/lib.rs` (senior pool, `claim`, `senior_interest`, `mark_default`), `contracts/fx-pool/src/lib.rs` (`mint_senior`, `set_rate`) and `contracts/fx-pool/src/lp.rs` (`settle_position`, `lp_member`), `contracts/nila-common/src/lib.rs` (client traits, `upgrade`).
- Conventions: every stored key and struct is sampled in that crate's `storage_layout_is_frozen` test; every privileged change is evented; upgrades go through `nila_common::upgrade` (48 h); comments carry the decision date. Keep them.
- Hard limits: 128 KiB per wasm (CI gate). Today `fx_pool` is 130,013 B (1,059 B left) and `fund_core` 118,331 B. Claude Code never deploys to mainnet.

## 1. Goal

Senior money becomes one liquid token, nUSD: a USDC-denominated claim that rises in value as the vouchers it guarantees earn interest.

## 2. Decisions already made (do not reopen)

1. **Denominated in USDC.** The lender's principal comes back at par in USDC. Only the interest is earned in rupees and crosses the exchange rate.
2. **The USDC stays in the vault.** It guarantees vouchers and leaves only for redemptions and a loss cover. fx_pool never holds senior USDC again.
3. **Non-rebasing, wstETH style.** Balances never change; the price (USDC per nUSD) rises.
4. **Price from tracked USDC only.** `price = (total + V) / (shares + V)`, V = 1 USDC (10,000,000 units), a virtual offset against rounding games. `total` and `shares` are counters the vault keeps. The attester's fix never sets the price.
5. **The vault is the only senior investor** in fund_core. Individual senior lenders are not onboarded; `mint_senior` for persons goes away.
6. **Owner split is the guarantee mechanism** (base branch): owners get the prepay at the claim, the postpaid rest as the farmer repays, and from senior at a default.
7. **Interest is turned into USDC inside the union, by a member LP**, never by sending rupees out of India: an accepted LP takes the union's interest rupees and pays USDC into the vault. This keeps Nila's one-way rule (money on-ramps into India, never off-ramps).
8. **Simplest working version.** No new role beyond `operator`. No oracle in the price. No automatic allocation across unions.

## 3. Architecture

- **nUSD token:** a classic Stellar asset (`NUSD`), its SAC deployed, SAC admin = the vault, issuer locked afterwards. This is the same pattern as nIN with fx_pool (checklist section C). The SAC has no total supply, so the vault counts `shares` itself.
- **New crate `contracts/nusd-vault`** (`nila-nusd-vault`). It holds all senior USDC and mints and burns all nUSD.
- **Allocation:** the trust assigns guarantee to a union. fx_pool mints senior nIN for the vault at the day's fix, and no USDC moves. The vault records per union `Alloc { usdc, nin }`.
- **Income:** fund_core counts senior interest apart from principal. Monthly, an LP takes those rupees at the union office and pays USDC to the vault in one transaction, and `total` rises.
- **Release:** the vault unbonds principal shares in fund_core (14 days) and burns them, and that USDC becomes free again.
- **Loss:** after a voucher default the trust writes down the union's allocation and may send that USDC out to make the union whole, behind a 48-hour delay. `total` falls by exactly that.

## 4. Contract work

### 4.1 `nusd-vault` (new)

| Key | Tier | Value |
| --- | --- | --- |
| `Admin`, `Operator`, `Usdc`, `Nusd`, `FundCore`, `FxPool`, `Paused` | instance | Address ×6, bool |
| `Total`, `Shares` | instance | i128: tracked USDC, nUSD supply |
| `Alloc(union)` | persistent | `Alloc{usdc, nin}`: guarantee assigned to a union and the senior nIN minted for it |
| `Queue` | persistent | `Vec<Ticket{id, holder, nusd, at}>`, max 64, oldest first |
| `NextTicket` | instance | u64 |
| `PendingCover` | instance | `Cover{union, usdc, to, eta}` |
| upgrade keys | instance | `nila_common::upgrade` (48 h) |

Functions:

| Function | Who signs | Does |
| --- | --- | --- |
| `__constructor(admin, usdc, nusd, fund_core, fx_pool)` | deployer | stores config; upgrade delay 48 h |
| `set_operator(op)`, `pause`, `unpause` | admin | evented |
| `set_holder(holder, ok)` | admin or operator | `set_authorized` on the nUSD SAC (needed when the asset is issued with authorisation required) |
| `deposit(holder, usdc)` | holder | USDC in; mints `usdc × (shares + V) / (total + V)` nUSD; `total += usdc` |
| `redeem(holder, nusd)` | holder | burns nUSD; pays `nusd × price` USDC if `free()` covers it, else refused |
| `request_redeem(holder, nusd)` / `cancel_request(id)` / `fulfil(id)` | holder / holder / anyone | moves nUSD into a ticket; `fulfil` pays at the price of that moment once `free()` covers it, oldest first |
| `allocate(union, usdc)` | admin | requires `free() ≥ usdc`; calls `fx_pool.mint_senior_guaranteed(union, usdc)`; `Alloc += (usdc, nin)` |
| `release(union, usdc)` | admin | `fund_core.request_unbond(union, vault, shares_for(nin_part), senior = true)` for `nin_part = usdc × Alloc.nin / Alloc.usdc` |
| `release_claim(union)` | anyone, after 14 days | `fund_core.claim(union, vault, 0, true)` (burns); `Alloc -=` pro rata |
| `harvest(union, leader, lp, inr, usdc)` | operator + leader + lp | lp must be an accepted, id-checked LP of the union (`fx_pool.lp_member`, `fx_pool.id_checked`); `usdc ≥ inr / fix × (1 − lp_fee)`; calls `fund_core.take_senior_income(union, leader, inr)`; pulls `usdc` from the lp; `total += usdc` |
| `write_down(union)` | anyone | reads `fund_core.senior_position(union, vault)`; if its principal value (value − `SeniorIncome`) < `Alloc.nin`, cuts `Alloc` to it and moves the matching USDC (`Alloc.usdc` pro rata) out of `total` into the cover |
| `propose_cover(union, usdc, to)` / `execute_cover` / `cancel_cover` | admin | 48 h delay; USDC to `to` (an LP who brings the rupees); only up to what `write_down` set aside |
| `price()`, `free()`, `alloc(union)`, `queue()`, `total()`, `shares()` | view | `free() = USDC balance − Σ Alloc.usdc − cover set aside` |

Invariants for the tests: the vault's USDC balance equals `total` plus any set-aside cover; `Σ Alloc.usdc ≤ total`; `shares` equals the nUSD minted minus burned.

### 4.2 `fund_core`

1. **Senior `claim` burns.** With `senior = true`, burn the nIN in fund_core and return the amount; never transfer it (today it fails with SAC error 11). Only the vault is senior.
2. **`senior_position(union, investor)`**: shares, value, pending, claimable, request_ts, cost.
3. **`SeniorIncome(union)`** (new persistent key, i128): add in `senior_interest()` and add `m_vi` in `mark_default`. This is voucher interest that senior earned and has not yet taken.
4. **`take_senior_income(union, leader, amount)`**: the vault signs, and a leader of the union co-signs (she hands the rupees to the LP). It takes `amount ≤ SeniorIncome ≤ senior idle cash`, burns the vault's shares worth `amount` at the current index (the index is unchanged), burns the nIN, calls `fx_pool.settle_cash(union, amount)` and lowers `SeniorIncome`. This is the same desk pattern as `claim_junior_cash`. It is evented (`SeniorIncomeTaken`).
5. Layout guard: sample `SeniorIncome` and the new view struct; re-pin; add a row to `STORAGE_LAYOUT.md`.

### 4.3 `fx_pool`

1. **Free space first.** For a fresh deploy, remove the pre-24 Oct escrow migration (`EscrowsOf`, `Escrow`, the read-back in `open_escrows`, the cleanup in `save_open_escrows`), the unused `Scan` and `LpUsdc` keys, and `VoucherFloor` with `set_voucher_floor`/`voucher_floor` (nothing reads it since 31 Oct).
2. **Replace the person senior route.** Remove `mint_senior`, `set_senior_investor`, `senior_investor`, `SeniorInvestor`, `SeniorUsdc`, `senior_usdc`. Add `set_vault(vault)` (admin, set once) and `mint_senior_guaranteed(union, usdc) -> nin`: it requires the vault's auth, not paused, and a fresh fix. It mints `usdc × fix / RATE_ONE` nIN, `authorize_pull`, then `fund_core.deposit_senior(union, vault, nin)`. No USDC transfer. It is evented (`SeniorGuaranteed{union, usdc, nin, inr_per_usd}`).
3. **Rate limit (review H4) and pause (review M2).** `set_rate` allows one attester fix per 12 hours (store its time) and is refused while paused. `settle_position` is refused while paused. `allocate` depends on the fix, so this is a prerequisite.
4. Layout guard re-pinned; `STORAGE_LAYOUT.md` updated; report the wasm size in the commit message (it must stay under 131,072 B).

**Stop and ask** if the target deployment has any `SeniorUsdc` above 0, or any escrow still under `EscrowsOf`: then a key removal would orphan real money, and an in-place upgrade needs a migration instead.

### 4.4 Other crates

- `nila-common`: client traits the vault needs (fund_core `request_unbond`, `claim`, `senior_position`, `take_senior_income`; fx_pool `mint_senior_guaranteed`, `lp_member`, `id_checked`, `rate`, `lp_fee`).
- `machinery`, `account`, `account-factory`, `land-title`, `roles`, `crop-calendar`: no change in this lane.

## 5. Tests (definition of done)

- End to end, in the fund-core test harness (it already wires fx_pool, machinery and accounts): deposit, then allocate, a voucher draw, an owner claim (owner split), repayment with interest, harvest through an LP, the price going up, and redeem from free USDC. Also a queue when USDC is allocated, release and release_claim after 14 days, and a default with write_down, propose and execute cover and the price going down.
- Vault unit tests: rounding with the virtual offset (a 1-unit first deposit followed by a large yield does not hurt the next depositor beyond 1 unit), pause, the upgrade timelock, cover delay and cancel, and a queue of 64.
- Auth: `env.auths()` checks on `deposit`, `harvest` (operator, leader and lp all required), `allocate`, `execute_cover`.
- Conservation: fund_core's existing check still holds, and the vault invariants in 4.1 hold.
- `cargo fmt`, `clippy -D warnings`, all tests and the size gate pass. Layout guards are re-pinned in the same commit as the change.
- Docs: `STORAGE_LAYOUT.md` (vault table, fx_pool removals), `MAINNET_CHECKLIST.md` (nUSD issuer: flags then SAC then `set_admin(vault)` then lock; deploy order; `fx_pool.set_vault`; alerts on `CoverProposed`, `SeniorGuaranteed`, `Allocated`), and a testnet rehearsal script `scripts/c6-testnet.sh` in the style of `c5b`.

## 6. Out of scope here

The other review findings (H1–H3, H5–H7, the account factory's instant settings). The discount that drivers get for a smaller prepay. The Polygon side. Any mainnet step.

## 7. Changes outside this repo

| Where | Change |
| --- | --- |
| PWA, machine owner | After a claim, show the amount "paid now" and the amount owed "when the farmer repays". Add a balance card with **Collect** (`fund_core.collect`) and the list of what each voucher still owes (`owner_dues`). Notify on `OwnerDueSettled`. (Owner split, base branch: needed now.) |
| PWA, leader | Add a monthly **Interest to USDC** step: pick an accepted LP, count the rupees out, co-sign `harvest`. The repay receipt shows the owners it settled. |
| PWA, member | No change. |
| PWA | Remove any senior-investor screens; senior goes through the vault only. |
| Website (nila.land, invite-gated invest flow) | nUSD page: KYC through the anchor, then `set_holder`, deposit and redeem, request, cancel and the queue. Show the price, realised yield over 30 and 90 days (from `YieldAdded` and `Harvested`), allocation by union, and free versus allocated USDC. |
| Node, indexer | New events: vault (`Deposited`, `Redeemed`, `RedeemRequested`, `Fulfilled`, `Allocated`, `ReleaseRequested`, `Released`, `Harvested`, `WrittenDown`, `CoverProposed`, `CoverExecuted`); fund_core (`OwnerDueSettled`, `OwnerCollected`, `SeniorIncomeTaken`); fx_pool (`SeniorGuaranteed`). Changed meaning: `VoucherPaid.prepay` = paid now, `.pending` = held for the owner. |
| Node, beats | Attester fix at most every 12 hours (the contract refuses more often). A monthly harvest job prepares `harvest` per union for the operator, the leader and the LP. Daily reconciliation: vault USDC balance against `total` plus cover, and fund_core conservation. Alerts on cover proposals, queue length, and free USDC under a threshold. |
| nila-anchor | KYC (SEP-12) for nUSD holders feeds `set_holder`; no conversion role (interest is converted in-union by LPs). |

## 8. Open questions (stop and ask before building these parts)

1. **nUSD asset flags.** Is the asset issued with authorisation required (KYC'd holders only) or open? This cannot change after the issuer is locked. The code supports both through `set_holder`.
2. **Issuer and vault admin.** Which entity issues nUSD and is the vault's admin: the trust multisig or another entity?
3. **Operator key.** Who holds `operator` (the node, the trust, or the anchor server)?
4. **Loss cover on the rupee side.** When cover USDC buys rupees through an LP, how are those rupees booked in the union (which scan purpose)? Implement `write_down` and the delayed cover. Leave the booking to the ops runbook unless told otherwise.
5. **Offshore and domestic tag (R24).** Is it still needed per deposit? If yes, record it in the `Deposited` event only.
6. **Deployment route.** Fresh redeploy or in-place upgrade of fx_pool and fund_core? This decides whether the removals in 4.3 need a migration.
7. **Repo names** for the PWA, the website and the node, so that tickets land in the right place.
