#!/usr/bin/env bash
# scripts/mainnet-deploy.sh — go-live steps 3–4: the Nila contracts on Stellar, in the order of
# docs/MAINNET_CHECKLIST.md C (0–18), then the read-only checks of D. Rehearse on testnet first.
#
#   scripts/mainnet-deploy.sh <config>              run every step not yet done (resumable)
#   scripts/mainnet-deploy.sh <config> verify       only the read-only checks (D) and the env blocks
#   scripts/mainnet-deploy.sh <config> lock-issuer  checklist C10: lock the nIN issuer (counsel gate X14)
#
# <config> is a bash file of PUBLIC values only (scripts/mainnet-deploy.conf.example). Keys stay in the stellar CLI:
#   DEPLOYER_ID  hot key: uploads the wasm and deploys; pays the fees. Has no power once the contracts exist.
#   ADMIN_ID     the trust admin (a Ledger identity: `stellar keys add <name> --ledger --hd-path N`). Every admin call
#                is signed on the device: approve each prompt. It is passed as --source, never --sign-with-ledger.
#   ISSUER_ID    the nIN issuer (the trust). Sets the flags and home_domain, deploys the nIN SAC, hands its admin to
#                fx_pool. The lock (C10) is a separate command.
#   UNION_ID     optional: the union account, when its key is a CLI identity (testnet, or the union's first key on
#                mainnet). Without it the union's own calls are written to <state>.union-calls.sh for its signers.
#
# State: .deploy/deploy-<NET>.env (gitignored) holds the ids and which steps are done, so a run that stops (a
# rejected Ledger prompt, an RPC error) continues where it stopped. Public values only. Delete it to start over.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
CONF="${1:?usage: scripts/mainnet-deploy.sh <config> [verify|lock-issuer]}"
MODE="${2:-run}"
[ -f "$CONF" ] || { echo "no config $CONF" >&2; exit 1; }
# shellcheck disable=SC1090
. "$CONF"

say() { printf '\033[1m[deploy]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31m[deploy]\033[0m %s\n' "$*" >&2; exit 1; }
need() { for v in "$@"; do [ -n "${!v:-}" ] || die "config: $v is empty"; done; }
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
command -v stellar >/dev/null || die "stellar CLI not found"

# ── network ────────────────────────────────────────────────────────────────────
need NET RPC_URL
case "$NET" in
  testnet) export STELLAR_NETWORK_PASSPHRASE="Test SDF Network ; September 2015"; HORIZON=https://horizon-testnet.stellar.org ;;
  mainnet) export STELLAR_NETWORK_PASSPHRASE="Public Global Stellar Network ; September 2015"; HORIZON=https://horizon.stellar.org ;;
  *) die "NET must be testnet or mainnet" ;;
esac
export STELLAR_RPC_URL="$RPC_URL"
unset STELLAR_NETWORK   # the CLI's default network on this Mac is testnet: the two variables above decide

mkdir -p .deploy
STATE="$ROOT/.deploy/deploy-$NET.env"
UNION_CALLS="$ROOT/.deploy/deploy-$NET.union-calls.sh"
touch "$STATE"
get() { sed -n "s/^$1=//p" "$STATE" | tail -1; }
put() { grep -v "^$1=" "$STATE" >"$STATE.tmp" || true; echo "$1=$2" >>"$STATE.tmp"; mv "$STATE.tmp" "$STATE"; }
done_() { [ "$(get "DONE_$1")" = 1 ]; }
mark() { put "DONE_$1" 1; }

need DEPLOYER_ID ADMIN_ID
addr() { stellar keys address "$1"; }
DEPLOYER=$(addr "$DEPLOYER_ID") || die "no identity $DEPLOYER_ID"
ADMIN=$(addr "$ADMIN_ID") || die "no identity $ADMIN_ID"
WASM="$ROOT/target/wasm32v1-none/release"
NIN=10000000
LOG="$ROOT/.deploy/deploy-$NET.last.err"

