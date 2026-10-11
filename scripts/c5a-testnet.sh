#!/usr/bin/env bash
# Step C5a on Stellar testnet: the senior route. An LP the trust accepted deposits USDC; the
# attester's INR/USD fix sets the nIN minted into fund_core's senior pool; the deposit is tagged.
# Usage: bash scripts/c5a-testnet.sh        (testnet only; refuses any other network)
#
# USDC here is a throwaway classic asset "USDC" from a test issuer, so the run needs no faucet;
# mainnet uses Circle's USDC SAC. Identities live in the stellar CLI config, never in this repo;
# only public addresses and tx hashes are printed.
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
[[ "$NETWORK" == "testnet" ]] || { echo "C5a runs on testnet only" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
OUT="$ROOT/.stellar/c5a-testnet.env"
WASM_DIR="$ROOT/target/wasm32v1-none/release"
P="${PREFIX:-c5r$(date +%m%d%H%M)}"
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

log "identities"
for n in admin nin-issuer usdc-issuer attester union lp member; do ident "${P}-$n"; done
ISSUER=$(addr ${P}-nin-issuer)
UNION=$(addr ${P}-union)
LP=$(addr ${P}-lp)

log "nIN (auth required, revocable, clawback) and a test USDC"
tx stellar tx new set-options --source ${P}-nin-issuer \
  --set-required --set-revocable --set-clawback-enabled >/dev/null
tx stellar contract asset deploy --asset "nIN:$ISSUER" --source ${P}-nin-issuer --alias ${P}_nin >/dev/null
NIN_SAC=$(stellar contract alias show ${P}_nin)
tx stellar contract asset deploy --asset "USDC:$(addr ${P}-usdc-issuer)" --source ${P}-usdc-issuer \
  --alias ${P}_usdc >/dev/null
USDC_SAC=$(stellar contract alias show ${P}_usdc)
tx stellar tx new change-trust --source ${P}-lp --line "USDC:$(addr ${P}-usdc-issuer)" >/dev/null
tx stellar contract invoke --id "$USDC_SAC" --source ${P}-usdc-issuer -- \
  mint --to "$LP" --amount $((1000 * NIN)) >/dev/null

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
tx stellar contract invoke --id "$CORE" --source ${P}-admin -- \
  set_union_config --union "$UNION" --uc "{
    \"max_loan\": \"$((10000 * NIN))\", \"gate\": \"$((600 * NIN))\", \"rate_bp\": 1250,
    \"treasury_fee_bp\": 100, \"rainy_fee_bp\": 77, \"safety_bp\": 0, \"safety_floor\": \"0\",
    \"hard_stop\": false, \"enforce_token_amount\": false }" >/dev/null

mint_senior() { # usdc tag
  stellar contract invoke --id "$FX" --source ${P}-lp -- \
    mint_senior --lp ${P}-lp --union "$UNION" --usdc "$1" --tag "$2"
}
KYC=$(printf '4b%.0s' {1..32})

log "senior route: refused before the LP is accepted and before a fix exists"
must_fail "an LP the trust has not accepted" mint_senior $((100 * NIN)) 0
TX_LP=$(tx stellar contract invoke --id "$FX" --source ${P}-admin -- \
  set_lp --lp ${P}-lp --accepted true --kyc_ref "$KYC")
must_fail "a deposit with no INR/USD fix" mint_senior $((100 * NIN)) 0
NOW=$(date +%s)
must_fail "a fix posted by a non-attester" stellar contract invoke --id "$FX" --source ${P}-lp -- \
  set_rate --attester ${P}-lp --inr_per_usd $((95 * NIN)) --valid_until $((NOW + 3600))
must_fail "a fix valid longer than 2 days" stellar contract invoke --id "$FX" --source ${P}-attester -- \
  set_rate --attester ${P}-attester --inr_per_usd $((95 * NIN)) --valid_until $((NOW + 3 * 86400))
TX_RATE=$(tx stellar contract invoke --id "$FX" --source ${P}-attester -- \
  set_rate --attester ${P}-attester --inr_per_usd $((95 * NIN)) --valid_until $((NOW + 86400)))

log "mint_senior: 100 USDC offshore at ₹95 → 9,500 nIN into the senior pool, the LP alone signs"
TX_MINT=$(tx mint_senior $((100 * NIN)) 0)
expect "senior pool cash" \
  "$(view "$CORE" get_senior_market --union "$UNION" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cash"])')" \
  "$((9500 * NIN))"
expect "USDC held by fx_pool" "$(view "$USDC_SAC" balance --id "$FX")" "\"$((100 * NIN))\""
expect "offshore USDC for the union" "$(view "$FX" senior_usdc --union "$UNION" --tag 0)" "\"$((100 * NIN))\""
expect "fx_pool holds no nIN" "$(view "$NIN_SAC" balance --id "$FX")" '"0"'

mkdir -p "$ROOT/.stellar"
cat >"$OUT" <<EOF
# C5a testnet run $(date -u +%Y-%m-%dT%H:%MZ), identity prefix $P — public addresses and tx hashes only
NIN_SAC=$NIN_SAC
USDC_SAC=$USDC_SAC
UNION=$UNION
LP=$LP
FUND_CORE=$CORE
FX_POOL=$FX
TX_LP=$TX_LP
TX_RATE=$TX_RATE
TX_MINT=$TX_MINT
EOF
log "wrote $OUT (explorer: https://stellar.expert/explorer/testnet/tx/<hash>)"
cat "$OUT"
