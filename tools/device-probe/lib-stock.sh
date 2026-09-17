# Shared safety helpers for any script that takes the display from stock.
# Source this; do not execute it.
#
# Two hazards this exists to prevent, both learned the hard way on WWW-20:
#
# 1. xochitl.service has StartLimitBurst=4 over StartLimitIntervalUSec=10min,
#    and OnFailure=emergency.target remarkable-fail.service. On this image
#    remarkable-fail.service DOES NOT EXIST, and emergency.target runs
#    systemd-sulogin-shell. So a start refused by the rate limiter fails the
#    unit, fires OnFailure, and drops the tablet to a serial emergency shell:
#    it looks dead and needs a power cycle. Any script that restarts stock more
#    than a few times in ten minutes can brick the session this way.
#
#    Mitigation: reset-failed immediately before every start, which clears both
#    the failed state and the start rate-limit counter, plus a local record of
#    recent starts so a script can refuse to pile on.
#
# 2. A takeover session must hold a kernel wakelock. Without one, a mid-session
#    suspend resumes into a SECOND xochitl instance contending for the panel.
#    Correctness, not optimisation. Note the charger holds `udev.charger` for
#    you while plugged in, which masks the bug -- do not rely on it.
#
#    A leaked wakelock is its own hazard: it survives the script, blocks suspend
#    until the next reboot, and drains the battery invisibly. Every caller must
#    release it from a trap, and its guardian should release it too.

STOCK_WAKELOCK="paperclip-takeover"
STOCK_START_LOG=/tmp/paperclip-xochitl-starts
STOCK_WAKELOCK_HELD=0

stock_say() { echo "[$(date '+%H:%M:%S')] $*" >> "${STOCK_LOG:-/dev/null}"; }

# --- wakelock ---------------------------------------------------------------

stock_wakelock_acquire() {
    if [ -w /sys/power/wake_lock ]; then
        echo "$STOCK_WAKELOCK" > /sys/power/wake_lock 2>/dev/null
        if grep -q "$STOCK_WAKELOCK" /sys/power/wake_lock 2>/dev/null; then
            STOCK_WAKELOCK_HELD=1
            stock_say "wakelock acquired: $STOCK_WAKELOCK (held=[$(cat /sys/power/wake_lock)])"
            return 0
        fi
    fi
    stock_say "WARNING: could not acquire wakelock -- a suspend now would resume into a second xochitl"
    return 1
}

stock_wakelock_release() {
    [ "$STOCK_WAKELOCK_HELD" = "1" ] || return 0
    echo "$STOCK_WAKELOCK" > /sys/power/wake_unlock 2>/dev/null
    STOCK_WAKELOCK_HELD=0
    stock_say "wakelock released (remaining=[$(cat /sys/power/wake_lock 2>/dev/null)])"
}

# --- start budget -----------------------------------------------------------

# How many times have we started xochitl in the last 10 minutes?
stock_recent_starts() {
    now=$(date +%s)
    count=0
    [ -f "$STOCK_START_LOG" ] || { echo 0; return; }
    while read -r t; do
        case "$t" in ''|*[!0-9]*) continue ;; esac
        [ $((now - t)) -lt 600 ] && count=$((count + 1))
    done < "$STOCK_START_LOG"
    echo "$count"
}

# Call BEFORE stopping stock. Returns non-zero if this session should not run.
stock_check_start_budget() {
    recent=$(stock_recent_starts)
    stock_say "start budget: $recent start(s) recorded in the last 600s (limit 4)"
    if [ "$recent" -ge 3 ]; then
        stock_say "REFUSING: too close to xochitl's StartLimitBurst=4/10min."
        stock_say "A refused start fails the unit and drops the tablet to an emergency shell."
        return 1
    fi
    return 0
}

# --- stop and restore -------------------------------------------------------

stock_stop() {
    stock_say "stopping xochitl (clean stop only -- never kill it)"
    systemctl stop xochitl >> "${STOCK_LOG:-/dev/null}" 2>&1
    stock_say "stopped: active=$(systemctl is-active xochitl) result=$(systemctl show xochitl -p Result --value)"
}

# Restore stock. Safe to call repeatedly; used from trap handlers.
stock_restore() {
    if systemctl is-active --quiet xochitl; then
        stock_say "restore: xochitl already active"
    else
        # Clear any failed state AND the start rate-limit counter before the
        # start, so the start can never be the one that trips the limiter.
        systemctl reset-failed xochitl.service 2>/dev/null
        date +%s >> "$STOCK_START_LOG"
        stock_say "restore: reset-failed done, starting xochitl"
        systemctl start xochitl >> "${STOCK_LOG:-/dev/null}" 2>&1
        stock_say "restore: start rc=$?"
    fi

    i=0
    while [ $i -lt 25 ]; do
        systemctl is-active --quiet xochitl && break
        sleep 1
        i=$((i + 1))
    done

    if ! systemctl is-active --quiet xochitl; then
        # One careful retry, never a loop: hammering is what trips the limiter.
        stock_say "restore: still not active after 25s -- one guarded retry"
        systemctl reset-failed xochitl.service 2>/dev/null
        date +%s >> "$STOCK_START_LOG"
        systemctl start xochitl >> "${STOCK_LOG:-/dev/null}" 2>&1
        sleep 5
    fi

    stock_wakelock_release

    stock_say "post: xochitl=$(systemctl is-active xochitl) pid=$(systemctl show -p MainPID --value xochitl) result=$(systemctl show xochitl -p Result --value)"
    stock_say "post: system=$(systemctl is-system-running 2>&1) failed=[$(systemctl --failed --no-legend | tr '\n' ' ')]"
    stock_say "post: /home=$(mountpoint -q /home && echo mounted || echo NOT-MOUNTED) notebooks=$(ls /home/root/.local/share/remarkable/xochitl 2>/dev/null | wc -l)"

    # Leave the unit's counters clean for whoever runs next.
    systemctl reset-failed xochitl.service 2>/dev/null
}
