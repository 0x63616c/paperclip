#!/bin/sh
# SUPERSEDED -- DO NOT RUN.
#
# This driver presents through raw DRM/KMS. That path is now prohibited: the
# 405x1084 -> 1620x2160 transport packing is proprietary and has never been
# publicly reverse-engineered, and presentation goes through the vendor
# waveform engine instead (ADR-0007).
#
# It also predates lib-stock.sh, so it does NOT hold a wakelock for the
# takeover and does NOT check xochitl's start budget before stopping stock.
# Running it risks a suspend resuming into a second xochitl, and repeated runs
# risk tripping StartLimitBurst=4/10min -- which fails the unit, fires an
# OnFailure naming a service that does not exist, and drops the tablet to an
# emergency shell.
#
# Kept only as the record of a refuted hypothesis. Use run-calib.sh as the
# pattern for any new driver.
# WWW-20 Gate 3: what happens to a custom display session across suspend/resume.
# Runs detached on the tablet so that losing the control channel cannot abort it.
# No `set -e`, no `set -u`, no `date +%N`.

LOG=/tmp/paperclip-gate3.log
PROBE_PID=""
SAMPLER_PID=""
RESTORED=0

ts() { date '+%H:%M:%S'; }
say() { echo "[$(ts)] $*" >> "$LOG"; }

restore() {
    [ "$RESTORED" = "1" ] && return
    RESTORED=1
    say "=== restore begins ==="
    [ -n "$PROBE_PID" ] && kill "$PROBE_PID" 2>/dev/null
    sleep 2
    [ -n "$SAMPLER_PID" ] && kill "$SAMPLER_PID" 2>/dev/null
    echo 0 > /sys/class/rtc/rtc0/wakealarm 2>/dev/null
    systemctl is-active --quiet xochitl || systemctl start xochitl >> "$LOG" 2>&1
    i=0
    while [ $i -lt 25 ]; do systemctl is-active --quiet xochitl && break; sleep 1; i=$((i+1)); done
    say "post: xochitl=$(systemctl is-active xochitl) pid=$(systemctl show -p MainPID --value xochitl)"
    say "post: system=$(systemctl is-system-running 2>&1) failed=[$(systemctl --failed --no-legend | tr '\n' ' ')]"
    say "post: /home=$(mountpoint -q /home && echo mounted || echo NOT-MOUNTED) notebooks=$(ls /home/root/.local/share/remarkable/xochitl 2>/dev/null | wc -l)"
    say "post: dm=[$(ls /dev/mapper | tr '\n' ' ')]"
    say "post: conn=$(cat /sys/class/drm/card0-LVDS-1/enabled)"
    say "DONE" ; echo DONE > /tmp/paperclip-gate3.done
    say "=== restore complete ==="
}
trap restore EXIT INT TERM HUP

: > "$LOG"; rm -f /tmp/paperclip-gate3.done
say "gate3 starting, uptime=$(cut -d. -f1 /proc/uptime)s"
say "pre: suspend_success=$(cat /sys/power/suspend_stats/success) fail=$(cat /sys/power/suspend_stats/fail)"
say "pre: xochitl=$(systemctl is-active xochitl)"

# Independent last-resort guardian: whatever happens below, stock comes back.
setsid sh -c 'sleep 200; systemctl is-active --quiet xochitl || systemctl start xochitl' >/dev/null 2>&1 &
say "guardian armed (200s)"

VCOMD=""
for d in /sys/class/regulator/regulator.*; do
    [ "$(cat $d/name 2>/dev/null)" = "VCOM" ] && VCOMD="$d"
done
( while :; do
    echo "[$(ts)] g3 conn=$(cat /sys/class/drm/card0-LVDS-1/enabled 2>/dev/null) vcom=$(cat $VCOMD/state 2>/dev/null)/$(cat $VCOMD/num_users 2>/dev/null) probe=$(kill -0 $PROBE_PID 2>/dev/null && echo alive || echo gone)" >> /tmp/paperclip-gate3-samples.log
    sleep 1
  done ) &
SAMPLER_PID=$!

say "stopping xochitl"
systemctl stop xochitl >> "$LOG" 2>&1
sleep 1

say "starting probe in --hold mode (90s, watchdog 150s)"
/tmp/panelprobe --watchdog 150 --hold 90 >> "$LOG" 2>&1 &
PROBE_PID=$!
sleep 6
say "probe pid=$PROBE_PID alive=$(kill -0 $PROBE_PID 2>/dev/null && echo yes || echo NO) conn=$(cat /sys/class/drm/card0-LVDS-1/enabled) vcom=$(cat $VCOMD/state)/$(cat $VCOMD/num_users)"

NOW=$(date +%s)
WAKE=$((NOW + 30))
echo 0 > /sys/class/rtc/rtc0/wakealarm
echo "$WAKE" > /sys/class/rtc/rtc0/wakealarm
say "wakealarm set: now=$NOW wake=$WAKE readback=[$(cat /sys/class/rtc/rtc0/wakealarm)]"
say "rtc time=[$(cat /sys/class/rtc/rtc0/since_epoch 2>/dev/null)]"

say ">>> entering suspend (echo mem > /sys/power/state) with custom session holding the display"
echo mem > /sys/power/state 2>>"$LOG"
RC=$?
say "<<< resumed, echo rc=$RC, wall=$(date '+%H:%M:%S')"
say "post-resume: suspend_success=$(cat /sys/power/suspend_stats/success) fail=$(cat /sys/power/suspend_stats/fail)"
say "post-resume: probe alive=$(kill -0 $PROBE_PID 2>/dev/null && echo yes || echo NO)"
say "post-resume: conn=$(cat /sys/class/drm/card0-LVDS-1/enabled) vcom=$(cat $VCOMD/state)/$(cat $VCOMD/num_users)"
say "post-resume: /home=$(mountpoint -q /home && echo mounted || echo NOT-MOUNTED)"
say "post-resume: dmesg tail:"
dmesg | grep -E "PM: suspend|Triggering wakeup|autosleep|imx|drm|cumulus" | tail -n 20 >> "$LOG" 2>&1

sleep 20
say "20s after resume: probe alive=$(kill -0 $PROBE_PID 2>/dev/null && echo yes || echo NO) conn=$(cat /sys/class/drm/card0-LVDS-1/enabled) vcom=$(cat $VCOMD/state)/$(cat $VCOMD/num_users)"

restore
exit 0
