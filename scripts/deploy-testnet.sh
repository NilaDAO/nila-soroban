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
# fund_core / fx_pool built with testnet-short-windows are for the c*-testnet.sh rehearsals only.
if LC_ALL=C grep -a -l "testnet-short" "$WASM_DIR"/*.wasm; then
  echo "refusing: a wasm above carries the testnet-short-windows marker" >&2
  exit 1
fi

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

# Land titles mirrored from Polygon; holding one is membership. The attester is a role in roles.
LAND_TITLE=$(deploy nila_land_title --admin "$SOURCE" --roles "$ROLES")
echo "land_title=$LAND_TITLE"

VIEWER=$(deploy nila_fund_viewer --admin "$SOURCE")
echo "viewer=$VIEWER"

# nIN is a classic asset (7 decimals). Pass its SAC address as --nin_token.
NIN="${NIN_SAC:?set NIN_SAC to the nIN Stellar Asset Contract address}"
CORE=$(deploy nila_fund_core \
  --admin "$SOURCE" \
  --roles "$ROLES" \
  --land_title "$LAND_TITLE" \
  --nin_token "$NIN")
echo "fund_core=$CORE"

USDC="${USDC_SAC:?set USDC_SAC to the USDC SAC address}"
DAILY_CAP="${DAILY_CAP:-25000000000}" # 2,500 USDC at 7 decimals
FX=$(deploy nila_fx_pool \
  --admin "$SOURCE" \
  --roles "$ROLES" \
  --land_title "$LAND_TITLE" \
  --nin "$NIN" \
  --usdc "$USDC" \
  --daily_cap "$DAILY_CAP")
echo "fx_pool=$FX"

stellar contract invoke --id "$FX" --source-account "$SOURCE" --network "$NETWORK" -- \
  set_fund_core --fund_core "$CORE"
stellar contract invoke --id "$CORE" --source-account "$SOURCE" --network "$NETWORK" -- \
  set_fx_pool --fx_pool "$FX"

# R8: the nIN issuer hands the SAC admin to fx_pool (only it mints, claws back and authorises
# trustlines), then fx_pool authorises its own and fund_core's balances. The issuer signs:
#   stellar contract invoke --id "$NIN" --source-account <issuer> -- set_admin --new_admin "$FX"
#   stellar contract invoke --id "$FX" --source-account "$SOURCE" -- activate
# Then lock the issuer (master weight 0) or its classic ops still mint, authorise and claw back.
# Mainnet too: the trust locks the issuer right after set_admin (executive decision, 10 Oct).
echo "==> next: issuer runs set_admin(fx_pool) on the nIN SAC, then fx_pool.activate (see above)"

mkdir -p "$ROOT/.stellar"
cat > "$ROOT/.stellar/${NETWORK}-addresses.env" <<EOF
ROLES=$ROLES
LAND_TITLE=$LAND_TITLE
VIEWER=$VIEWER
FUND_CORE=$CORE
FX_POOL=$FX
NIN=$NIN
USDC=$USDC
EOF
echo "==> Wrote .stellar/${NETWORK}-addresses.env"
