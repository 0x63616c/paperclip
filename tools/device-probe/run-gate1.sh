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
# WWW-20 Gate 1 driver. Runs on the tablet under busybox ash.
#
# Deliberately avoids `set -u` and `date +%s%N`: busybox date has no %N, and the
# combination is what left stock stopped for two extra minutes during WWW-1.
# `set -e` is also NOT used, because an early exit must not skip the restore.

LOG=/tmp/paperclip-gate1.log
PROBE=/tmp/panelprobe
SAMPLER_PID=""
EV2_PID=""
EV3_PID=""
RESTORED=0

ts() { date '+%H:%M:%S'; }
say() { echo "[$(ts)] $*" >> "$LOG"; }

restore() {
    [ "$RESTORED" = "1" ] && return
    RESTORED=1
    say "=== restore begins ==="
    for p in $SAMPLER_PID $EV2_PID $EV3_PID; do
        kill "$p" 2>/dev/null
    done
    if ! systemctl is-active --quiet xochitl; then
        say "starting xochitl"
        systemctl start xochitl >> "$LOG" 2>&1
        say "systemctl start xochitl rc=$?"
    else
        say "xochitl already active"
    fi
    i=0
    while [ $i -lt 20 ]; do
        systemctl is-active --quiet xochitl && break
        sleep 1
        i=$((i + 1))
    done
    say "post-restore: xochitl=$(systemctl is-active xochitl) pid=$(systemctl show -p MainPID --value xochitl)"
    say "post-restore: system=$(systemctl is-system-running 2>&1) failed=[$(systemctl --failed --no-legend | tr '\n' ' ')]"
    say "post-restore: /home mountpoint=$(mountpoint -q /home && echo yes || echo NO)"
    say "post-restore: notebooks=$(ls /home/root/.local/share/remarkable/xochitl 2>/dev/null | wc -l)"
    say "post-restore: connector=$(cat /sys/class/drm/card0-LVDS-1/enabled 2>/dev/null)"
    say "post-restore: epframebuffer.lock=[$(tr '\n' ' ' < /tmp/epframebuffer.lock 2>/dev/null)]"
    say "=== restore complete ==="
}
trap restore EXIT INT TERM HUP

: > "$LOG"
say "gate1 run starting on $(uname -n), image $(cat /etc/version)"
say "pre: xochitl=$(systemctl is-active xochitl) pid=$(systemctl show -p MainPID --value xochitl)"
say "pre: connector=$(cat /sys/class/drm/card0-LVDS-1/enabled) status=$(cat /sys/class/drm/card0-LVDS-1/status)"
say "pre: battery=$(cat /sys/class/power_supply/max77818-battery/capacity 2>/dev/null)"
say "pre: fpga=$(cat /sys/class/fpga_manager/fpga0/state)"

# Resolve the EPD rails by name so the sampler can report them meaningfully.
RAILS=""
for d in /sys/class/regulator/regulator.*; do
    n=$(cat "$d/name" 2>/dev/null)
    case "$n" in
        VCOM|VPOS1|VPOS2|VPOS3|VNEG1|VNEG2|VNEG3|VGH1|VGH2|VGL|VPDD|G2194_PS|FPGA_PWR_EN)
            RAILS="$RAILS $n:$d" ;;
    esac
done
say "pre-rails:$(for r in $RAILS; do n=${r%%:*}; d=${r#*:}; printf ' %s=%s/%s' "$n" "$(cat $d/state 2>/dev/null)" "$(cat $d/num_users 2>/dev/null)"; done)"

# Background sampler: rails + connector + dpms, once a second, while the probe runs.
(
    while :; do
        line="[$(ts)] sample conn=$(cat /sys/class/drm/card0-LVDS-1/enabled 2>/dev/null)/$(cat /sys/class/drm/card0-LVDS-1/dpms 2>/dev/null)"
        for r in $RAILS; do
            n=${r%%:*}; d=${r#*:}
            line="$line $n=$(cat $d/state 2>/dev/null)/$(cat $d/num_users 2>/dev/null)"
        done
        echo "$line" >> /tmp/paperclip-gate1-samples.log
        sleep 1
    done
) &
SAMPLER_PID=$!

# Bank any touches Calum happens to make during the test (Gate 2 raw material).
: > /tmp/paperclip-ev2.bin
: > /tmp/paperclip-ev3.bin
cat /dev/input/event2 > /tmp/paperclip-ev2.bin &
EV2_PID=$!
cat /dev/input/event3 > /tmp/paperclip-ev3.bin &
EV3_PID=$!

say "stopping xochitl"
systemctl stop xochitl >> "$LOG" 2>&1
say "systemctl stop xochitl rc=$? active=$(systemctl is-active xochitl)"
sleep 1
say "locks after stop: epd=[$(ls -l /tmp/epd.lock 2>/dev/null)] fbl=[$(tr '\n' ' ' < /tmp/epframebuffer.lock 2>/dev/null)]"

say "--- probe output follows ---"
"$PROBE" --watchdog 240 --scene-seconds 12 --fiducials >> "$LOG" 2>&1
say "--- probe exited rc=$? ---"

restore
exit 0
