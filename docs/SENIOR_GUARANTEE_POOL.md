# Senior capital: the guarantee pool, end to end

SCF #45 Tranche 2, deliverable A (row 2): the senior-capital entry path. Executive decisions of 23 Nov 2026;
contracts `senior`, `fund_core` and `fx_pool` on `main` (`01f6f00`), live after the lane-B upgrade.
Approved as the row 2 deliverable (executive decision, 25 Nov 2026).

**In one line:** senior investors put in USDC; it stays in the `senior` contract and stands behind the machine
vouchers of the unions; it earns their interest and covers a service provider's loss when a farmer cannot pay.
Senior never lends.

## 1. Who

- A senior investor is anyone the Nila Farmer Livelihoods Trust accepts after KYC at its anchor (SEP-12): the trust
  calls `senior.set_investor` (3-of-5), which fixes the residency tag (domestic or offshore) once.
- Members with a land title may also invest in senior. Their own interest is junior, which funds the cash part of
  their union's crop agreements.
- Unaudited: deposits stop at the trust's cap, which is 0 until the contract is audited.

## 2. Entry and exit

| Step | Where | What happens on-chain |
|---|---|---|
| KYC | the trust's anchor (anchor.nila.land, SEP-12) | nothing; the anchor keeps the record |
| Acceptance | the trust, 3-of-5 | `set_investor(investor, true, kyc_ref, tag)` |
| Deposit | nila.land/senior, the investor's own Stellar wallet | `deposit(investor, usdc)`: USDC into `senior`, shares at the current index |
| Ask to withdraw | nila.land/senior | `request_redeem(investor, shares)`: the shares keep their part of interest and losses for 14 days |
| Withdraw | nila.land/senior, after 14 days | `redeem(investor)`: shares × index of that moment, in USDC, within the daily cap (2,500 USDC a day) |

One pool, one index for every investor: a share's value is shares × index (ray math, as fund_core's pools). The
investor's principal is in USDC and carries no exchange-rate risk.

## 3. What the money does

- **Vouchers are lent from junior money** (members, sold agreements; during the development period Nila itself,
  as junior through a leader, who may invest without a land title). Senior's USDC is never lent and never becomes
  nIN; it is the guarantee behind every voucher's value (100% of it).
- **Senior earns the voucher interest, all of it.** fund_core sets each voucher's interest aside per union, in
  rupees (`guarantee_fee`; the trust can lower the share with `set_guarantee_fee`). Whenever an LP collects the
  union's cash, senior's part comes first: the LP pays its USDC at the day's rate less the LP's fee inside the one
  total she accepts; at the leader's confirm that USDC goes to `senior` (`fee_in`) and the index rises. The
  exchange-rate risk on the interest is senior's.
- **Return.** About the voucher rate × (vouchers ÷ senior USDC) − the rupee's fall: at a 12.5% voucher rate and
  half the pool standing behind vouchers, about 3–5% a year before losses.

## 4. Losses

At a default the junior pool carries the cash part of the crop agreement; senior carries the voucher part:

1. fund_core books the voucher value the member's own investment did not cover (`voucher_loss(union)`;
   `LoanDefaulted.senior_applied`). The junior pool's value does not fall for it.
2. The trust proposes the cover (`propose_cover(union, ceiling)`, 3-of-5); it waits 48 hours, visible to all.
3. `execute_cover`: the USDC that buys the booked loss at the rate in force goes to the union through fx_pool
   (`cover_from_senior`: the union's treasury USDC); the same value in nIN refills the junior pool, which had paid
   the machine owners. The senior index falls by the USDC paid.
4. If senior is short, the rest stays booked as owed by senior until covered; junior does not take it.

## 5. Controls

- No admin withdrawal: USDC leaves `senior` only by a cover (48 h) or an exit (14 days, daily cap).
- Every contract upgrade is timelocked 48 h and signed by the trust 3-of-5.
- Alerts at indexer.nila.land ([`ALERTS.md`](ALERTS.md)): A20 USDC out a day, A22 caps and the fee share, A26 a
  senior cover proposed, paid or cancelled (page), A27 a voucher loss waiting for senior, A1 upgrades.
- Event schema: [`EVENT_SCHEMA.md`](EVENT_SCHEMA.md) (`senior`: `deposited`, `redeem_requested`, `redeemed`,
  `fee_paid`, `cover_proposed`, `cover_paid`, `cover_cancelled`).

## 6. Not yet

- The audit before the cap rises above 0.
- Counsel on the regime of a USDC guarantee pool that earns voucher interest (AML/KYC, investment, guarantee),
  and on offshore interest paid by Indian LPs (compliance invariants I2/I3).
- nUSD, a token for senior shares (`docs/HANDOFF_NUSD.md`, 1 Nov), is not built.
