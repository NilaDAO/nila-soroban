#!/usr/bin/env bash
# Step C5b on Stellar testnet: member LP offers and the voucher floor.
# A member LP the anchor accepted does a Collect (she pays USDC, takes the union's spare rupees)
# and then a Bring (she brings rupees, the union pays her USDC + 1%). A senior investor's USDC is
# then drawn only above the voucher floor.
# Usage: bash scripts/c5b-testnet.sh        (testnet only; refuses any other network)
#
# USDC is a throwaway test asset "USDC" so no faucet is needed. Identities live in the stellar CLI
# config, never in this repo; only public addresses and tx hashes are printed.
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
[[ "$NETWORK" == "testnet" ]] || { echo "C5b runs on testnet only" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
OUT="$ROOT/.stellar/c5b-testnet.env"
WASM_DIR="$ROOT/target/wasm32v1-none/release"
P="${PREFIX:-c5b$(date +%m%d%H%M)}"
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

log "identities"
for n in admin nin-issuer usdc-issuer attester anchor union leader member lp investor; do ident "${P}-$n"; done
ISSUER=$(addr ${P}-nin-issuer)
USDC_ISSUER=$(addr ${P}-usdc-issuer)
UNION=$(addr ${P}-union)
LP=$(addr ${P}-lp)

log "assets: nIN (auth required, revocable, clawback) and a test USDC"
tx stellar tx new set-options --source ${P}-nin-issuer \
  --set-required --set-revocable --set-clawback-enabled >/dev/null
tx stellar contract asset deploy --asset "nIN:$ISSUER" --source ${P}-nin-issuer --alias ${P}_nin >/dev/null
NIN_SAC=$(stellar contract alias show ${P}_nin)
tx stellar contract asset deploy --asset "USDC:$USDC_ISSUER" --source ${P}-usdc-issuer --alias ${P}_usdc >/dev/null
USDC_SAC=$(stellar contract alias show ${P}_usdc)
for who in lp investor; do
  tx stellar tx new change-trust --source ${P}-$who --line "USDC:$USDC_ISSUER" >/dev/null
  tx stellar contract invoke --id "$USDC_SAC" --source ${P}-usdc-issuer -- \
    mint --to "$(addr ${P}-$who)" --amount $((200 * NIN)) >/dev/null
done

log "build and deploy (release)"
stellar contract build -q
ROLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_roles.wasm" --source ${P}-admin -q -- --admin ${P}-admin)
TITLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_land_title.wasm" --source ${P}-admin -q -- \
  --admin ${P}-admin --roles "$ROLES")
CORE=$(stellar contract deploy --wasm "$WASM_DIR/nila_fund_core.wasm" --source ${P}-admin -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES" --nin_token "$NIN_SAC")
FX=$(stellar contract deploy --wasm "$WASM_DIR/nila_fx_pool.wasm" --source ${P}-admin -q -- \
  --admin ${P}-admin --roles "$ROLES" --land_title "$TITLES" --nin "$NIN_SAC" --usdc "$USDC_SAC" \
  --daily_cap $((2500 * NIN)))
tx stellar contract invoke --id "$FX" --source ${P}-admin -- set_fund_core --fund_core "$CORE" >/dev/null
tx stellar contract invoke --id "$CORE" --source ${P}-admin -- set_fx_pool --fx_pool "$FX" >/dev/null
tx stellar contract invoke --id "$NIN_SAC" --source ${P}-nin-issuer -- set_admin --new_admin "$FX" >/dev/null
tx stellar contract invoke --id "$FX" --source ${P}-admin -- activate >/dev/null
tx stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_role --account ${P}-attester --role 1 --enabled true >/dev/null
tx stellar contract invoke --id "$ROLES" --source ${P}-union -- \
  set_leader --union "$UNION" --account ${P}-leader --enabled true >/dev/null
tx stellar contract invoke --id "$CORE" --source ${P}-admin -- \
  set_union_config --union "$UNION" --uc "{
    \"max_loan\": \"$((10000 * NIN))\", \"gate\": \"$((600 * NIN))\", \"rate_bp\": 1250,
    \"treasury_fee_bp\": 100, \"rainy_fee_bp\": 77, \"safety_bp\": 0, \"safety_floor\": \"0\",
    \"hard_stop\": false, \"enforce_token_amount\": false }" >/dev/null
H=$(printf 'ab%.0s' {1..32})
for pair in "201 member" "202 lp"; do
  set -- $pair
  tx stellar contract invoke --id "$TITLES" --source ${P}-attester -- \
    set_title --title_id "$1" --attester ${P}-attester --holder ${P}-$2 --union "$UNION" \
    --polygon_holder aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --record_hash "$H" --metadata_hash "$H" >/dev/null
done
tx stellar tx new change-trust --source ${P}-member --line "nIN:$ISSUER" >/dev/null

scan() { # purpose member amount
  tx stellar contract invoke --id "$FX" --source ${P}-leader -- \
    cash_scan_mint --leader ${P}-leader --union "$UNION" --purpose "$1" --member "$2" \
    --amount "$3" --scan_hash "$(receipt)"
}
log "the union's cash at hand: treasury 5,000, invest 2,000"
scan 3 ${P}-leader $((5000 * NIN)) >/dev/null
scan 0 ${P}-member $((2000 * NIN)) >/dev/null

log "anchor and LP: only the anchor accepts, only members qualify (R20)"
tx stellar contract invoke --id "$FX" --source ${P}-admin -- set_anchor --anchor ${P}-anchor >/dev/null
KYC=$(printf '4c%.0s' {1..32})
must_fail "a non-member as LP" stellar contract invoke --id "$FX" --source ${P}-anchor -- \
  set_lp --union "$UNION" --lp ${P}-investor --accepted true --kyc_ref "$KYC"
must_fail "the admin accepting an LP instead of the anchor" stellar contract invoke --id "$FX" \
  --source ${P}-admin -- set_lp --union "$UNION" --lp ${P}-lp --accepted true --kyc_ref "$KYC"
TX_LP=$(tx stellar contract invoke --id "$FX" --source ${P}-anchor -- \
  set_lp --union "$UNION" --lp ${P}-lp --accepted true --kyc_ref "$KYC")
NOW=$(date +%s)
TX_RATE=$(tx stellar contract invoke --id "$FX" --source ${P}-admin -- \
  override_rate --inr_per_usd $((95 * NIN)) --valid_until $((NOW + 86400)))

post() { # kind inr -> offer id
  stellar contract invoke --id "$FX" --source ${P}-leader -- post_offer --leader ${P}-leader \
    --union "$UNION" --kind "$1" --inr "$2" --expires $(($(date +%s) + 3600)) 2>/dev/null
}
accept() { # offer
  tx stellar contract invoke --id "$FX" --source ${P}-lp -- accept --lp ${P}-lp --offer_id "$1" \
    --sep24_id '"sep24-test"' --quote_id '"sep38-test"'
}
confirm() { tx stellar contract invoke --id "$FX" --source ${P}-leader -- confirm --leader ${P}-leader --offer_id "$1"; }

log "Collect ₹950: she pays 9.9 USDC (10 less 1%), takes the rupees; cash at hand drops"
HAND=$(view "$FX" cash_at_hand --union "$UNION" | tr -d '"')
C=$(post 1 $((950 * NIN)))
TX_COLLECT_ACCEPT=$(accept "$C")
TX_COLLECT_CONFIRM=$(confirm "$C")
expect "union LP USDC" "$(view "$FX" lp_usdc --union "$UNION")" '"99000000"'
expect "cash at hand after the Collect" "$(view "$FX" cash_at_hand --union "$UNION")" "\"$((HAND - 950 * NIN))\""

log "Bring ₹475: she brings rupees, gets 5.05 USDC from the LP USDC"
B=$(post 0 $((475 * NIN)))
TX_BRING_ACCEPT=$(accept "$B")
TX_BRING_CONFIRM=$(confirm "$B")
expect "union LP USDC after the Bring" "$(view "$FX" lp_usdc --union "$UNION")" '"48500000"'
expect "LP USDC wallet" "$(view "$USDC_SAC" balance --id "$LP")" "\"$((200 * NIN - 99000000 + 50500000))\""

log "senior USDC is drawn only above the voucher floor"
tx stellar contract invoke --id "$FX" --source ${P}-admin -- \
  set_senior_investor --investor ${P}-investor --accepted true --kyc_ref "$KYC" >/dev/null
tx stellar contract invoke --id "$FX" --source ${P}-investor -- \
  mint_senior --investor ${P}-investor --union "$UNION" --usdc $((100 * NIN)) --tag 0 >/dev/null
TX_FLOOR=$(tx stellar contract invoke --id "$FX" --source ${P}-admin -- \
  set_voucher_floor --union "$UNION" --usdc $((95 * NIN)))
BIG=$(post 0 $((950 * NIN)))
must_fail "a Bring that would cut into the voucher floor" accept "$BIG"
TX_BIG_CANCEL=$(tx stellar contract invoke --id "$FX" --source ${P}-leader -- \
  cancel_offer --leader ${P}-leader --offer_id "$BIG")
expect "senior offshore USDC untouched" "$(view "$FX" senior_usdc --union "$UNION" --tag 0)" "\"$((100 * NIN))\""
expect "fx_pool holds no nIN" "$(view "$NIN_SAC" balance --id "$FX")" '"0"'

mkdir -p "$ROOT/.stellar"
cat >"$OUT" <<EOF
# C5b testnet run $(date -u +%Y-%m-%dT%H:%MZ), identity prefix $P — public addresses and tx hashes only
NIN_SAC=$NIN_SAC
USDC_SAC=$USDC_SAC
UNION=$UNION
LP=$LP
FUND_CORE=$CORE
FX_POOL=$FX
TX_LP=$TX_LP
TX_RATE=$TX_RATE
TX_COLLECT_ACCEPT=$TX_COLLECT_ACCEPT
TX_COLLECT_CONFIRM=$TX_COLLECT_CONFIRM
TX_BRING_ACCEPT=$TX_BRING_ACCEPT
TX_BRING_CONFIRM=$TX_BRING_CONFIRM
TX_FLOOR=$TX_FLOOR
TX_BIG_CANCEL=$TX_BIG_CANCEL
EOF
log "wrote $OUT (explorer: https://stellar.expert/explorer/testnet/tx/<hash>)"
cat "$OUT"
