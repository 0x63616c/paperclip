#!/bin/sh
# WWW-20: does a userspace wakelock actually inhibit autosleep on this device?
#
# WWW-3 is about to depend on this for correctness -- without a held wakelock a
# mid-session suspend resumes into a second xochitl instance. The mechanism was
# shown to exist; this measures whether it WORKS.
#
# Touches neither the display nor xochitl. Only meaningful off charge, because
# the charger holds `udev.charger` and nothing suspends while plugged in.

STOCK_LOG=/tmp/paperclip-wakelock-test.log
export STOCK_LOG
LIB_DIR=$(dirname "$0")
if [ -f "$LIB_DIR/lib-stock.sh" ]; then . "$LIB_DIR/lib-stock.sh"; else . /tmp/lib-stock.sh; fi

PHASE_SECS="${1:-90}"
: > "$STOCK_LOG"; rm -f /tmp/paperclip-wakelock-test.done

cleanup() { stock_wakelock_release; echo DONE > /tmp/paperclip-wakelock-test.done; }
trap cleanup EXIT INT TERM HUP

stock_say "charger online=$(cat /sys/class/power_supply/max77963-charger/online 2>/dev/null) status=$(cat /sys/class/power_supply/max77963-charger/status 2>/dev/null)"
stock_say "wake_lock at start=[$(cat /sys/power/wake_lock)]"
stock_say "autosleep=[$(cat /sys/power/autosleep 2>/dev/null)]"

s0=$(cat /sys/power/suspend_stats/success)
stock_say "PHASE A (no wakelock): baseline suspend_success=$s0, waiting ${PHASE_SECS}s"
i=0
while [ $i -lt "$PHASE_SECS" ]; do
    sleep 15; i=$((i + 15))
    stock_say "  A t+${i}s success=$(cat /sys/power/suspend_stats/success) lock=[$(cat /sys/power/wake_lock)]"
done
s1=$(cat /sys/power/suspend_stats/success)
stock_say "PHASE A result: $s0 -> $s1, delta=$((s1 - s0)) suspends in ${PHASE_SECS}s WITHOUT a wakelock"

stock_wakelock_acquire
s2=$(cat /sys/power/suspend_stats/success)
stock_say "PHASE B (wakelock held): baseline suspend_success=$s2, waiting ${PHASE_SECS}s"
i=0
while [ $i -lt "$PHASE_SECS" ]; do
    sleep 15; i=$((i + 15))
    stock_say "  B t+${i}s success=$(cat /sys/power/suspend_stats/success) lock=[$(cat /sys/power/wake_lock)]"
done
s3=$(cat /sys/power/suspend_stats/success)
stock_say "PHASE B result: $s2 -> $s3, delta=$((s3 - s2)) suspends in ${PHASE_SECS}s WITH a wakelock held"

stock_wakelock_release
stock_say "verdict: without=$((s1 - s0)) with=$((s3 - s2))"
stock_say "xochitl untouched: $(systemctl is-active xochitl)"
cleanup
exit 0
