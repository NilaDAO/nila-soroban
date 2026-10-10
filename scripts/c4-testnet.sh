#!/usr/bin/env bash
# Step C4 on Stellar testnet: mainnet readiness rehearsal.
# - The admin of all five contracts is a trust account with 3 signers, threshold 2, master
#   weight 0 (D2, C11): one signature is refused, a plain key is refused, two pass.
# - The timelock holds: an upgrade filed on roles cannot run before 48 hours; it is cancelled.
# - A real upgrade end to end: fund_core is upgraded through the timelock and keeps its
#   stored state (union terms, pause flag, delay); the new wasm hash is checked on-chain.
# Usage: bash scripts/c4-testnet.sh        (testnet only; refuses any other network)
#
# fund_core and fx_pool are built with `testnet-short-windows`, so fund_core's 48-hour delay is
# 2 seconds here. fund_core is then upgraded to the release build of the same source.
#
# Identities live in the stellar CLI config (~/.config/stellar), never in this repo, and are
# never printed — only public addresses are.
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
[[ "$NETWORK" == "testnet" ]] || { echo "C4 runs on testnet only" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
OUT="$ROOT/.stellar/c4-testnet.env"
WASM_DIR="$ROOT/target/wasm32v1-none/release"
SHORT_DIR="$ROOT/target/short"
SHORT_WASM="$SHORT_DIR/wasm32v1-none/release"
P="${PREFIX:-c4r$(date +%m%d%H%M)}"
NIN=10000000
USDC_SAC="${USDC_SAC:-CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA}"

log() { printf '\n==> %s\n' "$*" >&2; }
addr() { stellar keys address "$1"; }
tx() {
  local out
  out=$("$@" 2>&1) || { echo "$out" >&2; return 1; }
  grep -oE 'Signing transaction: [0-9a-f]{64}' <<<"$out" | tail -1 | awk '{print $3}'
}
view() { stellar contract invoke --id "$1" --source ${P}-deployer --send=no -- "${@:2}"; }
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

log "identities: deployer, the trust account and its three signer keys"
for n in deployer trust nin-issuer union leader; do ident "${P}-$n"; done
for i in 1 2 3; do ident "${P}-trust-s$i"; done # signer keys only
TRUST=$(addr ${P}-trust)
UNION=$(addr ${P}-union)

log "trust account: 3 signers of weight 1, thresholds 2, master weight 0 (D2)"
X=$(stellar tx new set-options --inclusion-fee 1000 --source ${P}-trust \
  --signer "$(addr ${P}-trust-s1)" --signer-weight 1 --build-only)
for i in 2 3; do
  X=$(stellar tx op add set-options --source-account ${P}-trust \
    --signer "$(addr "${P}-trust-s$i")" --signer-weight 1 <<<"$X")
done
X=$(stellar tx op add set-options --source-account ${P}-trust \
  --master-weight 0 --low-threshold 2 --med-threshold 2 --high-threshold 2 <<<"$X")
X=$(stellar tx sign --sign-with-key ${P}-trust -q <<<"$X")
TX_TRUST=$(stellar tx hash <<<"$X")
stellar tx send -q <<<"$X" >/dev/null

# An admin call by the trust account. $1 = number of signers (1 or 2); prints the tx hash.
admin_call() {
  local n="$1" id="$2"
  shift 2
  local X
  X=$(stellar contract invoke --id "$id" --source "$TRUST" --build-only -- "$@")
  X=$(stellar tx simulate --source-account "$TRUST" <<<"$X")
  for i in $(seq 1 "$n"); do X=$(stellar tx sign --sign-with-key "${P}-trust-s$i" -q <<<"$X"); done
  local h
  h=$(stellar tx hash <<<"$X")
  stellar tx send -q <<<"$X" >/dev/null || return 1
  echo "$h"
}

log "nIN SAC (throwaway issuer, as in C3)"
tx stellar tx new set-options --source ${P}-nin-issuer \
  --set-required --set-revocable --set-clawback-enabled >/dev/null
tx stellar contract asset deploy --asset "nIN:$(addr ${P}-nin-issuer)" --source ${P}-nin-issuer \
  --alias ${P}_nin >/dev/null
NIN_SAC=$(stellar contract alias show ${P}_nin)

log "build: release, plus fund_core and fx_pool with testnet-short-windows"
stellar contract build -q
for pkg in nila-fund-core nila-fx-pool; do
  CARGO_TARGET_DIR="$SHORT_DIR" cargo build -q --release --target wasm32v1-none \
    -p $pkg --features testnet-short-windows
done

log "deploy all five with the trust account as admin"
dep() { # wasm alias args...
  local w="$1" a="$2"
  shift 2
  stellar contract deploy --wasm "$w" --source ${P}-deployer --alias "${P}_$a" -q -- "$@"
}
ROLES=$(dep "$WASM_DIR/nila_roles.wasm" roles --admin "$TRUST")
TITLES=$(dep "$WASM_DIR/nila_land_title.wasm" land_title --admin "$TRUST" --roles "$ROLES")
VIEWER=$(dep "$WASM_DIR/nila_fund_viewer.wasm" viewer --admin "$TRUST")
CORE=$(dep "$SHORT_WASM/nila_fund_core.wasm" core --admin "$TRUST" --roles "$ROLES" \
  --land_title "$TITLES" --nin_token "$NIN_SAC")
FX=$(dep "$SHORT_WASM/nila_fx_pool.wasm" fx --admin "$TRUST" --roles "$ROLES" \
  --land_title "$TITLES" --nin "$NIN_SAC" --usdc "$USDC_SAC" --daily_cap $((2500 * NIN)))
for c in "$ROLES" "$TITLES" "$VIEWER"; do
  expect "upgrade delay of $c" "$(view "$c" upgrade_delay)" "172800"
done
expect "upgrade delay of fund_core (short build: 2 days = 2 s)" "$(view "$CORE" upgrade_delay)" "2"

log "admin by the trust: a plain key, and one signature of three, are refused"
must_fail "an admin call signed by the deployer key" stellar contract invoke --id "$CORE" \
  --source ${P}-deployer -- set_fx_pool --fx_pool "$FX"
must_fail "an admin call with 1 of 3 signatures" admin_call 1 "$CORE" set_fx_pool --fx_pool "$FX"
TX_SET_FX=$(admin_call 2 "$CORE" set_fx_pool --fx_pool "$FX")
TX_SET_CORE=$(admin_call 2 "$FX" set_fund_core --fund_core "$CORE")
TX_UCFG=$(admin_call 2 "$CORE" set_union_config --union "$UNION" --uc "{
    \"max_loan\": \"$((10000 * NIN))\", \"gate\": \"$((600 * NIN))\", \"rate_bp\": 1250,
    \"treasury_fee_bp\": 100, \"rainy_fee_bp\": 77, \"safety_bp\": 0, \"safety_floor\": \"0\",
    \"hard_stop\": false, \"enforce_token_amount\": false }")
TX_ATTESTER_ROLE=$(admin_call 2 "$ROLES" set_role --account ${P}-leader --role 1 --enabled true)

log "timelock: an upgrade filed on roles cannot run before 48 hours; cancelled"
ROLES_HASH=$(stellar contract upload --wasm "$WASM_DIR/nila_roles.wasm" --source ${P}-deployer -q)
TX_ROLES_PROPOSE=$(admin_call 2 "$ROLES" upgrade --new_wasm_hash "$ROLES_HASH")
must_fail "execute_upgrade on roles before the delay" admin_call 2 "$ROLES" execute_upgrade
TX_ROLES_LOWER=$(admin_call 2 "$ROLES" set_upgrade_delay --delay_seconds 0)
expect "roles delay right after asking for 0 (lowering waits 48 h)" "$(view "$ROLES" upgrade_delay)" "172800"
TX_ROLES_CANCEL=$(admin_call 2 "$ROLES" cancel_upgrade)
expect "roles pending upgrade" "$(view "$ROLES" pending_upgrade)" "null"

log "real upgrade: fund_core (short build) -> release build of the same source, state kept"
TX_PAUSE=$(admin_call 2 "$CORE" pause)
NEW_HASH=$(stellar contract upload --wasm "$WASM_DIR/nila_fund_core.wasm" --source ${P}-deployer -q)
TX_PROPOSE=$(admin_call 2 "$CORE" upgrade --new_wasm_hash "$NEW_HASH")
sleep 6
TX_EXECUTE=$(admin_call 2 "$CORE" execute_upgrade)
stellar contract fetch --id "$CORE" -o "$SHORT_DIR/core-onchain.wasm"
expect "fund_core wasm on-chain" "$(shasum -a 256 "$SHORT_DIR/core-onchain.wasm" | awk '{print $1}')" "$NEW_HASH"
expect "pause kept across the upgrade" "$(view "$CORE" is_paused)" "true"
expect "union terms kept across the upgrade" \
  "$(view "$CORE" get_union_config --union "$UNION" | python3 -c 'import json,sys; print(json.load(sys.stdin)["max_loan"])')" \
  "$((10000 * NIN))"
expect "fx_pool slot kept across the upgrade" \
  "$(view "$CORE" get_config | python3 -c 'import json,sys; print(json.load(sys.stdin)["fx_pool"])')" "$FX"
TX_UNPAUSE=$(admin_call 2 "$CORE" unpause)

mkdir -p "$ROOT/.stellar"
cat >"$OUT" <<EOF
# C4 testnet run $(date -u +%Y-%m-%dT%H:%MZ), identity prefix $P — public addresses and tx hashes only
TRUST=$TRUST
NIN_SAC=$NIN_SAC
ROLES=$ROLES
LAND_TITLE=$TITLES
FUND_VIEWER=$VIEWER
FUND_CORE=$CORE
FX_POOL=$FX
FUND_CORE_NEW_WASM=$NEW_HASH
TX_TRUST=$TX_TRUST
TX_SET_FX=$TX_SET_FX
TX_SET_CORE=$TX_SET_CORE
TX_UCFG=$TX_UCFG
TX_ATTESTER_ROLE=$TX_ATTESTER_ROLE
TX_ROLES_LOWER=$TX_ROLES_LOWER
TX_ROLES_PROPOSE=$TX_ROLES_PROPOSE
TX_ROLES_CANCEL=$TX_ROLES_CANCEL
TX_PAUSE=$TX_PAUSE
TX_PROPOSE=$TX_PROPOSE
TX_EXECUTE=$TX_EXECUTE
TX_UNPAUSE=$TX_UNPAUSE
EOF
log "wrote $OUT (explorer: https://stellar.expert/explorer/testnet/tx/<hash>)"
cat "$OUT"
