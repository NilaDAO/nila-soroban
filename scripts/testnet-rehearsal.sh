#!/usr/bin/env bash
# Testnet rehearsal: issue stand-in nIN, deploy the facility, run deposit → draw → repay.
# Usage: bash scripts/testnet-rehearsal.sh
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
export CARGO_TARGET_DIR="$ROOT/target"

need() { command -v "$1" >/dev/null || { echo "missing $1" >&2; exit 1; }; }
need stellar
need python3

log() { printf '\n==> %s\n' "$*" >&2; }
stellar network use "$NETWORK" >/dev/null

# Deploy stdout is the contract id; keep chatter on stderr.
deploy_wasm() {
  local pkg="$1" alias="$2"
  shift 2
  log "deploy $pkg"
  stellar contract deploy \
    --wasm "$WASM_DIR/${pkg}.wasm" \
    --source-account nila-admin \
    --network "$NETWORK" \
    --alias "$alias" \
    -- \
    "$@"
}

addr() { stellar keys address "$1"; }

gen_funded() {
  local name="$1"
  if stellar keys address "$name" >/dev/null 2>&1; then
    log "identity $name exists ($(addr "$name")) — funding"
    stellar keys fund "$name" --network "$NETWORK" >/dev/null || true
  else
    log "generate+fund $name"
    stellar keys generate "$name" --fund
  fi
}

SENIOR_AMT=750000000    # 75 nIN (7 decimals)
JUNIOR_AMT=250000000    # 25 nIN
LOAN_AMT=100000000      # 10 nIN — well under the ₹10,000 cap
LOAN_TYPE="$(python3 -c 'print("11" + "00"*31)')"
LOAN_ID="$(python3 -c 'print("42" + "00"*31)')"

log "identities"
for id in nila-admin nila-oracle nila-leader nila-union nila-borrower nila-junior nila-senior; do
  gen_funded "$id"
done

ADMIN=$(addr nila-admin)
ORACLE=$(addr nila-oracle)
LEADER=$(addr nila-leader)
UNION=$(addr nila-union)
BORROWER=$(addr nila-borrower)
JUNIOR=$(addr nila-junior)
SENIOR=$(addr nila-senior)

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  log "build wasm"
  stellar contract build
else
  log "skip build (SKIP_BUILD=1)"
fi
WASM_DIR="$ROOT/target/wasm32v1-none/release"
test -f "$WASM_DIR/nila_fund_core.wasm"

log "deploy nIN and stand-in USDC SACs (issuer = nila-admin)"
stellar contract asset deploy --asset "nIN:${ADMIN}" --source-account nila-admin --network "$NETWORK" --alias nila_nin
stellar contract asset deploy --asset "nUSD:${ADMIN}" --source-account nila-admin --network "$NETWORK" --alias nila_nusd
NIN=$(stellar contract alias show nila_nin --network "$NETWORK")
USDC=$(stellar contract alias show nila_nusd --network "$NETWORK")
log "NIN_SAC=$NIN"
log "USDC_SAC=$USDC"

ROLES=$(deploy_wasm nila_roles nila_roles --admin nila-admin)
MEMBERSHIP=$(deploy_wasm nila_membership nila_membership --admin nila-admin --attester nila-admin)
VIEWER=$(deploy_wasm nila_fund_viewer nila_viewer --admin nila-admin)
CORE=$(deploy_wasm nila_fund_core nila_core \
  --admin nila-admin \
  --roles "$ROLES" \
  --membership "$MEMBERSHIP" \
  --viewer "$VIEWER" \
  --nin_token "$NIN")
FX=$(deploy_wasm nila_fx_pool nila_fx \
  --admin nila-admin \
  --usdc "$USDC" \
  --nin "$NIN" \
  --daily_cap 25000000000)

log "wire contracts"
stellar contract invoke --id "$VIEWER" --source nila-admin --network "$NETWORK" -- \
  set_core --core "$CORE"
stellar contract invoke --id "$MEMBERSHIP" --source nila-admin --network "$NETWORK" -- \
  set_roles --roles "$ROLES"
stellar contract invoke --id "$FX" --source nila-admin --network "$NETWORK" -- \
  set_fund_core --fund_core "$CORE"
stellar contract invoke --id "$CORE" --source nila-admin --network "$NETWORK" -- \
  set_fx_pool --v "$FX"

log "roles + reserve"
stellar contract invoke --id "$ROLES" --source nila-admin --network "$NETWORK" -- \
  set_oracle --account nila-oracle --enabled true
stellar contract invoke --id "$ROLES" --source nila-admin --network "$NETWORK" -- \
  set_leader --union nila-union --account nila-leader --enabled true
