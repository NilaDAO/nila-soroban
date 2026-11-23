# Alert rules — Nila on Stellar mainnet

SCF #45 Tranche 2, deliverable D (row 24). One row per rule, referenced from [`THREAT_MODEL.md`](THREAT_MODEL.md).
**Source:** `event` = an event kept by the indexer ([indexer.nila.land](https://indexer.nila.land/events), names as
in [`EVENT_SCHEMA.md`](EVENT_SCHEMA.md)); `state` = a contract or Horizon read; `http` = an endpoint check.
**Level:** `page` = someone acts now; `warn` = look the same day; `info` = logged and shown.
A rule fires once per event, or once per state change for `state` and `http` rules (checked every 10 minutes).

| ID | Rule | Source | Fires when | Level |
|---|---|---|---|---|
| A1 | Upgrade filed, applied or cancelled | event `upgrade_proposed`, `upgrade_applied`, `upgrade_cancelled` (every contract) | any | page |
| A2 | Upgrade delay changed | event `upgrade_delay_set`; state `upgrade_delay` | any event, or a delay ≠ 172800 s | page |
| A3 | Admin or role changed | event `admin_set`, `role_updated` | any | page |
| A4 | nIN issuer operation | Horizon operations of the issuer `GABG…IG5A` | any operation (until the issuer is locked) | page |
| A5 | Trust account signers changed | Horizon `GC2V…5U3E` signers and thresholds | ≠ 5 signers of weight 1 with thresholds 3/3/3 | page |
| A6 | Union settings changed | event `union_settings_set` | any; `page` when `wait` < 4 h or `sms_key` changed | warn |
| A7 | Leader added or removed | event `leader_updated`, `union_role_updated` | any | warn |
| A8 | Union admitted or revoked | event `union_admitted`, `union_revoked` | any | warn |
| A9 | Member-account recovery proposed | event `recovery_proposed` (member accounts) | any: the union's wait (4 h) is the window to react | page |
| A10 | Large cash movement | event `cash_scanned`, `cash_out_signed` | amount > ₹50,000 | warn |
| A11 | Member-account signer changed | event `signer_set` (member accounts) | any without a `recovery_proposed` on that account in the last 30 days | page |
| A12 | Rate fix away from the reference | state `fx_pool.rate()`; reference Chainlink USD/INR on Polygon (the feed the node's rate beat posts) | \|fix − reference\| > 2 % | page |
| A13 | Rate stale | state `fx_pool.rate().valid_until` | less than 12 h left (the beat posts every 12 h) | warn |
| A14 | Node key low on XLM | Horizon balances of attester, oracle, sponsor | attester or oracle < 10 XLM; sponsor < 50 XLM | warn |
| A15 | End of season reported early | event `maturity_reported`; state `fund_core.food_token().from_date` (its window's opening) | earlier than the opening + 45 days (the shortest crop, a green manure) | warn |
| A16 | Loan defaulted | event `loan_defaulted` | any | warn |
| A17 | Escrow near expiry | state `fx_pool.open_escrows(union)`, `offers_of(union)` + `offer(id)` (open or accepted) | expires within 24 h | warn |
| A18 | Escrow expired | event `escrow_expired`, `offer_closed` (state expired) | any | warn |
| A19 | Anchor down | http `anchor.nila.land/.well-known/stellar.toml`, the anchor's health | not 200 twice in a row | page |
| A20 | Much USDC out in a day | events `redeemed` (senior exits), `cover_paid` (senior loss covers, 23 Nov) and `offer_confirmed` (LP offers), `usdc`, since 00:00 UTC; senior and fx_pool enforce their own daily caps after the lane-B upgrade | > 80 % of 2,500 USDC | warn |
| A21 | Pause | event `paused`, `paused_set`; state `is_paused` / `paused` | any contract paused | page |
| A22 | Cap changed | event `max_scan_set`, `daily_cap_set`, `cap_set`, `guarantee_fee_set` (23 Nov), `union_config_set` | any | warn |
| A23 | fund_core does not add up | state: fund_core's nIN balance vs Σ unions (junior cash + senior cash + treasury + rainy reserve + voucher reserve + guarantee fee waiting, 23 Nov), as the tests' `assert_conserved` | difference > 1 nIN; or fx_pool holds nIN | page |
| A24 | Indexer behind | http `indexer.nila.land/health` | `ok` false, or a new gap in `/coverage` | warn |
| A25 | Wiring changed | event `fx_pool_set`, `fund_core_set`, `machinery_set`, `senior_set`, `anchor_set`, `swap_key_set`, `activated` | any (each is set once at deploy) | page |
| A26 | Senior loss cover | event `cover_proposed`, `cover_paid`, `cover_cancelled` (senior, 23 Nov): USDC leaves the guarantee pool for a union's voucher loss 48 h after the proposal | any | page |
| A27 | Voucher loss waiting for senior | state `fund_core.voucher_loss(union)` (23 Nov: a default's voucher part, booked for senior's cover) | > 0 | warn |

## Status (19 Nov)

- All 25 rules are checked every 10 minutes by the indexer (nila-sensing `indexer/alerts.py`), shown at
  [indexer.nila.land](https://indexer.nila.land/) and emailed (`page` and `warn`) through an SNS topic.
- Read on mainnet 19 Nov: every rule ok. fund_core holds 21,713.18 nIN = junior cash 9,299 + treasury 12,414.18;
  the fix in force equals Chainlink; no open escrow or offer.
- State rules per union (A17, A23) read the unions in the indexer's `INDEXER_UNIONS` (Mother Theresa today).
- Thresholds marked here are first values; each change to them is a commit to this file.
