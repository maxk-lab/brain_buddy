#!/usr/bin/env bash
# Fly cutover / rollback / erase between the Python and Rust backends (027 R10).
#
# Usage: fly_cutover.sh cutover|rollback|erase
#
# Writer authority moves one way at a time: the running side is stopped and
# VERIFIED stopped (`flyctl machines list`) before the other side starts.
# Evidence lines carry identifiers and timestamps only, never secrets or data.
#
# Environment (all optional unless noted):
#   FLYCTL            flyctl binary (default: flyctl)
#   OLD_APP           default brain-buddy-backend
#   NEW_APP           default brain-buddy-api
#   FRONTEND_APP      default brain-buddy-frontend
#   NEW_CONFIG        default fly.api.toml
#   FRONTEND_CONFIG   default fly.frontend.toml
#   FORK_VOLUMES      extra "app:volume-id" forks/copies to destroy (space separated)
#   OFFPLATFORM_COPIES  operator-named off-platform copies (comma separated)
#   OFFPLATFORM_CONFIRMED  must be 1 when copies are listed (operator removed them)
#   SMOKE_CMD         production smoke (required for cutover and for erase, which
#                     re-runs it to prove NEW_APP is serving before destroying data)
#   ERASE_OLD         1 = erase after a successful cutover smoke
#   EVIDENCE_FILE     append evidence lines here (default: stdout only)
#   VERIFY_ATTEMPTS / VERIFY_SLEEP   stop-verification polling (default 10 / 3)
set -euo pipefail

FLYCTL="${FLYCTL:-flyctl}"
OLD_APP="${OLD_APP:-brain-buddy-backend}"
NEW_APP="${NEW_APP:-brain-buddy-api}"
FRONTEND_APP="${FRONTEND_APP:-brain-buddy-frontend}"
NEW_CONFIG="${NEW_CONFIG:-fly.api.toml}"
FRONTEND_CONFIG="${FRONTEND_CONFIG:-fly.frontend.toml}"
FORK_VOLUMES="${FORK_VOLUMES:-}"
OFFPLATFORM_COPIES="${OFFPLATFORM_COPIES:-}"
OFFPLATFORM_CONFIRMED="${OFFPLATFORM_CONFIRMED:-0}"
ERASE_OLD="${ERASE_OLD:-0}"
VERIFY_ATTEMPTS="${VERIFY_ATTEMPTS:-10}"
VERIFY_SLEEP="${VERIFY_SLEEP:-3}"
ROLLBACK_ARMED=0

evidence() {
  local line
  line="$(date -u +%Y-%m-%dT%H:%M:%SZ) $*"
  echo "EVIDENCE $line"
  if [ -n "${EVIDENCE_FILE:-}" ]; then echo "$line" >>"$EVIDENCE_FILE"; fi
}

die() {
  echo "fly_cutover: $*" >&2
  exit 1
}

# Count machines in a running-ish state for an app.
running_count() {
  "$FLYCTL" machines list -a "$1" --json | python3 -c '
import json, sys
live = {"started", "starting", "replacing", "created"}
print(sum(1 for m in json.load(sys.stdin) if str(m.get("state", "")).lower() in live))
'
}

verify_stopped() {
  local app="$1" n i
  for ((i = 0; i < VERIFY_ATTEMPTS; i++)); do
    n="$(running_count "$app")"
    if [ "$n" = "0" ]; then
      evidence "verified-stopped app=$app"
      return 0
    fi
    sleep "$VERIFY_SLEEP"
  done
  die "$app still has running machines; refusing to start the other side"
}

verify_running() {
  local app="$1" n i
  for ((i = 0; i < VERIFY_ATTEMPTS; i++)); do
    n="$(running_count "$app")"
    if [ "$n" != "0" ]; then
      evidence "verified-running app=$app machines=$n"
      return 0
    fi
    sleep "$VERIFY_SLEEP"
  done
  die "$app has no running machines after start"
}

stop_app() {
  evidence "stop app=$1"
  "$FLYCTL" scale count 0 -a "$1" --yes
  verify_stopped "$1"
}

