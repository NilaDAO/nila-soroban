#!/usr/bin/env bash
# Deploy Nila contracts to Stellar testnet.
# Requires: stellar CLI, a funded identity named `nila-admin`.
#
#   stellar keys generate nila-admin --network testnet --fund
#   bash scripts/deploy-testnet.sh
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
SOURCE="${SOURCE:-nila-admin}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v stellar >/dev/null; then
  echo "Install stellar-cli: cargo install --locked stellar-cli" >&2
  exit 1
fi

echo "==> Building WASM"
stellar contract build

WASM_DIR="$ROOT/target/wasm32v1-none/release"

deploy() {
  local pkg="$1"
  shift
  local wasm="$WASM_DIR/${pkg}.wasm"
  echo "==> Deploy $pkg"
  stellar contract deploy \
    --wasm "$wasm" \
    --source-account "$SOURCE" \
    --network "$NETWORK" \
    --alias "$pkg" \
    -- \
    "$@"
}

# Constructor args: after `--`
ROLES=$(deploy nila_roles --admin "$SOURCE")
echo "roles=$ROLES"

ATTESTER="${ATTESTER:-$SOURCE}"
MEMBERSHIP=$(deploy nila_membership --admin "$SOURCE" --attester "$ATTESTER")
echo "membership=$MEMBERSHIP"

VIEWER=$(deploy nila_fund_viewer --admin "$SOURCE")
echo "viewer=$VIEWER"

# nIN is a classic asset (7 decimals). Pass its SAC address as --nin_token.
NIN="${NIN_SAC:?set NIN_SAC to the nIN Stellar Asset Contract address}"
CORE=$(deploy nila_fund_core \
  --admin "$SOURCE" \
  --roles "$ROLES" \
  --membership "$MEMBERSHIP" \
  --viewer "$VIEWER" \
  --nin_token "$NIN")
echo "fund_core=$CORE"

USDC="${USDC_SAC:?set USDC_SAC to the USDC SAC address}"
DAILY_CAP="${DAILY_CAP:-25000000000}" # 2,500 USDC at 7 decimals
FX=$(deploy nila_fx_pool \
  --admin "$SOURCE" \
  --usdc "$USDC" \
  --nin "$NIN" \
  --daily_cap "$DAILY_CAP")
echo "fx_pool=$FX"

stellar contract invoke --id "$VIEWER" --source-account "$SOURCE" --network "$NETWORK" -- \
  set_core --core "$CORE"
stellar contract invoke --id "$MEMBERSHIP" --source-account "$SOURCE" --network "$NETWORK" -- \
  set_roles --roles "$ROLES"
stellar contract invoke --id "$FX" --source-account "$SOURCE" --network "$NETWORK" -- \
  set_fund_core --fund_core "$CORE"
stellar contract invoke --id "$CORE" --source-account "$SOURCE" --network "$NETWORK" -- \
  set_fx_pool --v "$FX"

mkdir -p "$ROOT/.stellar"
cat > "$ROOT/.stellar/${NETWORK}-addresses.env" <<EOF
ROLES=$ROLES
MEMBERSHIP=$MEMBERSHIP
VIEWER=$VIEWER
FUND_CORE=$CORE
FX_POOL=$FX
NIN=$NIN
USDC=$USDC
EOF
echo "==> Wrote .stellar/${NETWORK}-addresses.env"