stellar contract invoke --id "$CORE" --source nila-admin --network "$NETWORK" -- \
  set_reserve_config \
    --who nila-admin \
    --union nila-union \
    --safety_bp 0 \
    --safety_floor 0 \
    --hard_stop false \
    --escrow_duration 0

log "membership (borrower + junior LP)"
POLY="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PLOTS='[1]'
stellar contract invoke --id "$MEMBERSHIP" --source nila-admin --network "$NETWORK" \
  --auto-sign -- \
  confirm_member \
    --union nila-union \
    --member nila-borrower \
    --leader nila-leader \
    --polygon_address "$POLY" \
    --plot_ids "$PLOTS" \
    --cycle 1
stellar contract invoke --id "$MEMBERSHIP" --source nila-admin --network "$NETWORK" \
  --auto-sign -- \
  confirm_member \
    --union nila-union \
    --member nila-junior \
    --leader nila-leader \
    --polygon_address "$POLY" \
    --plot_ids "$PLOTS" \
    --cycle 1

log "trustlines + mint nIN"
for acct in nila-junior nila-senior nila-borrower; do
  stellar tx new change-trust --source-account "$acct" --network "$NETWORK" --line "nIN:${ADMIN}"
done
stellar contract invoke --id "$NIN" --source nila-admin --network "$NETWORK" -- \
  mint --to nila-junior --amount "$JUNIOR_AMT"
stellar contract invoke --id "$NIN" --source nila-admin --network "$NETWORK" -- \
  mint --to nila-senior --amount "$SENIOR_AMT"
stellar contract invoke --id "$NIN" --source nila-admin --network "$NETWORK" -- \
  mint --to nila-borrower --amount "$LOAN_AMT"

log "deposits"
stellar contract invoke --id "$CORE" --source nila-junior --network "$NETWORK" -- \
  deposit_junior --investor nila-junior --union nila-union --loan_type "$LOAN_TYPE" --amount "$JUNIOR_AMT"
stellar contract invoke --id "$CORE" --source nila-senior --network "$NETWORK" -- \
  deposit_senior --investor nila-senior --union nila-union --amount "$SENIOR_AMT"

log "draw loan (fast-draw, oracle + borrower auth)"
stellar contract invoke --id "$CORE" --source nila-borrower --network "$NETWORK" --auto-sign -- \
  draw_loan --borrower nila-borrower --oracle nila-oracle --union nila-union --p "{
    \"loan_id\": \"$LOAN_ID\",
    \"loan_type\": \"$LOAN_TYPE\",
    \"amount\": \"$LOAN_AMT\",
    \"rate_bp\": 1200,
    \"maturity_ts\": 0,
    \"max_amount\": \"$LOAN_AMT\",
    \"min_rate_bp\": 1000,
    \"fast_draw\": true,
    \"escrow_id\": 0,
    \"sos_date\": 0
  }"

log "views after draw"
stellar contract invoke --id "$CORE" --source nila-admin --network "$NETWORK" --send=no -- \
  get_junior_market --union nila-union --loan_type "$LOAN_TYPE"
stellar contract invoke --id "$CORE" --source nila-admin --network "$NETWORK" --send=no -- \
  get_senior_market --union nila-union
stellar contract invoke --id "$NIN" --source nila-admin --network "$NETWORK" --send=no -- \
  balance --id nila-borrower
stellar contract invoke --id "$CORE" --source nila-admin --network "$NETWORK" --send=no -- \
  union_borrows --union nila-union

log "repay principal (interest ~0 on same ledger)"
stellar contract invoke --id "$CORE" --source nila-borrower --network "$NETWORK" -- \
  repay_loan --payer nila-borrower --union nila-union --loan_id "$LOAN_ID" --amount "$LOAN_AMT"

FULLY=$(stellar contract invoke --id "$CORE" --source nila-admin --network "$NETWORK" --send=no -- \
  get_loan --union nila-union --loan_id "$LOAN_ID")
log "loan after repay: $FULLY"

mkdir -p "$ROOT/.stellar"
cat > "$ROOT/.stellar/${NETWORK}-addresses.env" <<EOF
NETWORK=$NETWORK
ADMIN=$ADMIN
ORACLE=$ORACLE
LEADER=$LEADER
UNION=$UNION
BORROWER=$BORROWER
JUNIOR=$JUNIOR
SENIOR=$SENIOR
ROLES=$ROLES
MEMBERSHIP=$MEMBERSHIP
VIEWER=$VIEWER
FUND_CORE=$CORE
FX_POOL=$FX
NIN=$NIN
USDC=$USDC
LOAN_ID=$LOAN_ID
LOAN_TYPE=$LOAN_TYPE
EOF
log "wrote .stellar/${NETWORK}-addresses.env"
cat "$ROOT/.stellar/${NETWORK}-addresses.env"
