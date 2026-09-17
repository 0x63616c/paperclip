#!/bin/sh
# WWW-20 Gate 2: capture taps against physical screen landmarks.
#   run-calib.sh <seconds>
#
# Needs no rendering, so it takes no display risk. xochitl is stopped only so
# that stray taps cannot open or edit Calum's notebooks; e-ink retention leaves
# the screen usable as a visual reference meanwhile.
#
# Safety comes from lib-stock.sh: wakelock held for the whole takeover, start
# budget checked before stopping stock, guarded restore on every exit path.
# No `set -e`, no `set -u`, no `date +%s%N`.

SECS="${1:-300}"
STOCK_LOG=/tmp/paperclip-calib.log
export STOCK_LOG
PEN_PID=""
TOUCH_PID=""
RESTORED=0

LIB_DIR=$(dirname "$0")
if [ -f "$LIB_DIR/lib-stock.sh" ]; then . "$LIB_DIR/lib-stock.sh"; else . /tmp/lib-stock.sh; fi

restore() {
    [ "$RESTORED" = "1" ] && return
    RESTORED=1
    stock_say "=== restore begins ==="
    [ -n "$PEN_PID" ] && kill "$PEN_PID" 2>/dev/null
    [ -n "$TOUCH_PID" ] && kill "$TOUCH_PID" 2>/dev/null
    stock_say "captured: pen=$(wc -c < /tmp/paperclip-calib-pen.bin 2>/dev/null)B touch=$(wc -c < /tmp/paperclip-calib-touch.bin 2>/dev/null)B"
    stock_restore
    echo DONE > /tmp/paperclip-calib.done
    stock_say "=== restore complete ==="
}
trap restore EXIT INT TERM HUP

: > "$STOCK_LOG"; rm -f /tmp/paperclip-calib.done
stock_say "calibration capture: ${SECS}s"

if ! stock_check_start_budget; then
    stock_say "ABORTED before touching stock -- try again in a few minutes"
    echo DONE > /tmp/paperclip-calib.done
    exit 2
fi

# Resolve by name, never by event-node number.
PEN_NODE=""; TOUCH_NODE=""
for d in /sys/class/input/event*; do
    n=$(cat "$d/device/name" 2>/dev/null)
    node="/dev/input/$(basename $d)"
    case "$n" in
        *"marker input"*) PEN_NODE="$node" ;;
        *"touch input"*)  TOUCH_NODE="$node" ;;
    esac
done
stock_say "resolved by name: pen=$PEN_NODE touch=$TOUCH_NODE"
if [ -z "$PEN_NODE" ] || [ -z "$TOUCH_NODE" ]; then
    stock_say "FATAL: could not resolve input nodes by name"
    exit 1
fi

# Wakelock BEFORE stopping stock, so there is no unprotected window.
stock_wakelock_acquire

# The guardian restores stock AND drops our wakelock: if this script is killed
# outright, a leaked wakelock would block suspend until the next reboot and
# quietly drain the battery.
setsid sh -c "sleep $((SECS + 120)); echo $STOCK_WAKELOCK > /sys/power/wake_unlock 2>/dev/null; systemctl is-active --quiet xochitl || { systemctl reset-failed xochitl.service; systemctl start xochitl; }" >/dev/null 2>&1 &
stock_say "guardian armed ($((SECS + 120))s)"

stock_stop
sleep 1

: > /tmp/paperclip-calib-pen.bin
: > /tmp/paperclip-calib-touch.bin
cat "$PEN_NODE"   > /tmp/paperclip-calib-pen.bin   & PEN_PID=$!
cat "$TOUCH_NODE" > /tmp/paperclip-calib-touch.bin & TOUCH_PID=$!
stock_say "recording both nodes for ${SECS}s"

i=0
while [ $i -lt "$SECS" ]; do
    sleep 15
    i=$((i + 15))
    stock_say "t+${i}s pen=$(wc -c < /tmp/paperclip-calib-pen.bin)B touch=$(wc -c < /tmp/paperclip-calib-touch.bin)B"
done

restore
exit 0