# A call signed by <source> (the Ledger prompts for the admin). Fails loudly with the CLI's own message.
inv() { local id="$1" src="$2"; shift 2
  stellar contract invoke --id "$id" --source "$src" -- "$@" >/dev/null 2>"$LOG" || { cat "$LOG" >&2; die "invoke $1 on $id failed"; }; }
# A read: simulated only, nothing sent, nothing signed.
view() { local id="$1"; shift; stellar contract invoke --id "$id" --source "$DEPLOYER_ID" --send=no -- "$@" 2>/dev/null; }
q() { tr -d '"'; }

# ── guards ─────────────────────────────────────────────────────────────────────
if [ "$NET" = mainnet ] && [ "$MODE" != verify ]; then
  tag=$(git describe --tags --exact-match 2>/dev/null || true)
  [ -n "$tag" ] || die "mainnet deploys only from a release tag (git checkout mainnet-YYYY-MM-DD); this checkout is $(git rev-parse --short HEAD)"
  [ -z "$(git status --porcelain -- contracts Cargo.toml Cargo.lock)" ] || die "contracts differ from the tag $tag"
  say "MAINNET from tag $tag. Admin $ADMIN, deployer $DEPLOYER."
  printf 'Type "mainnet" to go on: ' >&2; read -r ok; [ "$ok" = mainnet ] || die "stopped"
fi