point_frontend() {
  local origin="https://$1.fly.dev"
  evidence "frontend-origin app=$FRONTEND_APP origin=$origin"
  "$FLYCTL" deploy -c "$FRONTEND_CONFIG" -a "$FRONTEND_APP" \
    --env "BACKEND_ORIGIN=$origin" --yes
}

# Ids of a `flyctl ... --json` list; a flyctl or parse failure fails the caller
# (pipefail), so assign it on its own line: ids="$(fly_ids ...)".
fly_ids() {
  "$FLYCTL" "$@" --json | python3 -c '
import json, sys
for r in json.load(sys.stdin):
    print(r.get("id", r.get("ID", "")))
'
}

# How many of the named app exist in `flyctl apps list`.
app_listed() {
  "$FLYCTL" apps list --json | python3 -c '
import json, sys
app = sys.argv[1]
print(sum(1 for a in json.load(sys.stdin) if a.get("Name", a.get("name")) == app))
' "$1"
}

# Rollback needs the old app and its data. After erase neither exists, and a
# "rollback" would stop the live new app and point the frontend at nothing.
require_rollback_target() {
  local listed vols
  listed="$(app_listed "$OLD_APP")"
  [ "$listed" != "0" ] || die "rollback unavailable: $OLD_APP is gone (erased); nothing was stopped"
  vols="$(fly_ids volumes list -a "$OLD_APP")"
  [ -n "$vols" ] || die "rollback unavailable: $OLD_APP has no volumes (erased); nothing was stopped"
}

do_rollback() {
  local frontend_failed=0
  require_rollback_target
  evidence "rollback-begin"
  stop_app "$NEW_APP"
  # The new side is verified stopped, so starting the old side is safe even if
  # the frontend cannot be repointed; report that after OLD_APP is up.
  point_frontend "$OLD_APP" || frontend_failed=1
  evidence "start app=$OLD_APP"
  "$FLYCTL" scale count 1 -a "$OLD_APP" --yes
  verify_running "$OLD_APP"
  [ "$frontend_failed" = "0" ] || die "$OLD_APP is running but the frontend still points at $NEW_APP"
  evidence "rollback-done"
}

# Snapshots still listed for a destroyed volume. flyctl may answer "not found"
# once the volume is gone, which is the observed absence; any other failure is
# not evidence of absence and aborts.
snapshots_left() { # volume-id app
  local out rc=0 err
  err="$(mktemp)"
  out="$("$FLYCTL" volumes snapshots list "$1" -a "$2" --json 2>"$err")" || rc=$?
  if [ "$rc" != "0" ] && grep -qiE 'not found|could not find' "$err"; then
    out="[]"
    rc=0
  fi
  rm -f "$err"
  [ "$rc" = "0" ] || die "cannot list snapshots of destroyed volume $1 (flyctl exit $rc)"
  python3 -c 'import json, sys; print(len(json.loads(sys.argv[1])))' "$out"
}

