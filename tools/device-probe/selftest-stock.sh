#!/bin/sh
# Exercises lib-stock.sh WITHOUT stopping xochitl: proves the wakelock round
# trip and the start-budget accounting before either is trusted in a session
# that actually takes the display.
STOCK_LOG=/tmp/paperclip-selftest.log
: > "$STOCK_LOG"
. /tmp/lib-stock.sh

echo "--- wakelock round trip ---"
echo "before: [$(cat /sys/power/wake_lock)]"
stock_wakelock_acquire && echo "acquire: ok" || echo "acquire: FAILED"
echo "during: [$(cat /sys/power/wake_lock)]"
stock_wakelock_release
echo "after:  [$(cat /sys/power/wake_lock)]"

echo "--- start budget accounting ---"
rm -f /tmp/paperclip-xochitl-starts
echo "empty log -> recent=$(stock_recent_starts)"
stock_check_start_budget && echo "budget check: proceed" || echo "budget check: REFUSE"
now=$(date +%s)
for n in 0 1 2; do echo $((now - n * 10)) >> /tmp/paperclip-xochitl-starts; done
echo "3 recent starts -> recent=$(stock_recent_starts)"
stock_check_start_budget && echo "budget check: proceed" || echo "budget check: REFUSE (correct)"
: > /tmp/paperclip-xochitl-starts
echo $((now - 900)) >> /tmp/paperclip-xochitl-starts
echo "1 start 900s ago -> recent=$(stock_recent_starts) (should be 0, outside window)"
rm -f /tmp/paperclip-xochitl-starts

echo "--- xochitl untouched ---"
echo "xochitl=$(systemctl is-active xochitl) result=$(systemctl show xochitl -p Result --value)"
echo "--- log ---"
cat "$STOCK_LOG"
