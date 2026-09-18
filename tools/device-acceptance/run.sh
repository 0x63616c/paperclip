#!/bin/sh
# The §17 acceptance run: a full platform-upgrade cycle against the real
# tablet, scripted so a person does not have to interpret it (WWW-41).
#
# It proves three things, in order, and restores stock on every exit path:
#
#   healthy   — a real upgrade, start to finish, on hardware.
#   corrupted — a damaged package is refused and changes nothing.
#   failing   — a candidate that never reaches `ready` is rolled back.
#
# Each step asserts on `paperctl upgrade status`, not on a command exiting
# zero — the same rule `tests/failure-harness` follows, applied to the one
# machine a VM cannot stand in for.
#
# Before running this against the real tablet: load the `remarkable-device-
# session` skill. This script stops and starts `xochitl.service` through
# `paperctl upgrade run`/`paperctl stock`, which is exactly the takeover this
# repository's standing rule says never to improvise around. It also spends
# the tablet's own start-budget ledger (WWW-41 precondition 1) and the
# separate, smaller allowance of Xochitl restarts this project has been
# rationing by hand — see the project description before running this
# unattended for the first time.
#
# Usage:
#   tools/device-acceptance/run.sh \
#       --key ~/paperclip-keys/paperclip.key \
#       --trust /home/root/paperclip/keys/paperclip.pub \
#       --from 0.3.1 --to 0.4.0 --fail-to 0.4.0-acceptance-fail \
#       [--device remarkable-wifi] [--workdir /tmp/paperclip-acceptance]
#
# `--from` is asserted, not assumed: the run refuses to start unless the
# tablet is already on exactly that version, because every later assertion is
# a diff against a known starting point.
#
# Idempotent: every exit path — a precondition refusal, a step failure, or a
# clean finish — restores stock and reports `paperctl upgrade status`. Running
# it again after a failure is always safe; running it again after a pass
# requires a fresh `--to`, because §12 published versions are immutable.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
workdir=/tmp/paperclip-acceptance
key=
trust=
from=
to=
fail_to=
device=

while [ $# -gt 0 ]; do
    case "$1" in
        --key) key=$2; shift 2 ;;
        --trust) trust=$2; shift 2 ;;
        --from) from=$2; shift 2 ;;
        --to) to=$2; shift 2 ;;
        --fail-to) fail_to=$2; shift 2 ;;
        --device) device=$2; shift 2 ;;
        --workdir) workdir=$2; shift 2 ;;
        *) echo "run: unknown argument $1" >&2; exit 2 ;;
    esac
done