do_erase() {
  local destroyed="" id snap spec item vols snaps n left_vols=0 left_snaps=0
  if [ -n "$OFFPLATFORM_COPIES" ] && [ "$OFFPLATFORM_CONFIRMED" != "1" ]; then
    die "off-platform copies listed ($OFFPLATFORM_COPIES); remove them and set OFFPLATFORM_CONFIRMED=1"
  fi
  [ -n "${SMOKE_CMD:-}" ] || die "SMOKE_CMD is required for erase"
  evidence "erase-begin"
  # The old app holds the only other data copy: erase only while the new app is
  # observed running and passing the smoke.
  verify_running "$NEW_APP"
  bash -c "$SMOKE_CMD" || die "$NEW_APP is not serving (smoke failed); refusing to erase $OLD_APP"
  evidence "new-app-serving app=$NEW_APP"
  # The old app must not be writable while its data is destroyed.
  stop_app "$OLD_APP"

  # Every volume of the old app (the data volume and any in-app fork), then the
  # operator-named forks. Entries are "app:volume-id".
  vols="$(fly_ids volumes list -a "$OLD_APP")"
  for id in $vols; do destroyed="$destroyed $OLD_APP:$id"; done
  destroyed="$destroyed $FORK_VOLUMES"
  for spec in $destroyed; do
    snaps="$(fly_ids volumes snapshots list "${spec#*:}" -a "${spec%%:*}")"
    for snap in $snaps; do
      evidence "snapshot-listed volume=${spec#*:} snapshot=$snap"
    done
    evidence "destroy-volume app=${spec%%:*} volume=${spec#*:}"
    "$FLYCTL" volumes destroy "${spec#*:}" -a "${spec%%:*}" --yes
  done

  # Observe, do not assume: re-list the volumes of the old app and of each fork
  # app, and the snapshots of every destroyed id.
  vols="$(fly_ids volumes list -a "$OLD_APP")"
  left_vols=$(printf '%s\n' "$vols" | grep -c . || true)
  for spec in $FORK_VOLUMES; do
    vols="$(fly_ids volumes list -a "${spec%%:*}")"
    n="$(printf '%s\n' "$vols" | grep -cFx -- "${spec#*:}" || true)"
    left_vols=$((left_vols + n))
  done
  for spec in $destroyed; do
    n="$(snapshots_left "${spec#*:}" "${spec%%:*}")"
    left_snaps=$((left_snaps + n))
  done
  evidence "post-destroy-listing volumes-remaining=$left_vols snapshots-remaining=$left_snaps"
  [ "$left_vols" = "0" ] || die "$left_vols volume(s) still listed after destroy; keeping $OLD_APP"
  [ "$left_snaps" = "0" ] || die "$left_snaps snapshot(s) still present after destroy; keeping $OLD_APP"

  evidence "destroy-app app=$OLD_APP"
  "$FLYCTL" apps destroy "$OLD_APP" --yes
  n="$(app_listed "$OLD_APP")"
  [ "$n" = "0" ] || die "$OLD_APP still listed after destroy"

  if [ -n "$OFFPLATFORM_COPIES" ]; then
    local -a items
    IFS=',' read -ra items <<<"$OFFPLATFORM_COPIES"
    for item in "${items[@]}"; do evidence "offplatform-copy-confirmed-removed name=$item"; done
  fi
  evidence "final-listing app=$OLD_APP remaining=$n volumes-remaining=$left_vols snapshots-remaining=$left_snaps destroyed=${destroyed# }"
  evidence "erase-done"
}

# Once OLD_APP is stopped, any failure before the smoke passes must put it back:
# run the stop-first rollback from the EXIT trap (covers set -e aborts and die).
on_exit() {
  local rc=$?
  if [ "$ROLLBACK_ARMED" = "1" ] && [ "$rc" != "0" ]; then
    ROLLBACK_ARMED=0
    evidence "cutover-failed rc=$rc; rolling back to $OLD_APP"
    set +e
    (set -e; do_rollback)
    [ $? = 0 ] || echo "fly_cutover: ROLLBACK FAILED; check both apps by hand" >&2
  fi
  exit "$rc"
}

do_cutover() {
  [ -n "${SMOKE_CMD:-}" ] || die "SMOKE_CMD is required for cutover"
  trap on_exit EXIT
  trap 'exit 130' INT TERM
  evidence "cutover-begin"
  stop_app "$OLD_APP"
  ROLLBACK_ARMED=1
  evidence "deploy app=$NEW_APP"
  "$FLYCTL" deploy -c "$NEW_CONFIG" -a "$NEW_APP" --yes
  "$FLYCTL" scale count serve=1 worker=1 -a "$NEW_APP" --yes
  verify_running "$NEW_APP"
  point_frontend "$NEW_APP"
  evidence "smoke-begin"
  if bash -c "$SMOKE_CMD"; then
    evidence "smoke-passed"
    ROLLBACK_ARMED=0
  else
    evidence "smoke-failed"
    die "smoke failed; rolling back to $OLD_APP; set target = \"python\" in a follow-up commit"
  fi
  if [ "$ERASE_OLD" = "1" ]; then do_erase; fi
  evidence "cutover-done"
}

case "${1:-}" in
  cutover) do_cutover ;;
  rollback) do_rollback ;;
  erase) do_erase ;;
  *) die "usage: $0 cutover|rollback|erase" ;;
esac