# ── build (release, as CI) ─────────────────────────────────────────────────────
build() {
  say "build (stellar contract build, release)"
  stellar contract build -q >/dev/null 2>"$LOG" || { cat "$LOG" >&2; die "build failed"; }
  for f in "$WASM"/*.wasm; do
    LC_ALL=C grep -a -q "testnet-short" "$f" && die "$(basename "$f") carries testnet-short-windows: never deploy it"
    LC_ALL=C grep -a -q "demo-upgrade-delay" "$f" && die "$(basename "$f") carries demo-upgrade-delay (60 s timelock): never deploy it"
  done
  return 0
}

# Upload a wasm as the deployer and extend its lease in steps (a fresh upload's short lease would make the first
# write extend it in one go, a rent that does not fit one transaction's fee). Prints the hash.
upload() {
  local w="$1" h
  h=$(stellar contract upload --wasm "$WASM/$w.wasm" --source "$DEPLOYER_ID" -q 2>"$LOG") || { cat "$LOG" >&2; die "upload $w failed"; }
  for x in 300000 1000000 2073600; do
    stellar contract extend --source-account "$DEPLOYER_ID" --wasm-hash "$h" --ledgers-to-extend "$x" --ttl-ledger-only \
      >/dev/null 2>"$LOG" || { cat "$LOG" >&2; die "extending the lease of $w failed"; }
  done
  echo "$h"
}
# Deploy once (by the deployer; the constructor names the admin). The id goes into the state under KEY.
deploy() {
  local key="$1" w="$2"; shift 2
  [ -n "$(get "$key")" ] && { say "$key: $(get "$key") (kept)"; return 0; }
  local h id
  h=$(upload "$w")
  id=$(stellar contract deploy --wasm-hash "$h" --source "$DEPLOYER_ID" -q -- "$@" 2>"$LOG") || { cat "$LOG" >&2; die "deploy $w failed"; }
  put "$key" "$id"; put "WASM_$key" "$h"
  say "$key: $id"
}

# ── the union's own calls: signed here when UNION_ID is a CLI identity, else written out for its signers ──────
UNION="${UNION:-}"
[ -n "${UNION_ID:-}" ] && UNION=$(addr "$UNION_ID")
union_call() {   # contract fn args… (the union account signs)
  if [ -n "${UNION_ID:-}" ]; then inv "$1" "$UNION_ID" "${@:2}"; return; fi
  printf 'stellar contract invoke --id %s --source "$UNION_SIGNER" -- %s\n' "$1" "$(printf '%q ' "${@:2}")" >>"$UNION_CALLS"
}

# An asset's SAC id, deployed by <source> unless it already exists (Circle's USDC SAC exists on both networks).
sac() {
  local id; id=$(stellar contract id asset --asset "$1" 2>"$LOG") || { cat "$LOG" >&2; die "no SAC id for $1"; }
  if [ -z "$(view "$id" symbol)" ]; then
    stellar contract asset deploy --asset "$1" --source "$2" -q >/dev/null 2>"$LOG" || { cat "$LOG" >&2; die "SAC deploy for $1 failed"; }
  fi
  [ -n "$(view "$id" symbol)" ] || die "the SAC of $1 does not answer"
  echo "$id"
}
details_hash() { printf '%s\0%s' "$UNION_NAME" "$UNION_GEO" | shasum -a 256 | cut -d' ' -f1; }
hex() { printf %s "$1" | xxd -p | tr -d '\n'; }

# ═════════════════════════════════════════════════════════════════════════════
run_all() {
  need ISSUER_ID USDC_ASSET HOME_DOMAIN ATTESTER ORACLE SMS_KEY UNION UNION_NAME UNION_GEO UNION_DISPLAY LEADERS CAL_WINDOWS
  [ -f "$CAL_WINDOWS" ] || die "CAL_WINDOWS: no file $CAL_WINDOWS (nila-sensing scripts/crop_calendar.py output)"
  [ ${#UNION_NAME} -le 64 ] || die "UNION_NAME is longer than 64 bytes"
  [[ "$UNION_GEO" =~ ^[0-9bcdefghjkmnpqrstuvwxyz]{5,7}$ ]] || die "UNION_GEO must be a 5–7 character geohash"
  ISSUER=$(addr "$ISSUER_ID")
  build
  : >"$UNION_CALLS"

  # C0 — issuer flags and home domain, the nIN SAC; the USDC SAC (Circle's on mainnet: deployed once by anyone)
  if ! done_ C0; then
    say "C0 issuer: auth required, revocable, clawback; home_domain $HOME_DOMAIN (stellar.toml must be published before the lock)"
    stellar tx new set-options --source "$ISSUER_ID" --set-required --set-revocable --set-clawback-enabled \
      --home-domain "$HOME_DOMAIN" >/dev/null 2>"$LOG" || { cat "$LOG" >&2; die "issuer set-options failed"; }
    put NIN_ASSET "nIN:$ISSUER"
    put NIN_SAC "$(sac "nIN:$ISSUER" "$ISSUER_ID")"
    put USDC_SAC "$(sac "$USDC_ASSET" "$DEPLOYER_ID")"
    mark C0
  fi
  local NIN_SAC USDC_SAC; NIN_SAC=$(get NIN_SAC); USDC_SAC=$(get USDC_SAC)

  # C1–C5a, C16, C17, C18 — deploys (deployer key; each constructor names the trust admin)
  # The anchor's union directory reads roles events from here on (a ledger before the roles deploy).
  [ -n "$(get ROLES_LEDGER)" ] || put ROLES_LEDGER "$(curl -s "$HORIZON/ledgers?order=desc&limit=1" | python3 -c 'import sys,json;print(json.load(sys.stdin)["_embedded"]["records"][0]["sequence"])')"
  say "deploy: roles, land_title, fund_viewer, fund_core, fx_pool, machinery, crop_calendar, senior, deployer"
  deploy ROLES nila_roles --admin "$ADMIN"
  deploy LAND_TITLE nila_land_title --admin "$ADMIN" --roles "$(get ROLES)"
  deploy FUND_VIEWER nila_fund_viewer --admin "$ADMIN"
  deploy FUND_CORE nila_fund_core --admin "$ADMIN" --roles "$(get ROLES)" --land_title "$(get LAND_TITLE)" --nin_token "$NIN_SAC"
  deploy FX_POOL nila_fx_pool --admin "$ADMIN" --roles "$(get ROLES)" --land_title "$(get LAND_TITLE)" --nin "$NIN_SAC" \
    --usdc "$USDC_SAC" --daily_cap "$((${DAILY_CAP_USDC:-2500} * NIN))"
  deploy MACHINERY nila_machinery --admin "$ADMIN" --roles "$(get ROLES)" --land_title "$(get LAND_TITLE)"
  deploy CROP_CALENDAR nila_crop_calendar --admin "$ADMIN"
  deploy SENIOR nila_senior --admin "$ADMIN" --usdc "$USDC_SAC" --fund_core "$(get FUND_CORE)" --fx_pool "$(get FX_POOL)" \
    --roles "$(get ROLES)"
  if [ -z "$(get ACCOUNT_DEPLOYER)" ]; then
    local aw; aw=$(upload nila_account)
    [ "$aw" = "$(shasum -a 256 "$WASM/nila_account.wasm" | cut -d' ' -f1)" ] || die "uploaded account wasm hash differs from the build"
    put ACCOUNT_WASM "$aw"
    local dh dep; dh=$(upload nila_deployer)
    dep=$(stellar contract deploy --wasm-hash "$dh" --source "$DEPLOYER_ID" -q -- --account_wasm "$aw" --roles "$(get ROLES)" 2>"$LOG") \
      || { cat "$LOG" >&2; die "deploy nila_deployer failed"; }
    put ACCOUNT_DEPLOYER "$dep"; put WASM_ACCOUNT_DEPLOYER "$dh"
    say "ACCOUNT_DEPLOYER: $(get ACCOUNT_DEPLOYER) (account wasm $aw; immutable)"
  fi

  local ROLES CORE FX MACH CAL SEN VIEWER
  ROLES=$(get ROLES); CORE=$(get FUND_CORE); FX=$(get FX_POOL); MACH=$(get MACHINERY); CAL=$(get CROP_CALENDAR)
  SEN=$(get SENIOR); VIEWER=$(get FUND_VIEWER)

  # C6, C7, C7a, C16, C17 — links, each set once (trust admin: Ledger prompts)
  if ! done_ LINKS; then
    say "links (trust admin, approve on the Ledger): fx_pool ⇄ fund_core, machinery ⇄ fund_core, crop calendar, senior"
    inv "$FX" "$ADMIN_ID" set_fund_core --fund_core "$CORE"
    inv "$CORE" "$ADMIN_ID" set_fx_pool --fx_pool "$FX"
    inv "$MACH" "$ADMIN_ID" set_fund_core --fund_core "$CORE"
    inv "$CORE" "$ADMIN_ID" set_machinery --machinery "$MACH"
    inv "$CORE" "$ADMIN_ID" set_crop_calendar --calendar "$CAL"
    inv "$CAL" "$ADMIN_ID" set_roles --roles "$ROLES"
    inv "$FX" "$ADMIN_ID" set_senior --senior "$SEN"
    inv "$CORE" "$ADMIN_ID" set_senior --senior "$SEN"
    mark LINKS
  fi

  # C8, C9 — the nIN SAC's admin to fx_pool (issuer), then activate (trust)
  if ! done_ C8; then say "C8 issuer: nIN SAC admin → fx_pool"; inv "$NIN_SAC" "$ISSUER_ID" set_admin --new_admin "$FX"; mark C8; fi
  if ! done_ C9; then say "C9 fx_pool.activate (Ledger)"; inv "$FX" "$ADMIN_ID" activate; mark C9; fi

  # C11 — oracle and attester roles
  if ! done_ C11; then
    say "C11 roles: oracle $ORACLE, attester $ATTESTER (Ledger)"
    inv "$ROLES" "$ADMIN_ID" set_role --account "$ORACLE" --role 0 --enabled true
    inv "$ROLES" "$ADMIN_ID" set_role --account "$ATTESTER" --role 1 --enabled true
    mark C11
  fi

  # C18 / C12 — the union's details on its own account, then the trust admits it (decision 008)
  if ! done_ ADMIT; then
    if [ -n "${UNION_ID:-}" ]; then
      say "union details on its own account: nila.name, nila.geo"
      stellar tx new manage-data --source-account "$UNION_ID" --data-name nila.name --data-value "$(hex "$UNION_NAME")" >/dev/null 2>"$LOG" \
        || { cat "$LOG" >&2; die "manageData nila.name failed"; }
      stellar tx new manage-data --source-account "$UNION_ID" --data-name nila.geo --data-value "$(hex "$UNION_GEO")" >/dev/null 2>"$LOG" \
        || { cat "$LOG" >&2; die "manageData nila.geo failed"; }
    else
      say "the union writes nila.name and nila.geo itself (see $UNION_CALLS); admitting with the hash of the config's values"
      printf '# the union account, before anything else (3 of 5 once it is a multisig):\n' >>"$UNION_CALLS"
      printf 'stellar tx new manage-data --source-account "$UNION_SIGNER" --data-name nila.name --data-value %s\n' "$(hex "$UNION_NAME")" >>"$UNION_CALLS"
      printf 'stellar tx new manage-data --source-account "$UNION_SIGNER" --data-name nila.geo --data-value %s\n' "$(hex "$UNION_GEO")" >>"$UNION_CALLS"
    fi
    say "C12 roles.admit_union($UNION, $(details_hash)) (Ledger)"
    inv "$ROLES" "$ADMIN_ID" admit_union --union "$UNION" --details_hash "$(details_hash)"
    mark ADMIT
  fi

  # C13, C14 — fund_viewer entry and the union's loan terms (trust)
  if ! done_ C13; then say "C13 fund_viewer.create_union (Ledger)"; inv "$VIEWER" "$ADMIN_ID" create_union --union "$UNION" --display_name "$UNION_DISPLAY"; mark C13; fi
  if ! done_ C14; then
    say "C14 union config: ₹10,000, gate 600, 12.5 %, fees 1 % + 0.77 %, amount not enforced (Ledger)"
    inv "$CORE" "$ADMIN_ID" set_union_config --union "$UNION" --uc "{
      \"max_loan\": \"$((10000 * NIN))\", \"gate\": \"$((600 * NIN))\", \"rate_bp\": 1250,
      \"treasury_fee_bp\": 100, \"rainy_fee_bp\": 77, \"safety_bp\": 0, \"safety_floor\": \"0\",
      \"hard_stop\": false, \"enforce_token_amount\": false }"
    mark C14
  fi

  # C16 — the crop calendar's windows, one family per line of CAL_WINDOWS (trust)
  if ! done_ C16; then
    say "C16 crop calendar windows from $CAL_WINDOWS (one Ledger prompt per family)"
    local fam windows
    while read -r fam windows; do
      [ -z "$fam" ] || [ "${fam:0:1}" = "#" ] && continue
      inv "$CAL" "$ADMIN_ID" set_windows --family "$fam" --windows "$windows"
      say "  family $fam: $windows"
    done <"$CAL_WINDOWS"
    mark C16
  fi

  # Anchor links (go-live step 5): only when the anchor's public keys are known
  if [ -n "${ANCHOR_ACCOUNT:-}" ] && [ -n "${ANCHOR_SWAP_KEY:-}" ] && ! done_ ANCHOR; then
    say "anchor: fx_pool.set_anchor, set_swap_key (Ledger)"
    inv "$FX" "$ADMIN_ID" set_anchor --anchor "$ANCHOR_ACCOUNT"
    inv "$FX" "$ADMIN_ID" set_swap_key --key "$(python3 -c 'import base64,sys;print(base64.b32decode(sys.argv[1])[1:33].hex())' "$ANCHOR_SWAP_KEY")"
    mark ANCHOR
  fi

  # The union's own calls: leaders, settings (C18, after admission), machine prices (C14a)
  if ! done_ UNION_CALLS; then
    say "the union's own calls: leaders, settings, machine prices$([ -z "${UNION_ID:-}" ] && echo " → written to $UNION_CALLS")"
    local l
    for l in $LEADERS; do union_call "$ROLES" set_leader --union "$UNION" --account "$l" --enabled true; done
    union_call "$ROLES" set_union_settings --union "$UNION" --settings \
      "{\"wait\": ${RECOVERY_WAIT:-14400}, \"session_len\": ${SESSION_LEN:-300}, \"session_contracts\": [\"$CORE\"], \"sms_key\": \"$SMS_KEY\", \"fx_pool\": \"$FX\", \"usdc\": \"$USDC_SAC\"}"
    # First prices an acre (checklist C14a, executive decision 27 Oct): the old price an hour × the catalogue's hours
    # an acre, as in nila-integrations scripts/demo/stellar-deploy.sh. The union adjusts them in the app later.
    rate() {   # type tenths-of-hours-an-acre driver diesel maintenance insurance depreciation
      local k=$2
      union_call "$MACH" set_rate --union "$UNION" --machine_type "$1" --parts "{\"driver\": \"$(($3 * k * NIN / 10))\", \"diesel\": \"$(($4 * k * NIN / 10))\", \"maintenance\": \"$(($5 * k * NIN / 10))\", \"insurance\": \"$(($6 * k * NIN / 10))\", \"depreciation\": \"$(($7 * k * NIN / 10))\", \"margin\": \"0\"}"
    }
    rate 1 15 290 350 160 170 130; rate 2 10 160 190 90 90 70;  rate 3 15 95 110 55 50 40;     rate 4 15 290 320 150 130 110
    rate 5 15 290 400 180 180 150; rate 6 15 290 360 160 160 130; rate 7 10 290 320 150 130 110; rate 8 30 290 360 160 160 130
    rate 9 10 290 250 100 90 70;   rate 10 40 290 280 120 110 100; rate 11 30 150 140 80 70 60; rate 12 20 200 180 150 140 130
    rate 13 20 300 380 220 200 200
    union_call "$MACH" set_crop_rate --union "$UNION" --machine_type 9 --crop_family 2 --parts "{\"driver\": \"$((1160 * NIN))\", \"diesel\": \"$((1120 * NIN))\", \"maintenance\": \"$((480 * NIN))\", \"insurance\": \"$((440 * NIN))\", \"depreciation\": \"$((400 * NIN))\", \"margin\": \"0\"}"
    [ -n "${UNION_ID:-}" ] && mark UNION_CALLS
  fi
  say "deploy steps done; the issuer is NOT locked (C10: scripts/mainnet-deploy.sh $CONF lock-issuer, after counsel gate X14)"
}

# ═════════════════════════════════════════════════════════════════════════════
lock_issuer() {
  need ISSUER_ID
  [ "$(get DONE_C8)" = 1 ] || die "C8 (nIN SAC admin → fx_pool) is not done: locking first would leave the SAC without its admin"
  say "C10: the nIN issuer gets master weight 0 and can sign nothing ever again. stellar.toml at $HOME_DOMAIN must be live."
  curl -sf "https://$HOME_DOMAIN/.well-known/stellar.toml" >/dev/null || die "https://$HOME_DOMAIN/.well-known/stellar.toml is not served"
  printf 'Type "lock" to lock the issuer %s: ' "$(addr "$ISSUER_ID")" >&2; read -r ok; [ "$ok" = lock ] || die "stopped"
  stellar tx new set-options --source "$ISSUER_ID" --master-weight 0 >/dev/null 2>"$LOG" || { cat "$LOG" >&2; die "lock failed"; }
  mark C10; say "issuer locked"
}

# ═════════════════════════════════════════════════════════════════════════════
fails=0
check() {  # label got want
  if [ "$2" = "$3" ]; then printf '  \033[32mok\033[0m   %s\n' "$1" >&2
  else printf '  \033[31mFAIL\033[0m %s: got %s, want %s\n' "$1" "$2" "$3" >&2; fails=$((fails + 1)); fi
}
verify() {
  say "D. read-only checks"
  local ROLES CORE FX MACH CAL SEN VIEWER NIN_SAC USDC_SAC DEP k id
  ROLES=$(get ROLES); CORE=$(get FUND_CORE); FX=$(get FX_POOL); MACH=$(get MACHINERY); CAL=$(get CROP_CALENDAR)
  SEN=$(get SENIOR); VIEWER=$(get FUND_VIEWER); NIN_SAC=$(get NIN_SAC); USDC_SAC=$(get USDC_SAC); DEP=$(get ACCOUNT_DEPLOYER)
  for k in ROLES LAND_TITLE FUND_VIEWER FUND_CORE FX_POOL MACHINERY CROP_CALENDAR SENIOR; do
    id=$(get $k)
    check "$k upgrade_delay 48 h" "$(view "$id" upgrade_delay | q)" 172800
    check "$k no pending upgrade" "$(view "$id" pending_upgrade)" null
    [ $k = FUND_CORE ] || check "$k admin" "$(view "$id" admin | q)" "$ADMIN"
    stellar contract fetch --id "$id" -o "$ROOT/.deploy/onchain.wasm" >/dev/null 2>&1
    check "$k on-chain wasm = recorded upload" "$(shasum -a 256 "$ROOT/.deploy/onchain.wasm" | cut -d' ' -f1)" "$(get WASM_$k)"
  done
  rm -f "$ROOT/.deploy/onchain.wasm"
  local cfg; cfg=$(view "$CORE" get_config)
  field() { python3 -c "import json,sys; print(json.loads(sys.argv[1])$2)" "$1"; }
  check "fund_core admin" "$(field "$cfg" '["admin"]')" "$ADMIN"
  check "fund_core.fx_pool" "$(field "$cfg" '["fx_pool"]')" "$FX"
  check "fund_core.machinery" "$(field "$cfg" '["machinery"]')" "$MACH"
  check "fund_core.senior" "$(field "$cfg" '["senior"]')" "$SEN"
  check "fx_pool.fund_core" "$(view "$FX" fund_core | q)" "$CORE"
  check "fx_pool.senior" "$(view "$FX" senior | q)" "$SEN"
  check "machinery.fund_core" "$(view "$MACH" fund_core | q)" "$CORE"
  check "fx_pool not paused" "$(view "$FX" is_paused)" false
  check "machinery not paused" "$(view "$MACH" is_paused)" false
  check "fx_pool max_scan ₹10,000" "$(view "$FX" max_scan | q)" "$((10000 * NIN))"
  check "fx_pool daily_cap" "$(view "$FX" daily_cap | q)" "$((${DAILY_CAP_USDC:-2500} * NIN))"
  check "fx_pool lp_fee 1 %" "$(view "$FX" lp_fee | q)" 100
  check "nIN SAC admin = fx_pool" "$(view "$NIN_SAC" admin | q)" "$FX"
  check "senior cap" "$(view "$SEN" cap | q)" "$((${SENIOR_CAP_USDC:-0} * NIN))"
  check "senior vault unset" "$(view "$SEN" vault)" null
  check "deployer.account_wasm" "$(view "$DEP" account_wasm | q)" "$(get ACCOUNT_WASM)"
  check "deployer.roles" "$(view "$DEP" roles | q)" "$ROLES"
  check "roles.is_union" "$(view "$ROLES" is_union --union "$UNION")" true
  local onchain_name onchain_geo
  onchain_name=$(curl -s "$HORIZON/accounts/$UNION/data/nila.name" | python3 -c 'import sys,json,base64;print(base64.b64decode(json.load(sys.stdin).get("value","")).decode())' 2>/dev/null || true)
  onchain_geo=$(curl -s "$HORIZON/accounts/$UNION/data/nila.geo" | python3 -c 'import sys,json,base64;print(base64.b64decode(json.load(sys.stdin).get("value","")).decode())' 2>/dev/null || true)
  check "union's nila.name on its account" "$onchain_name" "$UNION_NAME"
  check "union's nila.geo on its account" "$onchain_geo" "$UNION_GEO"
  check "roles.union_admission = hash of its details" "$(view "$ROLES" union_admission --union "$UNION" | q)" "$(details_hash)"
  local us; us=$(view "$ROLES" union_settings --union "$UNION")
  if [ -n "$us" ] && [ "$us" != null ]; then
    check "union settings: fx_pool" "$(field "$us" '["fx_pool"]')" "$FX"
    check "union settings: usdc" "$(field "$us" '["usdc"]')" "$USDC_SAC"
    check "union settings: session_contracts = [fund_core]" "$(field "$us" '["session_contracts"]')" "['$CORE']"
    check "union settings: sms_key" "$(field "$us" '["sms_key"]')" "$SMS_KEY"
  else
    check "union settings set (the union's own call)" "unset" "set"
  fi
  local l; for l in $LEADERS; do check "leader $l" "$(view "$ROLES" is_leader --union "$UNION" --account "$l")" true; done
  check "roles: oracle" "$(view "$ROLES" is_oracle --account "$ORACLE")" true
  check "roles: attester" "$(view "$ROLES" has_role --account "$ATTESTER" --role 1)" true
  check "machinery: tractor-acre value > 0" "$([ "$(view "$MACH" value_per_tractor_acre --union "$UNION" | q)" -gt 0 ] 2>/dev/null && echo yes || echo no)" yes
  local fam windows
  while read -r fam windows; do
    [ -z "$fam" ] || [ "${fam:0:1}" = "#" ] && continue
    check "calendar family $fam" "$(view "$CAL" windows --family "$fam" | python3 -c 'import sys,json;print(json.dumps([{"first":w["first"],"last":w["last"]} for w in json.load(sys.stdin)],separators=(",",":")))')" "$windows"
  done <"$CAL_WINDOWS"
  if [ -n "${ANCHOR_SWAP_KEY:-}" ]; then
    check "fx_pool.anchor" "$(view "$FX" anchor | q)" "$ANCHOR_ACCOUNT"
    check "fx_pool.swap_key" "$(view "$FX" swap_key | q)" "$(python3 -c 'import base64,sys;print(base64.b32decode(sys.argv[1])[1:33].hex())' "$ANCHOR_SWAP_KEY")"
  fi
  local iss; iss=$(curl -s "$HORIZON/accounts/$(get NIN_ASSET | cut -d: -f2)")
  check "issuer flags: required, revocable, clawback" "$(python3 -c 'import sys,json;f=json.load(sys.stdin)["flags"];print(f["auth_required"],f["auth_revocable"],f["auth_clawback_enabled"])' <<<"$iss")" "True True True"
  check "issuer home_domain" "$(python3 -c 'import sys,json;print(json.load(sys.stdin).get("home_domain",""))' <<<"$iss")" "$HOME_DOMAIN"
  if [ "$(get DONE_C10)" = 1 ]; then
    check "issuer locked: master weight 0, no other signer" "$(python3 -c 'import sys,json;s=json.load(sys.stdin)["signers"];print(sum(x["weight"] for x in s))' <<<"$iss")" 0
  else
    say "  (issuer not locked yet: C10 waits for counsel gate X14)"
  fi
  [ "$fails" = 0 ] && say "D: all checks pass" || say "D: $fails check(s) FAILED"
}

env_blocks() {
  cat >&2 <<EOF

──── nila-sensing node env (deploy/mainnet/node.env.example) ────
STELLAR_NETWORK=$NET
STELLAR_ROLES=$(get ROLES)
STELLAR_LAND_TITLE=$(get LAND_TITLE)
STELLAR_FUND_CORE=$(get FUND_CORE)
STELLAR_FX_POOL=$(get FX_POOL)
STELLAR_MACHINERY=$(get MACHINERY)
STELLAR_CROP_CALENDAR=$(get CROP_CALENDAR)
STELLAR_SENIOR=$(get SENIOR)
STELLAR_ACCOUNT_DEPLOYER=$(get ACCOUNT_DEPLOYER)
STELLAR_NIN_ASSET=$(get NIN_ASSET)
STELLAR_USDC_ASSET=$USDC_ASSET
STELLAR_DEFAULT_UNION=$UNION

──── nila-anchor mainnet.env (contracts block) ────
ANCHOR_FX_POOL=$(get FX_POOL)
ANCHOR_LAND_TITLE=$(get LAND_TITLE)
ANCHOR_ROLES=$(get ROLES)
ANCHOR_ACCOUNT_DEPLOYER=$(get ACCOUNT_DEPLOYER)
ANCHOR_UNION=$UNION
ANCHOR_UNIONS_FROM_LEDGER=$(get ROLES_LEDGER)
EOF
  [ -s "$UNION_CALLS" ] && say "the union's own calls are in $UNION_CALLS (run by its signers, UNION_SIGNER=<identity>)"
  return 0
}

case "$MODE" in
  run) run_all; verify; env_blocks ;;
  verify) verify; env_blocks ;;
  lock-issuer) lock_issuer; verify ;;
  *) die "unknown mode $MODE (run | verify | lock-issuer)" ;;
esac
[ "$fails" = 0 ]
