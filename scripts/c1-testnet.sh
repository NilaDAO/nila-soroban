#!/usr/bin/env bash
# Step C1 on Stellar testnet: nIN asset, roles with a 3-of-5 union account (D12), membership.
# Usage: bash scripts/c1-testnet.sh        (testnet only; refuses any other network)
#
# Identities live in the stellar CLI config (~/.config/stellar), never in this repo, and are
# never printed — only public addresses are. The nIN issuer is a throwaway testnet key; the
# trust binds as issuer only at mainnet (D7, X14).
set -euo pipefail

NETWORK="${NETWORK:-testnet}"
[[ "$NETWORK" == "testnet" ]] || { echo "C1 runs on testnet only" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export STELLAR_NETWORK="$NETWORK"
HORIZON="https://horizon-testnet.stellar.org"
OUT="$ROOT/.stellar/c1-testnet.env"
WASM_DIR="$ROOT/target/wasm32v1-none/release"
# One prefix per run: a union account cannot be re-keyed once its master weight is 0.
P="${PREFIX:-c1r$(date +%m%d%H%M)}"

log() { printf '\n==> %s\n' "$*" >&2; }
addr() { stellar keys address "$1"; }
# Run a stellar command, echo the tx hash it signed (last one) on stdout, fail on error.
tx() {
  local out
  out=$("$@" 2>&1) || { echo "$out" >&2; return 1; }
  grep -oE 'Signing transaction: [0-9a-f]{64}' <<<"$out" | tail -1 | awk '{print $3}'
}
ident() {
  local name="$1" fund="${2:-}"
  if ! addr "$name" >/dev/null 2>&1; then
    stellar keys generate "$name" ${fund:+--fund} -q
  fi
}
horizon() { curl -fsS "$HORIZON/$1"; }

log "identities"
for n in admin nin-issuer attester oracle union leader member member2; do
  ident "${P}-$n" fund
done
for i in 1 2 3 4 5; do ident "${P}-union-s$i"; done # signer keys only, no accounts
ISSUER=$(addr ${P}-nin-issuer)
UNION=$(addr ${P}-union)

log "nIN issuer flags: auth required, revocable, clawback (R8)"
TX_FLAGS=$(tx stellar tx new set-options --source ${P}-nin-issuer \
  --set-required --set-revocable --set-clawback-enabled)

log "nIN Stellar Asset Contract (SAC admin stays the issuer until fx_pool exists, C3)"
TX_SAC=$(tx stellar contract asset deploy --asset "nIN:$ISSUER" --source ${P}-nin-issuer --alias ${P}_nin)
NIN=$(stellar contract alias show ${P}_nin)

log "union account: 5 signers, weight 1 each, thresholds 3, master weight 0 (D12)"
X=$(stellar tx new set-options --inclusion-fee 1000 --source ${P}-union \
  --signer "$(addr ${P}-union-s1)" --signer-weight 1 --build-only)
for i in 2 3 4 5; do
  X=$(stellar tx op add set-options --source-account ${P}-union \
    --signer "$(addr "${P}-union-s$i")" --signer-weight 1 <<<"$X")
done
X=$(stellar tx op add set-options --source-account ${P}-union \
  --master-weight 0 --low-threshold 3 --med-threshold 3 --high-threshold 3 <<<"$X")
X=$(stellar tx sign --sign-with-key ${P}-union -q <<<"$X")
TX_UNION=$(stellar tx hash <<<"$X")
stellar tx send -q <<<"$X" >/dev/null

log "build + deploy roles, membership"
stellar contract build -q
ROLES=$(stellar contract deploy --wasm "$WASM_DIR/nila_roles.wasm" --source ${P}-admin \
  --alias ${P}_roles -q -- --admin ${P}-admin)
MEMBERSHIP=$(stellar contract deploy --wasm "$WASM_DIR/nila_membership.wasm" --source ${P}-admin \
  --alias ${P}_membership -q -- --admin ${P}-admin --roles "$ROLES")

log "trust-wide roles (admin): attester=1, oracle=0"
TX_ROLE_ATT=$(tx stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_role --account ${P}-attester --role 1 --enabled true)
TX_ROLE_ORA=$(tx stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_role --account ${P}-oracle --role 0 --enabled true)

log "set_leader by the admin key: must fail"
if stellar contract invoke --id "$ROLES" --source ${P}-admin -- \
  set_leader --union "$UNION" --account ${P}-leader --enabled true >/dev/null 2>&1; then
  echo "admin could set a leader — D12 broken" >&2
  exit 1
fi

log "set_leader by the union account: 2 of 5 must fail, 3 of 5 must pass"
X=$(stellar contract invoke --id "$ROLES" --source "$UNION" --build-only -- \
  set_leader --union "$UNION" --account ${P}-leader --enabled true)
X=$(stellar tx simulate --source-account "$UNION" <<<"$X")
X=$(stellar tx sign --sign-with-key ${P}-union-s1 -q <<<"$X")
X=$(stellar tx sign --sign-with-key ${P}-union-s2 -q <<<"$X")
if stellar tx send -q <<<"$X" >/dev/null 2>&1; then
  echo "2 of 5 signatures were enough — D12 broken" >&2
  exit 1
fi
X=$(stellar tx sign --sign-with-key ${P}-union-s4 -q <<<"$X")
TX_LEADER=$(stellar tx hash <<<"$X")
stellar tx send -q <<<"$X" >/dev/null
[[ $(stellar contract invoke --id "$ROLES" --source ${P}-admin --send=no -- \
  is_leader --union "$UNION" --account ${P}-leader) == "true" ]]

log "membership: confirm (leader + attester), set_plots, reattest, set_record, revoke"
POLY=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
confirm() {
  tx stellar contract invoke --id "$MEMBERSHIP" --source ${P}-leader --auto-sign -- \
    confirm_member --union "$UNION" --member "$1" --leader ${P}-leader --attester ${P}-attester \
    --polygon_address "$POLY" --plot_ids '[101,102]' --cycle 1
}
TX_CONFIRM=$(confirm ${P}-member)
TX_CONFIRM2=$(confirm ${P}-member2)
TX_PLOTS=$(tx stellar contract invoke --id "$MEMBERSHIP" --source ${P}-attester -- \
  set_plots --union "$UNION" --member ${P}-member --attester ${P}-attester --plot_ids '[102]')
TX_REATTEST=$(tx stellar contract invoke --id "$MEMBERSHIP" --source ${P}-attester -- \
  reattest --union "$UNION" --member ${P}-member --attester ${P}-attester --cycle 2)
TX_RECORD=$(tx stellar contract invoke --id "$MEMBERSHIP" --source ${P}-attester -- \
  set_record --title_id 102 --attester ${P}-attester \
  --record_hash "$(printf 'ab%.0s' {1..32})" --metadata_hash "$(printf 'cd%.0s' {1..32})")
TX_REVOKE=$(tx stellar contract invoke --id "$MEMBERSHIP" --source ${P}-leader -- \
  revoke_member --union "$UNION" --member ${P}-member2 --leader ${P}-leader)

log "nIN gate: member trustline opens unauthorised; a mint to it must fail (R9, R10)"
TX_TRUST=$(tx stellar tx new change-trust --source ${P}-member --line "nIN:$ISSUER")
if stellar contract invoke --id "$NIN" --source ${P}-nin-issuer -- \
  mint --to ${P}-member --amount 10000000 >/dev/null 2>&1; then
  echo "mint to an unauthorised trustline succeeded — R8 flags broken" >&2
  exit 1
fi

log "checks"
horizon "accounts/$ISSUER" | python3 -c 'import json,sys; print("issuer flags", json.load(sys.stdin)["flags"])'
horizon "accounts/$UNION" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("union thresholds", d["thresholds"], "signers", sorted(s["weight"] for s in d["signers"]))'
stellar contract invoke --id "$MEMBERSHIP" --source ${P}-admin --send=no -- \
  get_member --union "$UNION" --account ${P}-member

mkdir -p "$ROOT/.stellar"
cat >"$OUT" <<EOF
# C1 testnet run $(date -u +%Y-%m-%dT%H:%MZ), identity prefix $P — public addresses and tx hashes only
NIN_ISSUER=$ISSUER
NIN_SAC=$NIN
UNION=$UNION
ROLES=$ROLES
MEMBERSHIP=$MEMBERSHIP
TX_FLAGS=$TX_FLAGS
TX_SAC=$TX_SAC
TX_UNION=$TX_UNION
TX_ROLE_ATT=$TX_ROLE_ATT
TX_ROLE_ORA=$TX_ROLE_ORA
TX_LEADER=$TX_LEADER
TX_CONFIRM=$TX_CONFIRM
TX_CONFIRM2=$TX_CONFIRM2
TX_PLOTS=$TX_PLOTS
TX_REATTEST=$TX_REATTEST
TX_RECORD=$TX_RECORD
TX_REVOKE=$TX_REVOKE
TX_TRUST=$TX_TRUST
EOF
log "wrote $OUT (explorer: https://stellar.expert/explorer/testnet/tx/<hash>)"
cat "$OUT"
