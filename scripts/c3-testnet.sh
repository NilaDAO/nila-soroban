#!/usr/bin/env bash
# Step C3 on Stellar testnet: fx_pool as the nIN SAC admin and the union's cash desk.
# Treasury and invest scans, a draw that settles cash at hand, a cash repay with change,
# a claim to her wallet through the trustline flip (R10), a cash-out paid by the leader, a
# cash-out reclaimed after expiry, and an expired escrow covered from the treasury (R15).
# Usage: bash scripts/c3-testnet.sh        (testnet only; refuses any other network)
#
# fund_core and fx_pool are built with `testnet-short-windows`: in fund_core one day is one
# second (14-day unbond = 14 s), in fx_pool ten seconds (7-day escrow and cash-out = 70 s).
# Those builds declare it in their contract meta ("windows") and must never reach mainnet.
#
# Identities live in the stellar CLI config (~/.config/stellar), never in this repo, and are
# never printed — only public addresses are. The nIN issuer is a throwaway testnet key (D7, X14).
#
# Kept as a record of its step. From C6 on, loans are keyed by the food token id (`token_id`, no
# separate `loan_id`): this script no longer runs unchanged. The current flow is scripts/c5-testnet.sh.
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
[[ "$NETWORK" == "testnet" ]] || { echo "C3 runs on testnet only" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
OUT="$ROOT/.stellar/c3-testnet.env"
WASM_DIR="$ROOT/target/wasm32v1-none/release"
SHORT_DIR="$ROOT/target/short"
P="${PREFIX:-c3r$(date +%m%d%H%M)}"
NIN=10000000 # one nIN at 7 decimals
# Circle's testnet USDC SAC; fx_pool stores it for T2 and moves no USDC in T1.
USDC_SAC="${USDC_SAC:-CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA}"

log() { printf '\n==> %s\n' "$*" >&2; }
addr() { stellar keys address "$1"; }
tx() {
  local out
  out=$("$@" 2>&1) || { echo "$out" >&2; return 1; }
  grep -oE 'Signing transaction: [0-9a-f]{64}' <<<"$out" | tail -1 | awk '{print $3}'
}
view() { stellar contract invoke --id "$1" --source ${P}-admin --send=no -- "${@:2}"; }
ident() { addr "$1" >/dev/null 2>&1 || stellar keys generate "$1" --fund -q; }
must_fail() {
  local what="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    echo "$what succeeded — must fail" >&2
    exit 1
  fi
  log "refused as expected: $what"
}
expect() { # what actual wanted
  [[ "$2" == "$3" ]] || { echo "$1: got $2, want $3" >&2; exit 1; }
  log "ok: $1 = $2"
}
ledger_now() { # latest closed ledger time, as the contract sees it
  curl -s https://horizon-testnet.stellar.org/ledgers?order=desc\&limit=1 |
    python3 -c 'import json,sys,datetime as d; t=json.load(sys.stdin)["_embedded"]["records"][0]["closed_at"]; print(int(d.datetime.fromisoformat(t.replace("Z","+00:00")).timestamp()))'
}
wait_until() { # unix ts
  while (($(ledger_now) < $1)); do sleep 5; done
}
receipt() { openssl rand -hex 32; }

log "identities"
for n in admin nin-issuer attester oracle union leader member member2 anyone; do
  ident "${P}-$n"
done
ISSUER=$(addr ${P}-nin-issuer)
UNION=$(addr ${P}-union)

log "nIN: issuer flags auth required, revocable, clawback (R8); SAC"
TX_FLAGS=$(tx stellar tx new set-options --source ${P}-nin-issuer \
  --set-required --set-revocable --set-clawback-enabled)
TX_SAC=$(tx stellar contract asset deploy --asset "nIN:$ISSUER" --source ${P}-nin-issuer --alias ${P}_nin)
NIN_SAC=$(stellar contract alias show ${P}_nin)

log "build: roles, land_title (release); fund_core and fx_pool with testnet-short-windows"
stellar contract build -q
for pkg in nila-fund-core nila-fx-pool; do
  CARGO_TARGET_DIR="$SHORT_DIR" cargo build -q --release --target wasm32v1-none \
    -p $pkg --features testnet-short-windows
done
SHORT_WASM="$SHORT_DIR/wasm32v1-none/release"

log "deploy"
ROLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_roles.wasm" --source ${P}-admin \
  --alias ${P}_roles -q -- --admin ${P}-admin)
TITLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_land_title.wasm" --source ${P}-admin \
  --alias ${P}_land_title -q -- --admin ${P}-admin --roles "$ROLES")
CORE=$(stellar contract deploy --wasm "$SHORT_WASM/nila_fund_core.wasm" --source ${P}-admin \
  --alias ${P}_core -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES" --nin_token "$NIN_SAC")
FX=$(stellar contract deploy --wasm "$SHORT_WASM/nila_fx_pool.wasm" --source ${P}-admin \
  --alias ${P}_fx -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES" --nin "$NIN_SAC" \
  --usdc "$USDC_SAC" --daily_cap $((2500 * NIN)))
tx stellar contract invoke --id "$FX" --source ${P}-admin -- set_fund_core --fund_core "$CORE" >/dev/null
tx stellar contract invoke --id "$CORE" --source ${P}-admin -- set_fx_pool --fx_pool "$FX" >/dev/null
must_fail "pointing fx_pool at another fund_core" stellar contract invoke --id "$FX" --source ${P}-admin -- \
  set_fund_core --fund_core "$ROLES"

log "R8: the issuer hands the nIN SAC admin to fx_pool; fx_pool authorises the two contracts"
TX_SET_ADMIN=$(tx stellar contract invoke --id "$NIN_SAC" --source ${P}-nin-issuer -- \
  set_admin --new_admin "$FX")
TX_ACTIVATE=$(tx stellar contract invoke --id "$FX" --source ${P}-admin -- activate)
expect "SAC admin" "$(view "$NIN_SAC" admin)" "\"$FX\""
must_fail "the issuer minting after set_admin" stellar contract invoke --id "$NIN_SAC" --source ${P}-nin-issuer -- \
  mint --to "$FX" --amount $NIN

log "lock the issuer (master weight 0): it keeps no classic mint, authorise or clawback power"
TX_LOCK=$(tx stellar tx new set-options --source ${P}-nin-issuer --master-weight 0)
must_fail "any issuer transaction after the lock" stellar tx new set-options --source ${P}-nin-issuer \
  --home-domain nila.test

log "roles: attester, oracle (admin); leader (union account, D12)"
tx stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_role --account ${P}-attester --role 1 --enabled true >/dev/null
tx stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_role --account ${P}-oracle --role 0 --enabled true >/dev/null
tx stellar contract invoke --id "$ROLES" --source ${P}-union -- \
  set_leader --union "$UNION" --account ${P}-leader --enabled true >/dev/null

log "land titles (F1) for member and member2; their nIN trustlines open unauthorised (R9, R10)"
H1=$(printf 'ab%.0s' {1..32})
H2=$(printf 'cd%.0s' {1..32})
POLY=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
title() {
  tx stellar contract invoke --id "$TITLES" --source ${P}-attester -- \
    set_title --title_id "$1" --attester ${P}-attester --holder "$2" --union "$UNION" \
    --polygon_holder "$POLY" --record_hash "$H1" --metadata_hash "$H2"
}
title 101 ${P}-member >/dev/null
title 102 ${P}-member2 >/dev/null
for m in member member2; do
  tx stellar tx new change-trust --source ${P}-$m --line "nIN:$ISSUER" >/dev/null
done

log "union terms (admin): ₹10,000 cap, gate 600, 12.5%, fees 1% + 0.77%"
tx stellar contract invoke --id "$CORE" --source ${P}-admin -- \
  set_union_config --union "$UNION" --uc "{
    \"max_loan\": \"$((10000 * NIN))\", \"gate\": \"$((600 * NIN))\", \"rate_bp\": 1250,
    \"treasury_fee_bp\": 100, \"rainy_fee_bp\": 77, \"safety_bp\": 0, \"safety_floor\": \"0\",
    \"hard_stop\": false, \"enforce_token_amount\": false }" >/dev/null

scan() { # purpose member amount [loan_id] — purpose 0 invest, 1 repay, 2 recovery, 3 treasury
  local extra=()
  # Option<BytesN<32>>: the CLI reads it as a JSON string.
  [[ -n "${4:-}" ]] && extra=(--loan_id "\"$4\"")
  tx stellar contract invoke --id "$FX" --source ${P}-leader -- \
    cash_scan_mint --leader ${P}-leader --union "$UNION" --purpose "$1" --member "$2" \
    --amount "$3" --scan_hash "$(receipt)" ${extra[@]+"${extra[@]}"}
}

log "cash scans (leader): treasury first, since cash at hand may not exceed it (R15)"
must_fail "an invest scan above the treasury" scan 0 ${P}-member $((1000 * NIN))
TX_SCAN_TREASURY=$(scan 3 ${P}-leader $((5000 * NIN)))
must_fail "a scan above ₹10,000 (R76)" scan 3 ${P}-leader $((10001 * NIN))
must_fail "a scan by a non-leader" stellar contract invoke --id "$FX" --source ${P}-member -- \
  cash_scan_mint --leader ${P}-member --union "$UNION" --purpose 0 --member ${P}-member \
  --amount $NIN --scan_hash "$(receipt)"
must_fail "an invest scan for a non-member" scan 0 ${P}-anyone $NIN
TX_SCAN_INVEST=$(scan 0 ${P}-member $((1000 * NIN)))
TX_SCAN_INVEST2=$(scan 0 ${P}-member2 $((1000 * NIN)))
REUSED=$(receipt)
tx stellar contract invoke --id "$FX" --source ${P}-leader -- cash_scan_mint --leader ${P}-leader \
  --union "$UNION" --purpose 0 --member ${P}-member2 --amount $NIN --scan_hash "$REUSED" >/dev/null
must_fail "a receipt scanned twice" stellar contract invoke --id "$FX" --source ${P}-leader -- \
  cash_scan_mint --leader ${P}-leader --union "$UNION" --purpose 0 --member ${P}-member2 \
  --amount $NIN --scan_hash "$REUSED"
expect "cash at hand" "$(view "$FX" cash_at_hand --union "$UNION")" "\"$((2001 * NIN))\""
expect "fx_pool holds no nIN" "$(view "$NIN_SAC" balance --id "$FX")" '"0"'

log "draw 500 (member alone, D-c): settles the cash at hand oldest first (D9)"
L1=$(printf '01%.0s' {1..32})
T1=$(printf 'f1%.0s' {1..32})
tx stellar contract invoke --id "$CORE" --source ${P}-attester -- \
  attest_food_token --attester ${P}-attester --union "$UNION" --token_id "$T1" \
  --member ${P}-member --amount $((400 * NIN)) >/dev/null
TX_DRAW=$(tx stellar contract invoke --id "$CORE" --source ${P}-member -- \
  draw_loan --union "$UNION" --member ${P}-member --token_id "$T1" --loan_id "$L1" --cash $((500 * NIN)))
expect "cash at hand after the draw" "$(view "$FX" cash_at_hand --union "$UNION")" "\"$((1501 * NIN))\""
expect "her wallet after the draw" "$(view "$NIN_SAC" balance --id ${P}-member)" '"0"'

log "repay scan 600: only what she owes is minted, the leader hands back the change"
TX_SCAN_REPAY=$(scan 1 ${P}-member $((600 * NIN)) "$L1")
view "$CORE" get_loan --union "$UNION" --loan_id "$L1"
expect "fx_pool holds no nIN" "$(view "$NIN_SAC" balance --id "$FX")" '"0"'

log "member2 unbonds 400, claims after 14 short days: to her wallet through the flip (R10)"
TX_UNBOND=$(tx stellar contract invoke --id "$CORE" --source ${P}-member2 -- \
  request_unbond_junior --union "$UNION" --member ${P}-member2 --shares $((400 * NIN)))
wait_until $(($(ledger_now) + 16))
TX_CLAIM=$(tx stellar contract invoke --id "$CORE" --source ${P}-member2 -- \
  claim_junior --union "$UNION" --member ${P}-member2 --max_shares 0)
# 400 shares are worth a little more than 400 nIN: the repay's interest raised the index.
W2=$(view "$NIN_SAC" balance --id ${P}-member2 | tr -d '"')
((W2 >= 400 * NIN)) || { echo "member2 claim paid $W2" >&2; exit 1; }
log "ok: member2 wallet = $W2"
expect "member2 trustline authorised" "$(view "$NIN_SAC" authorized --id ${P}-member2)" "false"
must_fail "member2 sending nIN to member herself (R10, C2)" stellar contract invoke --id "$NIN_SAC" \
  --source ${P}-member2 -- transfer --from ${P}-member2 --to ${P}-member --amount $NIN

log "cash-out (D13): member2 escrows 250 under a one-time code; the leader scans and pays out"
CODE=$(openssl rand -hex 16)
CODE_HASH=$(printf '%s' "$CODE" | xxd -r -p | shasum -a 256 | awk '{print $1}')
NOW=$(ledger_now)
must_fail "a cash-out above ₹10,000" stellar contract invoke --id "$FX" --source ${P}-member2 -- \
  request_cash_out --member ${P}-member2 --union "$UNION" --amount $((10001 * NIN)) \
  --code_hash "$CODE_HASH" --expires $((NOW + 60))
must_fail "a cash-out longer than 7 days" stellar contract invoke --id "$FX" --source ${P}-member2 -- \
  request_cash_out --member ${P}-member2 --union "$UNION" --amount $NIN \
  --code_hash "$CODE_HASH" --expires $((NOW + 200))
TX_CASH_OUT=$(tx stellar contract invoke --id "$FX" --source ${P}-member2 -- \
  request_cash_out --member ${P}-member2 --union "$UNION" --amount $((250 * NIN)) \
  --code_hash "$CODE_HASH" --expires $((NOW + 60)))
expect "member2 trustline after the cash-out" "$(view "$NIN_SAC" authorized --id ${P}-member2)" "false"
must_fail "pay-out by a non-leader" stellar contract invoke --id "$FX" --source ${P}-member -- \
  pay_out --leader ${P}-member --code "$CODE"
TX_PAY_OUT=$(tx stellar contract invoke --id "$FX" --source ${P}-leader -- \
  pay_out --leader ${P}-leader --code "$CODE")
must_fail "the same code paid twice" stellar contract invoke --id "$FX" --source ${P}-leader -- \
  pay_out --leader ${P}-leader --code "$CODE"
expect "fx_pool holds no nIN" "$(view "$NIN_SAC" balance --id "$FX")" '"0"'

log "cash-out never collected: reclaimed by anyone after expiry, back to her wallet"
CODE2=$(openssl rand -hex 16)
CODE2_HASH=$(printf '%s' "$CODE2" | xxd -r -p | shasum -a 256 | awk '{print $1}')
NOW=$(ledger_now)
tx stellar contract invoke --id "$FX" --source ${P}-member2 -- \
  request_cash_out --member ${P}-member2 --union "$UNION" --amount $((100 * NIN)) \
  --code_hash "$CODE2_HASH" --expires $((NOW + 20)) >/dev/null
must_fail "reclaim before expiry" stellar contract invoke --id "$FX" --source ${P}-anyone -- \
  reclaim_cash_out --code_hash "$CODE2_HASH"
wait_until $((NOW + 21))
TX_RECLAIM=$(tx stellar contract invoke --id "$FX" --source ${P}-anyone -- \
  reclaim_cash_out --code_hash "$CODE2_HASH")
expect "member2 wallet after reclaim" "$(view "$NIN_SAC" balance --id ${P}-member2)" "\"$((W2 - 250 * NIN))\""

log "escrow 2 (member2's invest) left unsettled: after 7 short days anyone burns it from the treasury"
TREASURY_BEFORE=$(view "$CORE" union_treasury --union "$UNION" | tr -d '"')
E2_EXPIRES=$(view "$FX" get_escrow --escrow_id 2 | python3 -c 'import json,sys; print(json.load(sys.stdin)["expires"])')
wait_until $((E2_EXPIRES + 1))
TX_EXPIRE=$(tx stellar contract invoke --id "$FX" --source ${P}-anyone -- burn_expired_escrow --escrow_id 2)
expect "treasury after the expired escrow" "$(view "$CORE" union_treasury --union "$UNION")" \
  "\"$((TREASURY_BEFORE - 1000 * NIN))\""

log "checks"
view "$FX" escrows_of --union "$UNION"
echo "cash at hand $(view "$FX" cash_at_hand --union "$UNION")"
J=$(view "$CORE" get_junior_market --union "$UNION" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cash"])')
S=$(view "$CORE" get_senior_market --union "$UNION" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cash"])')
TR=$(view "$CORE" union_treasury --union "$UNION" | tr -d '"')
RA=$(view "$CORE" union_rainy_day --union "$UNION" | tr -d '"')
expect "fund_core nIN = junior + senior + treasury + rainy" \
  "$(view "$NIN_SAC" balance --id "$CORE" | tr -d '"')" "$((J + S + TR + RA))"

mkdir -p "$ROOT/.stellar"
cat >"$OUT" <<EOF
# C3 testnet run $(date -u +%Y-%m-%dT%H:%MZ), identity prefix $P — public addresses and tx hashes only
# fund_core and fx_pool built with testnet-short-windows
NIN_ISSUER=$ISSUER
NIN_SAC=$NIN_SAC
UNION=$UNION
ROLES=$ROLES
LAND_TITLE=$TITLES
FUND_CORE=$CORE
FX_POOL=$FX
TX_FLAGS=$TX_FLAGS
TX_SAC=$TX_SAC
TX_SET_ADMIN=$TX_SET_ADMIN
TX_ACTIVATE=$TX_ACTIVATE
TX_LOCK=$TX_LOCK
TX_SCAN_TREASURY=$TX_SCAN_TREASURY
TX_SCAN_INVEST=$TX_SCAN_INVEST
TX_SCAN_INVEST2=$TX_SCAN_INVEST2
TX_DRAW=$TX_DRAW
TX_SCAN_REPAY=$TX_SCAN_REPAY
TX_UNBOND=$TX_UNBOND
TX_CLAIM=$TX_CLAIM
TX_CASH_OUT=$TX_CASH_OUT
TX_PAY_OUT=$TX_PAY_OUT
TX_RECLAIM=$TX_RECLAIM
TX_EXPIRE=$TX_EXPIRE
EOF
log "wrote $OUT (explorer: https://stellar.expert/explorer/testnet/tx/<hash>)"
cat "$OUT"
