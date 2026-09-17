# ADR-0011 — Takeover ordering, and what actually guarantees the restore

**Status:** accepted for the shape, **never run on the tablet** (Stage 3, WWW-3).

## Context

§6 and §10 require that stock Xochitl is switched out as a full-screen session
and back in, and that what happens when the handoff fails partway is proven
rather than assumed. Everything needed to do it now exists as separate pieces —
a wakelock, advisory locks, a panel that can clear itself — and nothing owned
the order they go in.

Order is the whole of the safety argument here, and it is not obvious. Three
facts make it unforgiving:

- `xochitl.service` has `StartLimitBurst=4` over ten minutes and an effective
  `OnFailure=emergency.target remarkable-fail.service`, and **that unit does not
  exist on this image**. A start refused by the rate limiter *fails the unit*,
  fires `OnFailure`, and runs `systemd-sulogin-shell` on a serial console. The
  tablet then looks dead and needs a power cycle (ADR-0008).
- The tablet autosleeps on a kernel schedule that cannot be refused except by
  holding `/sys/power/wake_lock`. A suspend while stock is stopped resumes into
  a *second* Xochitl contending for the panel.
- That same autosleep takes SSH with it. A session driven from the Mac can be
  orphaned mid-takeover with stock still down, through nobody's mistake.

## Decision

One type, `paper_device::Takeover`, owns the order:

| | Acquire | Release |
|---|---|---|
| 1 | arm the watchdog | clear the panel |
| 2 | check the start budget | restore stock |
| 3 | take the wakelock | release the wakelock |
| 4 | stop stock cleanly | disarm the watchdog |

The watchdog is armed **first**, before anything can go wrong, so even a failure
inside the stop is covered by something outside this process. The budget is
checked before the stop, so a refusal leaves the tablet exactly as found. The
wakelock is taken before the stop, because the window where stock is down and
nothing inhibits suspend is the window that produces two Xochitls.

The watchdog is disarmed **only if the restore succeeded**. If it did not, the
watchdog is the remaining hope and leaving it armed is the point of it.

### Release order: a discrepancy, named rather than silently resolved

The `remarkable-device-session` skill lists releasing the wakelock *before*
starting Xochitl. `tools/device-probe/lib-stock.sh` starts Xochitl first and
releases after. **This follows the script.** A suspend is harmless once stock is
running and harmful while it is not, so "stock back, then stop inhibiting sleep"
is the order that is never wrong. The skill should be corrected, not this.

### The start budget is three, not four

`START_BUDGET = 3` against `StartLimitBurst = 4`. The spare slot is not caution
for its own sake: the restore this session is about to need, plus its one
guarded retry, must both fit. Running down to the limit and *then* failing to
restore is the exact sequence that strands the tablet.

### One guarded retry, never a loop

Every start is preceded by `reset-failed`, which clears the rate-limit counter
as well as the failed state, so a Paperclip start can never itself be the one
that trips the limiter. After two starts that do not bring stock up, `restore`
gives up and says so, recommending a power cycle — which always returns the
tablet to stock (ADR-0008). Further attempts past that point are more likely to
strand the device than to rescue it, and a human holding it has options this
process does not.

## What guarantees the restore

Three layers, because no one of them covers everything:

1. **`Drop`** — the ordinary path, and a panic unwind. A test panics inside a
   held session and asserts stock comes back, because a bug in screen code must
   not strand the tablet.
2. **An interrupt flag** polled by the session loop, so `SIGINT`/`SIGTERM` return
   *through* `Drop` rather than past it.
3. **A detached `setsid` guardian**, the only layer that survives `SIGKILL`, a
   segfault, or SSH going away. Verified on aarch64 Linux: a guardian completed
   its work after its parent was `SIGKILL`ed. The *mechanism* is proven; the
   `systemctl` script it runs is not, because that needs the tablet.

`Drop` cannot report a failed restore, so `release()` exists to be called
explicitly and the destructor is the net, not the plan.

## What is proven

On the Mac, against a fake `ServiceControl` that records every systemd call in
order: the acquire and release sequences; restore on drop; **restore after a
panic**; a spent budget refusing before stock is touched; a failed stop leaving
stock running and handing the wakelock back to the caller; exactly one guarded
retry; a hopeless restore reporting failure, releasing the wakelock anyway, and
leaving the watchdog armed; a double release doing one start; and a session that
finds stock already down not claiming a restore it did not cause.

## What is not

Whether `systemctl` on the tablet behaves as its manual says, whether the
guardian script is right, and everything about the panel. No part of this has
run on the device. A green test here is not device qualification.

## What would make this wrong

- If `reset-failed` turns out not to clear the start rate limiter on this
  systemd version, the budget becomes the only defence and should be lowered.
- If the panel clear proves slow enough to matter, step 1 of the release
  competes with the watchdog deadline, and the budget must account for it.
