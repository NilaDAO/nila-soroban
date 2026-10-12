#!/usr/bin/env bash
# Step C5 on Stellar testnet: the machinery voucher end to end.
#   union rate table (union account) → leader registers a member's tractor, owner confirms →
#   a senior investor's USDC backs the prepay → she draws cash + 10 machine hours against her
#   attested food token → she opens a spend code → the owner scans it after 4 hours of work and
#   is paid the prepay at once → she repays everything at the desk → the owner collects the rest.
# Usage: bash scripts/c5-testnet.sh        (testnet only; refuses any other network)
#
# USDC is a throwaway test asset. Identities live in the stellar CLI config, never in this repo;
# only public addresses and tx hashes are printed.
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
[[ "$NETWORK" == "testnet" ]] || { echo "C5 runs on testnet only" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
OUT="$ROOT/.stellar/c5-testnet.env"
WASM_DIR="$ROOT/target/wasm32v1-none/release"
P="${PREFIX:-c5v$(date +%m%d%H%M)}"
NIN=10000000

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
expect() {
  [[ "$2" == "$3" ]] || { echo "$1: got $2, want $3" >&2; exit 1; }
  log "ok: $1 = $2"
}
receipt() { openssl rand -hex 32; }
field() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

log "identities"
for n in admin nin-issuer usdc-issuer attester union leader member owner investor; do ident "${P}-$n"; done
ISSUER=$(addr ${P}-nin-issuer)
USDC_ISSUER=$(addr ${P}-usdc-issuer)
UNION=$(addr ${P}-union)
OWNER=$(addr ${P}-owner)

log "assets"
tx stellar tx new set-options --source ${P}-nin-issuer \
  --set-required --set-revocable --set-clawback-enabled >/dev/null
tx stellar contract asset deploy --asset "nIN:$ISSUER" --source ${P}-nin-issuer --alias ${P}_nin >/dev/null
NIN_SAC=$(stellar contract alias show ${P}_nin)
tx stellar contract asset deploy --asset "USDC:$USDC_ISSUER" --source ${P}-usdc-issuer --alias ${P}_usdc >/dev/null
USDC_SAC=$(stellar contract alias show ${P}_usdc)
tx stellar tx new change-trust --source ${P}-investor --line "USDC:$USDC_ISSUER" >/dev/null
tx stellar contract invoke --id "$USDC_SAC" --source ${P}-usdc-issuer -- \
  mint --to "$(addr ${P}-investor)" --amount $((200 * NIN)) >/dev/null
for m in member owner; do tx stellar tx new change-trust --source ${P}-$m --line "nIN:$ISSUER" >/dev/null; done

log "build and deploy (release): roles, land_title, fund_core, fx_pool, machinery"
stellar contract build -q
ROLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_roles.wasm" --source ${P}-admin -q -- --admin ${P}-admin)
TITLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_land_title.wasm" --source ${P}-admin -q -- \
  --admin ${P}-admin --roles "$ROLES")
CORE=$(stellar contract deploy --wasm "$WASM_DIR/nila_fund_core.wasm" --source ${P}-admin -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES" --nin_token "$NIN_SAC")
FX=$(stellar contract deploy --wasm "$WASM_DIR/nila_fx_pool.wasm" --source ${P}-admin -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES" --nin "$NIN_SAC" --usdc "$USDC_SAC" \
  --daily_cap $((2500 * NIN)))
MACH=$(stellar contract deploy --wasm "$WASM_DIR/nila_machinery.wasm" --source ${P}-admin -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES")
inv() { tx stellar contract invoke --id "$1" --source "$2" -- "${@:3}" >/dev/null; }
inv "$FX" ${P}-admin set_fund_core --fund_core "$CORE"
inv "$CORE" ${P}-admin set_fx_pool --fx_pool "$FX"
inv "$MACH" ${P}-admin set_fund_core --fund_core "$CORE"
inv "$CORE" ${P}-admin set_machinery --machinery "$MACH"
inv "$NIN_SAC" ${P}-nin-issuer set_admin --new_admin "$FX"
inv "$FX" ${P}-admin activate
inv "$ROLES" ${P}-admin set_role --account ${P}-attester --role 1 --enabled true
inv "$ROLES" ${P}-admin set_role --account ${P}-admin --role 0 --enabled true
inv "$ROLES" ${P}-union set_leader --union "$UNION" --account ${P}-leader --enabled true
inv "$CORE" ${P}-admin set_union_config --union "$UNION" --uc "{
    \"max_loan\": \"$((10000 * NIN))\", \"gate\": \"$((600 * NIN))\", \"rate_bp\": 1250,
    \"treasury_fee_bp\": 100, \"rainy_fee_bp\": 77, \"safety_bp\": 0, \"safety_floor\": \"0\",
    \"hard_stop\": false, \"enforce_token_amount\": false }"
H=$(printf 'ab%.0s' {1..32})
for pair in "301 member" "302 owner"; do
  set -- $pair
  inv "$TITLES" ${P}-attester set_title --title_id "$1" --attester ${P}-attester --holder ${P}-$2 \
    --union "$UNION" --polygon_holder aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --record_hash "$H" --metadata_hash "$H"
done

log "rate table: the trust's base for a tractor ₹1,450/h (driver 400, diesel 500, maint 200, ins 100, depr 250);"
log "the union account takes it at factor 100% (R57, D20)"
inv "$MACH" ${P}-admin set_base_rate --machine_type 1 --parts "{\"driver\": \"$((400 * NIN))\", \"diesel\": \"$((500 * NIN))\",
    \"maintenance\": \"$((200 * NIN))\", \"insurance\": \"$((100 * NIN))\",
    \"depreciation\": \"$((250 * NIN))\", \"margin\": \"0\"}"
TX_RATE_TABLE=$(tx stellar contract invoke --id "$MACH" --source ${P}-union -- set_rate --union "$UNION" \
  --machine_type 1 --factor_bp 10000)
must_fail "a union factor above 150%" stellar contract invoke --id "$MACH" --source ${P}-union -- \
  set_rate --union "$UNION" --machine_type 1 --factor_bp 15001
# (Only the union account sets rates: a unit test checks it with no signatures; on testnet the
# CLI holds the union's key locally and would sign for it, so the refusal cannot be shown here.)

log "L13: the leader registers the owner's tractor, 40 h a week; the owner confirms (N9)"
MACHINE=$(stellar contract invoke --id "$MACH" --source ${P}-leader -- register_machine --leader ${P}-leader \
  --union "$UNION" --owner ${P}-owner --machine_type 1 --hours_per_week 40 --photos_hash "$H" 2>/dev/null)
TX_CONFIRM=$(tx stellar contract invoke --id "$MACH" --source ${P}-owner -- \
  confirm_machine --owner ${P}-owner --machine_id "$MACHINE")

log "senior money for the prepay: 200 USDC at ₹50 = 10,000 nIN (N4)"
inv "$FX" ${P}-admin set_senior_investor --investor ${P}-investor --accepted true --kyc_ref "$H"
inv "$FX" ${P}-admin override_rate --inr_per_usd $((50 * NIN)) --valid_until $(($(date +%s) + 86400))
inv "$FX" ${P}-investor mint_senior --investor ${P}-investor --union "$UNION" --usdc $((200 * NIN)) --tag 0

log "the union's desk: treasury and her investment"
scan() {
  tx stellar contract invoke --id "$FX" --source ${P}-leader -- cash_scan_mint --leader ${P}-leader \
    --union "$UNION" --purpose "$1" --member "$2" --amount "$3" --scan_hash "$(receipt)" ${4:+--loan_id "\"$4\""}
}
# Cash at hand may not exceed the treasury (R15): 2 × ₹10,000 covers her invest and the repay.
scan 3 ${P}-leader $((10000 * NIN)) >/dev/null
scan 3 ${P}-leader $((10000 * NIN)) >/dev/null
scan 0 ${P}-member $((2000 * NIN)) >/dev/null

L=$(printf '0a%.0s' {1..32})
log "F2: her food token attested with the plan's recommendation (400 cash, 10 hours)"
TX_ATTEST=$(tx stellar contract invoke --id "$CORE" --source ${P}-attester -- attest_food_token \
  --attester ${P}-attester --union "$UNION" --token_id "$L" --member ${P}-member \
  --amount $((400 * NIN)) --hours 10 --from_date 0)
log "N10: she accepts the loan: 400 cash + 10 machine hours (senior earmarks 10 × ₹700)"
TX_DRAW=$(tx stellar contract invoke --id "$CORE" --source ${P}-member -- draw_loan --union "$UNION" \
  --member ${P}-member --token_id "$L" --loan_id "$L" --cash $((400 * NIN)) --hours 10)
expect "senior earmark" "$(view "$CORE" senior_earmark --union "$UNION")" "\"$((7000 * NIN))\""

log "A8a: she opens a code for up to 6 hours; the owner works 4 and scans it (D19)"
CODE=$(openssl rand -hex 16)
CODE_HASH=$(printf '%s' "$CODE" | xxd -r -p | shasum -a 256 | awk '{print $1}')
TX_CODE=$(tx stellar contract invoke --id "$MACH" --source ${P}-member -- open_code --member ${P}-member \
  --union "$UNION" --loan_id "$L" --max_hours 6 --code_hash "$CODE_HASH")
must_fail "a claim above the code's hours" stellar contract invoke --id "$MACH" --source ${P}-owner -- \
  claim --owner ${P}-owner --machine_id "$MACHINE" --union "$UNION" --loan_id "$L" --code "$CODE" --hours 7
TX_CLAIM=$(tx stellar contract invoke --id "$MACH" --source ${P}-owner -- claim --owner ${P}-owner \
  --machine_id "$MACHINE" --union "$UNION" --loan_id "$L" --code "$CODE" --hours 4)
expect "owner wallet after the job (prepay 4 × ₹700)" "$(view "$NIN_SAC" balance --id "$OWNER")" "\"$((2800 * NIN))\""
expect "owner trustline after the payout" "$(view "$NIN_SAC" authorized --id "$OWNER")" "false"
expect "owner's pending part (4 × ₹750)" \
  "$(view "$CORE" voucher_book --union "$UNION" --loan_id "$L" | field '["dues"][0]["amount"]')" "$((3000 * NIN))"

log "she repays everything at the desk: owners first, interest, principal, senior prepay"
TX_REPAY=$(scan 1 ${P}-member $((6400 * NIN)) "$L")
expect "loan closed" "$(view "$CORE" get_loan --union "$UNION" --loan_id "$L" | field '["closed"]')" "True"
expect "senior earmark released" "$(view "$CORE" senior_earmark --union "$UNION")" '"0"'
expect "senior pool cash back whole" \
  "$(view "$CORE" get_senior_market --union "$UNION" | field '["cash"]')" "$((10000 * NIN))"
log "A13: the owner collects her settled part"
TX_COLLECT=$(tx stellar contract invoke --id "$CORE" --source ${P}-owner -- \
  collect_dues --owner ${P}-owner --union "$UNION")
expect "owner wallet after collecting" "$(view "$NIN_SAC" balance --id "$OWNER")" "\"$((5800 * NIN))\""
expect "fx_pool holds no nIN" "$(view "$NIN_SAC" balance --id "$FX")" '"0"'

mkdir -p "$ROOT/.stellar"
cat >"$OUT" <<EOF
# C5 testnet run $(date -u +%Y-%m-%dT%H:%MZ), identity prefix $P — public addresses and tx hashes only
NIN_SAC=$NIN_SAC
USDC_SAC=$USDC_SAC
UNION=$UNION
FUND_CORE=$CORE
FX_POOL=$FX
MACHINERY=$MACH
MACHINE_ID=$MACHINE
TX_RATE_TABLE=$TX_RATE_TABLE
TX_CONFIRM=$TX_CONFIRM
TX_ATTEST=$TX_ATTEST
TX_DRAW=$TX_DRAW
TX_CODE=$TX_CODE
TX_CLAIM=$TX_CLAIM
TX_REPAY=$TX_REPAY
TX_COLLECT=$TX_COLLECT
EOF
log "wrote $OUT (explorer: https://stellar.expert/explorer/testnet/tx/<hash>)"
cat "$OUT"
