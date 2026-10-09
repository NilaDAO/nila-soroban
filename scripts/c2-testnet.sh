#!/usr/bin/env bash
# Step C2 on Stellar testnet: land titles as membership, junior invest, draw -> repay, and a
# second loan that runs past its payback date into default.
# Usage: bash scripts/c2-testnet.sh        (testnet only; refuses any other network)
#
# fund_core is built with `testnet-short-windows` (one day = one second) so the payback date
# (drawdown + 90 days, end of season + 21 days) comes after ~2 minutes. That build declares
# itself in its contract meta ("windows") and must never reach mainnet.
#
# fx_pool's cash scan is C3. Until then a stand-in key sits in fund_core's fx_pool slot and pays
# the nIN in for the cash the leader received (executive decision, 9 Oct: money in only through fx_pool).
#
# Identities live in the stellar CLI config (~/.config/stellar), never in this repo, and are
# never printed — only public addresses are. The nIN issuer is a throwaway testnet key (D7, X14).
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
[[ "$NETWORK" == "testnet" ]] || { echo "C2 runs on testnet only" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
OUT="$ROOT/.stellar/c2-testnet.env"
WASM_DIR="$ROOT/target/wasm32v1-none/release"
SHORT_DIR="$ROOT/target/short"
P="${PREFIX:-c2r$(date +%m%d%H%M)}"
NIN=10000000 # one nIN at 7 decimals

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

log "identities"
for n in admin nin-issuer attester oracle union leader member member2 fx-standin anyone; do
  ident "${P}-$n"
done
ISSUER=$(addr ${P}-nin-issuer)
UNION=$(addr ${P}-union)
FXS=$(addr ${P}-fx-standin)

log "nIN: issuer flags auth required, revocable, clawback (R8); SAC"
TX_FLAGS=$(tx stellar tx new set-options --source ${P}-nin-issuer \
  --set-required --set-revocable --set-clawback-enabled)
TX_SAC=$(tx stellar contract asset deploy --asset "nIN:$ISSUER" --source ${P}-nin-issuer --alias ${P}_nin)
NIN_SAC=$(stellar contract alias show ${P}_nin)

log "build: roles, land_title (release); fund_core with testnet-short-windows"
stellar contract build -q
CARGO_TARGET_DIR="$SHORT_DIR" cargo build -q --release --target wasm32v1-none \
  -p nila-fund-core --features testnet-short-windows
CORE_WASM="$SHORT_DIR/wasm32v1-none/release/nila_fund_core.wasm"

log "deploy"
ROLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_roles.wasm" --source ${P}-admin \
  --alias ${P}_roles -q -- --admin ${P}-admin)
TITLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_land_title.wasm" --source ${P}-admin \
  --alias ${P}_land_title -q -- --admin ${P}-admin --roles "$ROLES")
CORE=$(stellar contract deploy --wasm "$CORE_WASM" --source ${P}-admin --alias ${P}_core -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES" --nin_token "$NIN_SAC")

log "roles: attester, oracle (admin); leader (union account, D12)"
tx stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_role --account ${P}-attester --role 1 --enabled true >/dev/null
tx stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_role --account ${P}-oracle --role 0 --enabled true >/dev/null
tx stellar contract invoke --id "$ROLES" --source ${P}-union -- \
  set_leader --union "$UNION" --account ${P}-leader --enabled true >/dev/null

log "land titles (F1): one each for member and member2, minted by the union"
H1=$(printf 'ab%.0s' {1..32})
H2=$(printf 'cd%.0s' {1..32})
POLY=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
title() {
  tx stellar contract invoke --id "$TITLES" --source ${P}-attester -- \
    set_title --title_id "$1" --attester ${P}-attester --holder "$2" --union "$UNION" \
    --polygon_holder "$POLY" --record_hash "$H1" --metadata_hash "$H2"
}
TX_TITLE=$(title 101 ${P}-member)
TX_TITLE2=$(title 102 ${P}-member2)

log "union terms (admin): ₹10,000 cap, gate 600, 12.5%, fees 1% + 0.77%"
TX_UCFG=$(tx stellar contract invoke --id "$CORE" --source ${P}-admin -- \
  set_union_config --union "$UNION" --uc "{
    \"max_loan\": \"$((10000 * NIN))\", \"gate\": \"$((600 * NIN))\", \"rate_bp\": 1250,
    \"treasury_fee_bp\": 100, \"rainy_fee_bp\": 77, \"safety_bp\": 0, \"safety_floor\": \"0\",
    \"hard_stop\": false, \"escrow_duration\": 604800, \"enforce_token_amount\": false }")
tx stellar contract invoke --id "$CORE" --source ${P}-admin -- set_fx_pool --fx_pool "$FXS" >/dev/null

log "nIN to the stand-in fx_pool; fund_core's balance authorised by the issuer"
tx stellar tx new change-trust --source ${P}-fx-standin --line "nIN:$ISSUER" >/dev/null
for who in "$FXS" "$CORE"; do
  tx stellar contract invoke --id "$NIN_SAC" --source ${P}-nin-issuer -- \
    set_authorized --id "$who" --authorize true >/dev/null
done
tx stellar contract invoke --id "$NIN_SAC" --source ${P}-nin-issuer -- \
  mint --to "$FXS" --amount $((5000 * NIN)) >/dev/null

log "non-member cannot be invested for"
must_fail "deposit_junior for a non-member" stellar contract invoke --id "$CORE" --source ${P}-fx-standin -- \
  deposit_junior --union "$UNION" --member ${P}-anyone --amount $NIN

log "cash-scan INVEST (stand-in fx_pool): 1,000 nIN each"
invest() {
  tx stellar contract invoke --id "$CORE" --source ${P}-fx-standin -- \
    deposit_junior --union "$UNION" --member "$1" --amount $((1000 * NIN))
}
TX_INVEST=$(invest ${P}-member)
TX_INVEST2=$(invest ${P}-member2)

L1=$(printf '01%.0s' {1..32})
L2=$(printf '02%.0s' {1..32})
T1=$(printf 'f1%.0s' {1..32})
T2=$(printf 'f2%.0s' {1..32})
draw() { # loan_id cash token_id
  tx stellar contract invoke --id "$CORE" --source ${P}-member -- \
    draw_loan --union "$UNION" --member ${P}-member --token_id "$3" --loan_id "$1" --cash "$2" --escrow_id 0
}
attest() { # token_id amount — open from now
  tx stellar contract invoke --id "$CORE" --source ${P}-attester -- \
    attest_food_token --attester ${P}-attester --union "$UNION" --token_id "$1" \
    --member ${P}-member --amount "$2" --from_date 0
}

log "food tokens (F2): the attester copies two of her FoodTokens, 400 and 300 nIN"
must_fail "a draw without an attested food token" draw "$L1" $((100 * NIN)) "$T1"
TX_ATTEST=$(attest "$T1" $((400 * NIN)))
TX_ATTEST2=$(attest "$T2" $((300 * NIN)))

log "draw over the cap must fail; draw 500 nIN on the 400 token (amount gate off while testing), member alone (D-c)"
must_fail "draw over ₹10,000" draw "$L1" $((10001 * NIN)) "$T1"
TX_DRAW=$(draw "$L1" $((500 * NIN)) "$T1")
must_fail "a second loan on the same food token" draw "$L2" $((100 * NIN)) "$T1"
# She has no nIN trustline at all, so the balance read fails; either way she holds none.
BAL=$(view "$NIN_SAC" balance --id ${P}-member 2>/dev/null || echo '"0"')
[[ "$BAL" == '"0"' ]] || { echo "nIN reached her wallet (D9)" >&2; exit 1; }
view "$CORE" position --union "$UNION" --member ${P}-member

log "cash-scan REPAY 600 (takes only what is owed)"
TX_REPAY=$(tx stellar contract invoke --id "$CORE" --source ${P}-fx-standin -- \
  repay_loan --union "$UNION" --loan_id "$L1" --amount $((600 * NIN)))
view "$CORE" get_loan --union "$UNION" --loan_id "$L1"

log "second loan: 300 nIN, end of season reported, then default after the payback date"
TX_DRAW2=$(draw "$L2" $((300 * NIN)) "$T2")
DRAWDOWN=$(view "$CORE" get_loan --union "$UNION" --loan_id "$L2" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["drawdown_ts"])')
must_fail "mark_default without an end of season" stellar contract invoke --id "$CORE" --source ${P}-anyone -- \
  mark_default --union "$UNION" --loan_id "$L2"
must_fail "report_maturity by a non-oracle" stellar contract invoke --id "$CORE" --source ${P}-leader -- \
  report_maturity --oracle ${P}-leader --union "$UNION" --loan_id "$L2" --maturity_ts "$DRAWDOWN"
TX_MATURITY=$(tx stellar contract invoke --id "$CORE" --source ${P}-oracle -- \
  report_maturity --oracle ${P}-oracle --union "$UNION" --loan_id "$L2" --maturity_ts "$DRAWDOWN")
DUE=$(view "$CORE" due_date --union "$UNION" --loan_id "$L2")
log "due $DUE (drawdown $DRAWDOWN + 90 + 21 short days)"
must_fail "mark_default before the payback date" stellar contract invoke --id "$CORE" --source ${P}-anyone -- \
  mark_default --union "$UNION" --loan_id "$L2"

log "waiting for the payback date"
while (($(date +%s) <= ${DUE//\"/} + 10)); do sleep 10; done
TX_OVERDUE=$(tx stellar contract invoke --id "$CORE" --source ${P}-anyone -- \
  flag_overdue --union "$UNION" --loan_id "$L2")
T3=$(printf 'f3%.0s' {1..32})
attest "$T3" $NIN >/dev/null
must_fail "a draw while lending is on hold" draw "$(printf '03%.0s' {1..32})" $NIN "$T3"
TX_DEFAULT=$(tx stellar contract invoke --id "$CORE" --source ${P}-anyone -- \
  mark_default --union "$UNION" --loan_id "$L2")

log "checks"
view "$CORE" get_loan --union "$UNION" --loan_id "$L2"
view "$CORE" position --union "$UNION" --member ${P}-member
view "$CORE" position --union "$UNION" --member ${P}-member2
view "$CORE" on_hold --union "$UNION"
view "$CORE" get_junior_market --union "$UNION"
echo "fund_core nIN $(view "$NIN_SAC" balance --id "$CORE"), treasury $(view "$CORE" union_treasury --union "$UNION"), rainy $(view "$CORE" union_rainy_day --union "$UNION")"

mkdir -p "$ROOT/.stellar"
cat >"$OUT" <<EOF
# C2 testnet run $(date -u +%Y-%m-%dT%H:%MZ), identity prefix $P — public addresses and tx hashes only
# fund_core built with testnet-short-windows (one day = one second)
NIN_ISSUER=$ISSUER
NIN_SAC=$NIN_SAC
UNION=$UNION
FX_STANDIN=$FXS
ROLES=$ROLES
LAND_TITLE=$TITLES
FUND_CORE=$CORE
TX_FLAGS=$TX_FLAGS
TX_SAC=$TX_SAC
TX_TITLE=$TX_TITLE
TX_TITLE2=$TX_TITLE2
TX_UCFG=$TX_UCFG
TX_INVEST=$TX_INVEST
TX_INVEST2=$TX_INVEST2
TX_ATTEST=$TX_ATTEST
TX_ATTEST2=$TX_ATTEST2
TX_DRAW=$TX_DRAW
TX_REPAY=$TX_REPAY
TX_DRAW2=$TX_DRAW2
TX_MATURITY=$TX_MATURITY
TX_OVERDUE=$TX_OVERDUE
TX_DEFAULT=$TX_DEFAULT
EOF
log "wrote $OUT (explorer: https://stellar.expert/explorer/testnet/tx/<hash>)"
cat "$OUT"