for name_value in "key:$key" "trust:$trust" "from:$from" "to:$to" "fail-to:$fail_to"; do
    name=${name_value%%:*}
    value=${name_value#*:}
    [ -n "$value" ] || { echo "run: --$name is required" >&2; exit 2; }
done

paperctl="cargo run --quiet --manifest-path $here/tools/paperctl/Cargo.toml --"

log() { printf '%s  %s\n' "$(date -u +%H:%M:%S)" "$1"; }
fail() { log "REFUSED  $1"; exit 1; }

# --- preconditions -----------------------------------------------------

resolve_host() {
    if [ -n "$device" ]; then
        printf '%s' "$device"
        return
    fi
    $paperctl devices --output json | python3 -c '
import json, sys
found = json.load(sys.stdin)
reachable = [d for d in found if d.get("reachable")]
if not reachable:
    sys.exit(1)
print(reachable[0]["host"])
' || fail "no reachable tablet; run \`paperctl devices\` to see what auto-discovery found"
}

ssh_host=$(resolve_host)
# Every later `paperctl` call passes `--device "$ssh_host"` explicitly,
# pinned to this one resolution: re-resolving per call would let a second
# device that becomes reachable mid-run silently take over from the one every
# earlier assertion was made against.
log "device   $ssh_host"

ssh_raw() {
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$ssh_host" "$@"
}

# Precondition 1: the tablet is reachable at all. USB de-enumerates in deep
# sleep and the interface disappears from the host entirely (WWW-41) — this
# is the check that tells that apart from a tablet that is merely asleep.
ssh_raw true 2>/dev/null || fail "cannot reach $ssh_host over SSH; if this is USB, wake the tablet first"

# Precondition 2: paperctl's own start-budget ledger. `systemctl reset-failed`
# does not clear it; only time does, so refusing here is the only safe move —
# proceeding and hitting the same budget mid-upgrade is how a tablet is left
# mid-transaction with no more starts to finish restoring stock.
recent_starts=$(ssh_raw '
    now=$(date +%s)
    f=/tmp/paperclip-xochitl-starts
    [ -f "$f" ] || { echo 0; exit; }
    awk -v now="$now" "now - \$1 < 600" "$f" | wc -l
')
[ "$recent_starts" -lt 3 ] || fail "xochitl start budget: $recent_starts starts in the last 600s (limit 3); wait for the window to clear"
log "start budget  $recent_starts/3 in the last 600s"

# Precondition 3: nothing already holds the display. A stray `paperctl`
# process from a prior session that never released `/dev/dri/card0` would
# make this run's own takeover ambiguous about who is holding what.
holder=$(ssh_raw 'fuser /dev/dri/card0 2>/dev/null' || true)
[ -z "$holder" ] || fail "/dev/dri/card0 is held by pid(s) $holder; clear that session before running this"
log "display        free"

# Precondition 4: the tablet is where this run's assertions assume it is.
# Every step below diffs against `--from`; starting from anywhere else would
# make a pass meaningless and a failure ambiguous.
current=$($paperctl upgrade status --device "$ssh_host" | awk '/^current/{print $2}')
[ "$current" = "$from" ] || fail "tablet is on $current, not --from $from; update --from or roll the tablet back first"
log "current        $current"

mkdir -p "$workdir"
trap 'log "restoring stock"; $paperctl stock --device "$ssh_host" >/dev/null 2>&1 || true; log "final status"; $paperctl upgrade status --device "$ssh_host" || true' EXIT

# --- building ------------------------------------------------------------

build_release() {
    version=$1
    host_binary=$2
    ladder=$3
    dest="$workdir/build-$version"
    rm -rf "$dest"
    mkdir -p "$dest/bin"
    cp "$here/target/aarch64-unknown-linux-gnu/release/$host_binary" "$dest/bin/paperclip-host"
    for name in home app-store settings; do
        cp "$here/target/aarch64-unknown-linux-gnu/release/$name" "$dest/bin/$name"
    done
    include=""
    if [ -n "$ladder" ]; then
        printf '%s\n' "$ladder" > "$dest/ladder"
        include="--include ladder"
    fi
    # shellcheck disable=SC2086
    $paperctl upgrade package \
        --source "$dest" --version "$version" $include \
        --key "$key" --out "$workdir/paperclip-$version.tar.gz"
}

log "building $to"
(cd "$here" && cargo build --release --target aarch64-unknown-linux-gnu \
    -p paper-host -p paper-home -p paper-app-store -p paper-settings -p paper-fault-app)
build_release "$to" paperclip-host ""

# --- step 1: healthy -------------------------------------------------------

log "step 1/3: healthy upgrade $from -> $to"
$paperctl upgrade run "$workdir/paperclip-$to.tar.gz" --trust "$trust" --device "$ssh_host"
current=$($paperctl upgrade status --device "$ssh_host" | awk '/^current/{print $2}')
last=$($paperctl upgrade status --device "$ssh_host" | awk '/^last/{$1=""; print}')
[ "$current" = "$to" ] || fail "step 1: current is $current after a healthy upgrade, expected $to"
case "$last" in *commit*) ;; *) fail "step 1: journal did not reach commit: $last" ;; esac
log "PASS     step 1: current $current, $last"

# --- step 2: corrupted package ---------------------------------------------

log "step 2/3: a damaged package must be refused and change nothing"
corrupt="$workdir/paperclip-$to-corrupt.tar.gz"
cp "$workdir/paperclip-$to.tar.gz" "$corrupt"
# Flip one byte past the header, inside the compressed body — anywhere in a
# gzip stream invalidates the digest the manifest signed.
python3 -c "
import sys
path = sys.argv[1]
with open(path, 'r+b') as f:
    f.seek(-64, 2)
    b = f.read(1)
    f.seek(-64, 2)
    f.write(bytes([b[0] ^ 0xff]))
" "$corrupt"
if $paperctl upgrade run "$corrupt" --trust "$trust" --device "$ssh_host"; then
    fail "step 2: a damaged package was accepted"
fi
current=$($paperctl upgrade status --device "$ssh_host" | awk '/^current/{print $2}')
[ "$current" = "$to" ] || fail "step 2: current moved to $current after a refused package; must stay $to"
log "PASS     step 2: refused, current still $current"

# --- step 3: a build that never reaches ready -------------------------------

log "building $fail_to (paper-fault-app standing in for the host, scripted to panic)"
build_release "$fail_to" paper-fault-app panic

log "step 3/3: a candidate that panics on start must be rolled back"
# Unlike step 2, a candidate that starts and is then found unhealthy is not a
# CLI error — `paperctl upgrade run` exits 0 having rolled it back and
# reports the refusal in its own output, the same shape
# `upgrade-panics`/`upgrade-never-ready` assert against in the VM harness.
report=$($paperctl upgrade run "$workdir/paperclip-$fail_to.tar.gz" --trust "$trust" --device "$ssh_host")
case "$report" in *refused*) ;; *) fail "step 3: the report never said the candidate was refused: $report" ;; esac
current=$($paperctl upgrade status --device "$ssh_host" | awk '/^current/{print $2}')
[ "$current" = "$to" ] || fail "step 3: current is $current after a rollback; expected $to"
log "PASS     step 3: refused ($report), current still $current"

log "ACCEPTANCE PASS: $from -> $to proven healthy; a corrupted package and a failing build both left the tablet on $to"
