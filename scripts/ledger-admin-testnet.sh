#!/usr/bin/env bash
# The trust admin on a Ledger, rehearsed on testnet (3 Nov 2026). Run it with the Ledger plugged in, unlocked and the
# Stellar app open; approve each prompt on the device.
#
#   HD=1 scripts/ledger-admin-testnet.sh
#
# HD is the account index on the Ledger (m/44'/148'/HD'). Use one that is NOT the address the grant money arrives
# on. The script derives the address, funds it from friendbot (testnet only), deploys a roles contract with that
# address as admin (deployed by a hot key), and makes one admin call signed on the Ledger. Nothing touches mainnet.
set -euo pipefail
cd "$(dirname "$0")/.."

HD="${HD:-1}"
NAME="${NAME:-nila-admin-ledger}"
NET=testnet
WASM=target/wasm32v1-none/release/nila_roles.wasm

stellar keys add "$NAME" --ledger --hd-path "$HD" --overwrite
ADMIN=$(stellar keys address "$NAME")
echo "Ledger admin (m/44'/148'/$HD'): $ADMIN"
echo "Check this is not the grant address before going on. Ctrl-C to stop, Enter to continue."
read -r

curl -sf "https://friendbot.stellar.org?addr=$ADMIN" >/dev/null && echo "funded on testnet" || echo "friendbot: already funded?"
[ -f "$WASM" ] || make build

# The deployer is a plain hot key (as on mainnet, checklist C): a wasm upload is too large for the Ledger to
# show, and deploying needs no admin power. The constructor names the Ledger account as admin.
stellar keys generate nila-ledger-deployer --fund --network "$NET" --overwrite >/dev/null
echo "Deploy roles with the Ledger account as admin (hot deployer key, no device prompt)"
ROLES=$(stellar contract deploy --wasm "$WASM" --source nila-ledger-deployer --network "$NET" -- --admin "$ADMIN")
echo "roles: $ROLES"

echo "Admin call: give a throwaway key the attester role (approve on the device)"
stellar keys generate nila-ledger-check --overwrite >/dev/null
CHECK=$(stellar keys address nila-ledger-check)
stellar contract invoke --id "$ROLES" --source "$NAME" --sign-with-ledger --network "$NET" -- \
  set_role --account "$CHECK" --role 1 --enabled true

[ "$(stellar contract invoke --id "$ROLES" --source "$NAME" --network "$NET" --send no -- admin)" = "\"$ADMIN\"" ] \
  && echo "LEDGER ADMIN CHECK PASS: roles admin = $ADMIN, admin call signed on the Ledger" \
  || { echo "LEDGER ADMIN CHECK FAIL: admin differs"; exit 1; }
