#!/bin/sh
# WWW-20 interactive hold: put ONE pattern on the panel for a fixed window while
# Calum looks at it, then restore stock. Usage:
#   run-hold.sh <seconds> <rowpair|halves|interleaved> [zones|fiducials|geometry]
# No `set -e`, no `set -u`, no `date +%s%N`.

SECS="${1:-180}"
PACK="${2:-rowpair}"
MODE="$3"
LOG=/tmp/paperclip-hold.log
PROBE_PID=""
SAMPLER_PID=""
RESTORED=0
SCENEFLAG=""
case "$MODE" in
    fiducials) SCENEFLAG="--hold-fiducials" ;;
    zones)     SCENEFLAG="--hold-zones" ;;
esac

ts() { date '+%H:%M:%S'; }
say() { echo "[$(ts)] $*" >> "$LOG"; }

restore() {
    [ "$RESTORED" = "1" ] && return
    RESTORED=1
    say "=== restore begins ==="
    [ -n "$PROBE_PID" ] && kill "$PROBE_PID" 2>/dev/null
    sleep 2
    [ -n "$SAMPLER_PID" ] && kill "$SAMPLER_PID" 2>/dev/null
    [ -n "$EV2_PID" ] && kill "$EV2_PID" 2>/dev/null
    [ -n "$EV3_PID" ] && kill "$EV3_PID" 2>/dev/null
    systemctl is-active --quiet xochitl || systemctl start xochitl >> "$LOG" 2>&1
    i=0
    while [ $i -lt 25 ]; do systemctl is-active --quiet xochitl && break; sleep 1; i=$((i+1)); done
    say "post: xochitl=$(systemctl is-active xochitl) pid=$(systemctl show -p MainPID --value xochitl)"
    say "post: system=$(systemctl is-system-running 2>&1) failed=[$(systemctl --failed --no-legend | tr '\n' ' ')]"
    say "post: /home=$(mountpoint -q /home && echo mounted || echo NOT-MOUNTED) notebooks=$(ls /home/root/.local/share/remarkable/xochitl 2>/dev/null | wc -l)"
    say "post: conn=$(cat /sys/class/drm/card0-LVDS-1/enabled)"
    say "DONE"; echo DONE > /tmp/paperclip-hold.done
    say "=== restore complete ==="
}
trap restore EXIT INT TERM HUP

: > "$LOG"; rm -f /tmp/paperclip-hold.done
say "hold starting: ${SECS}s packing=$PACK mode=${MODE:-geometry}"
say "pre: xochitl=$(systemctl is-active xochitl) uptime=$(cut -d. -f1 /proc/uptime)s charger=$(cat /sys/class/power_supply/max77963-charger/online 2>/dev/null)"

# Last-resort guardian: stock comes back even if this script is killed outright.
setsid sh -c "sleep $((SECS + 90)); systemctl is-active --quiet xochitl || systemctl start xochitl" >/dev/null 2>&1 &
say "guardian armed ($((SECS + 90))s)"

VCOMD=""
for d in /sys/class/regulator/regulator.*; do
    [ "$(cat $d/name 2>/dev/null)" = "VCOM" ] && VCOMD="$d"
done

# Record both evdev nodes for the whole window: free Gate 2 raw material if he
# happens to touch the panel, and the only capture path during calibration.
: > /tmp/paperclip-ev2.bin
: > /tmp/paperclip-ev3.bin
cat /dev/input/event2 > /tmp/paperclip-ev2.bin & EV2_PID=$!
cat /dev/input/event3 > /tmp/paperclip-ev3.bin & EV3_PID=$!

say "stopping xochitl"
systemctl stop xochitl >> "$LOG" 2>&1
sleep 1

say "starting probe: --hold $SECS --pack $PACK $SCENEFLAG"
/tmp/panelprobe --watchdog $((SECS + 90)) --hold "$SECS" --pack "$PACK" $SCENEFLAG >> "$LOG" 2>&1 &
PROBE_PID=$!

( while :; do
    echo "[$(ts)] conn=$(cat /sys/class/drm/card0-LVDS-1/enabled 2>/dev/null) vcom=$(cat $VCOMD/state 2>/dev/null)/$(cat $VCOMD/num_users 2>/dev/null) probe=$(kill -0 $PROBE_PID 2>/dev/null && echo alive || echo gone)" >> /tmp/paperclip-hold-samples.log
    sleep 5
  done ) &
SAMPLER_PID=$!

sleep 5
say "5s in: probe alive=$(kill -0 $PROBE_PID 2>/dev/null && echo yes || echo NO) conn=$(cat /sys/class/drm/card0-LVDS-1/enabled) vcom=$(cat $VCOMD/state)/$(cat $VCOMD/num_users)"

wait "$PROBE_PID"
say "probe exited rc=$?"
restore
exit 0
