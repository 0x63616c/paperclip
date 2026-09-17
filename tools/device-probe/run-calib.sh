#!/bin/sh
# WWW-20 Gate 2: capture taps against physical screen landmarks.
#
# Needs no rendering, so it takes no display risk: xochitl is stopped only so
# that stray taps cannot open or edit Calum's notebooks, and because e-ink
# retains its last image he still has the screen as a visual reference.
#
# Resolves input nodes BY NAME, never by event-node number (WWW-20 / ADR-0007).
# No `set -e`, no `set -u`, no `date +%s%N`.

SECS="${1:-180}"
LOG=/tmp/paperclip-calib.log
PEN_PID=""
TOUCH_PID=""
RESTORED=0

ts() { date '+%H:%M:%S'; }
say() { echo "[$(ts)] $*" >> "$LOG"; }

restore() {
    [ "$RESTORED" = "1" ] && return
    RESTORED=1
    say "=== restore begins ==="
    [ -n "$PEN_PID" ] && kill "$PEN_PID" 2>/dev/null
    [ -n "$TOUCH_PID" ] && kill "$TOUCH_PID" 2>/dev/null
    systemctl is-active --quiet xochitl || systemctl start xochitl >> "$LOG" 2>&1
    i=0
    while [ $i -lt 25 ]; do systemctl is-active --quiet xochitl && break; sleep 1; i=$((i+1)); done
    say "post: xochitl=$(systemctl is-active xochitl) pid=$(systemctl show -p MainPID --value xochitl)"
    say "post: system=$(systemctl is-system-running 2>&1) failed=[$(systemctl --failed --no-legend | tr '\n' ' ')]"
    say "post: /home=$(mountpoint -q /home && echo mounted || echo NOT-MOUNTED) notebooks=$(ls /home/root/.local/share/remarkable/xochitl 2>/dev/null | wc -l)"
    say "post: pen=$(wc -c < /tmp/paperclip-calib-pen.bin 2>/dev/null)B touch=$(wc -c < /tmp/paperclip-calib-touch.bin 2>/dev/null)B"
    say "DONE"; echo DONE > /tmp/paperclip-calib.done
    say "=== restore complete ==="
}
trap restore EXIT INT TERM HUP

: > "$LOG"; rm -f /tmp/paperclip-calib.done

# Resolve by name, not by number.
PEN_NODE=""
TOUCH_NODE=""
for d in /sys/class/input/event*; do
    n=$(cat "$d/device/name" 2>/dev/null)
    node="/dev/input/$(basename $d)"
    case "$n" in
        *"marker input"*) PEN_NODE="$node" ;;
        *"touch input"*)  TOUCH_NODE="$node" ;;
    esac
done
say "resolved pen=$PEN_NODE touch=$TOUCH_NODE (by EVIOCGNAME-equivalent sysfs name)"
if [ -z "$PEN_NODE" ] || [ -z "$TOUCH_NODE" ]; then
    say "FATAL: could not resolve input nodes by name"
    exit 1
fi

say "calibration capture starting, ${SECS}s"
say "pre: xochitl=$(systemctl is-active xochitl) uptime=$(cut -d. -f1 /proc/uptime)s"

setsid sh -c "sleep $((SECS + 90)); systemctl is-active --quiet xochitl || systemctl start xochitl" >/dev/null 2>&1 &
say "guardian armed ($((SECS + 90))s)"

say "stopping xochitl so stray taps cannot touch notebooks"
systemctl stop xochitl >> "$LOG" 2>&1
sleep 1

: > /tmp/paperclip-calib-pen.bin
: > /tmp/paperclip-calib-touch.bin
cat "$PEN_NODE"   > /tmp/paperclip-calib-pen.bin   & PEN_PID=$!
cat "$TOUCH_NODE" > /tmp/paperclip-calib-touch.bin & TOUCH_PID=$!
say "recording both nodes for ${SECS}s"

i=0
while [ $i -lt "$SECS" ]; do
    sleep 10
    i=$((i + 10))
    say "t+${i}s pen=$(wc -c < /tmp/paperclip-calib-pen.bin)B touch=$(wc -c < /tmp/paperclip-calib-touch.bin)B"
done

restore
exit 0
