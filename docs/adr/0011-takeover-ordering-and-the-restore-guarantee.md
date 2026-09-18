# ADR-0011 — Takeover ordering, and what actually guarantees the restore

**Status:** accepted, **amended after a hardware incident** (Stage 3, WWW-3),
**amended again to close a duplication** (Stage 2, WWW-49).

> **Amendment, 2026-09-17.** The release order below originally ended at
> "restore stock", and that was wrong in a way that hurt: it left our PID in
> `/tmp/epframebuffer.lock`, Xochitl read it on restart, and aborted twice.
> `NRestarts=2` of a permitted 4. The table now has a step 2 that puts the
> vendor locks back **before** stock is started, and the "what guarantees the
> restore" section has been rewritten — the original three layers did not.

> **Amendment, WWW-49.** "One type owns the order" was the decision; it was
> not, until now, what the code did. Six ordering-critical steps — the health
> gate, the caller's own preflight, the lock capture and the wakelock —
> lived in the caller, written out three times (twice in `hold.rs`, once in
> `examples/takeover.rs`), untested, because they read through a concrete
> `Systemctl` rather than the `ServiceControl` seam the type's own tests use.
> `Takeover::acquire` now performs the whole sequence itself; a caller
> supplies closures for *what*, never *when*. The table below gains row 0 —
> the settle gate that refuses before anything is touched — promoted to first
> position for the same reason lock restoration below is "layer 0": no later
> row substitutes for it.

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

One type, `paper_device::Takeover`, owns the order — the whole of it, inside
`Takeover::acquire` and `Takeover::release`, not split across the type and
whichever caller remembered the preamble:

| | Acquire | Release |
|---|---|---|
| 0 | **the settle gate**: refuse before anything is touched unless stock is healthy and has restart budget to spare | |
| 1 | the caller's own preflight (e.g. refuse to present a blank frame) | clear the panel |
| 2 | record the vendor lock state | **put the vendor locks back** |
| 3 | take the wakelock | wait until the panel is genuinely free |
| 4 | arm the watchdog | restore stock |
| 5 | check the start budget, then stop stock cleanly | release the wakelock |
| 6 | | disarm the watchdog |

Row 0 is a private function, `wait_until_safe_to_stop`, that only
`Takeover::acquire` may call — `grep -rn 'wait_until_safe_to_stop'` finds no
call outside `platform/device/src/takeover.rs`. It reads a `StockHealth`
snapshot the caller supplies as a closure (a plain read with no side effect on
the device, so unlike every other row its *position* relative to the rest
carries no safety argument of its own — only the gate's position does) and
refuses before the lock capture, the wakelock or the stop, exactly the
ordering `hold.rs`'s two sessions and `examples/takeover.rs` used to
duplicate by hand.

The watchdog is armed **first** among the mutating steps, before anything can go wrong, so even a failure
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

## The lock step, and why it is where it is

A takeover claims `/tmp/epframebuffer.lock` by writing its own PID — correctly,
since DRM master alone is not display ownership here. Releasing without putting
that file back produced this, at 04:05 on 2026-09-17:

```text
04:05:57  Started reMarkable main application.
04:05:58  another instance is already running
04:05:58  Failed to initialize SWTCON.
04:05:58  Main process exited, code=dumped, status=6/ABRT
04:05:58  Scheduled restart job, restart counter is at 1.
          [second abort, then a third start that succeeded]
```

Calum was asked for his passcode. `NRestarts=2` against a `StartLimitBurst` of
4 with an `OnFailure=` that leads to a serial-console emergency shell — two
more and the tablet looks dead to its owner.

**It cannot be fixed by waiting for our process to exit.** The release path
*is* our process, and it has to start Xochitl before it can exit, so the PID in
that file is necessarily live at the moment Xochitl reads it. The file has to
be put back, and put back before the start.

`VendorLockState` records the registry's contents at preflight and restores
exactly that — the original bytes if it existed, removal if it did not. Then
the release polls, bounded, until the registry reads what it read before, and
only then starts stock. If it never gets there, stock is started anyway — a
tablet with no Xochitl is worse than one that had to retry — but the release
returns an error rather than reporting success.

`/tmp/epd.lock` is a zero-length `flock` target owned by root; its presence is
normal and the lock is the descriptor, not the file. Its state is recorded and
a disagreement is reported, but it is not deleted: removing a root-owned file
stock expects would be worse than leaving it.

## What guarantees the restore

Four layers. The original three did not, which is the correction this ADR
carries:

0. **Putting the vendor locks back, before stock starts.** Listed first
   because it is the one whose absence caused a real incident, and because no
   later layer substitutes for it — the watchdog would have started a Xochitl
   that aborted just the same.

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

## Verifying the restore, not just performing it

`StockHealth` gained `n_restarts` and `main_start`, because the check that
passed during the incident was not wrong so much as blind: Xochitl ended
`active` with `systemctl --failed` empty, since the *third* start worked.
`is_healthy()` still returns true for that state — deliberately, and there is a
test asserting it does — so the comparison now reads systemd's `NRestarts` and
treats any increase across a session as a regression.

There is also a new precondition. `StartBudget` counts starts *we* asked for;
it could not have prevented this, because these were systemd's own `Restart=`
retries. `StockHealth::allows_takeover()` refuses to begin within two restarts
of the limit, reading systemd's counter rather than ours.

## What is proven

On the Mac, against a fake `ServiceControl` that records every systemd call in
order — including a service that captures what the registry held at the moment
`start` was called, so the *ordering* of the lock restore is asserted rather
than assumed. Both lock tests were confirmed to fail with the restore removed: the acquire and release sequences; restore on drop; **restore after a
panic**; a spent budget refusing before stock is touched; a failed stop leaving
stock running and releasing the wakelock itself rather than handing it back;
exactly one guarded retry; a hopeless restore reporting failure, releasing the
wakelock anyway, and leaving the watchdog armed; a double release doing one
start; and a session that finds stock already down not claiming a restore it
did not cause.

WWW-49 added five: an unhealthy stock, and one two restarts from the limit,
each refusing before the lock capture or the wakelock are touched and before
anything reaches the `ServiceControl` fake at all; a failed caller preflight
refusing the same way; a failed wakelock acquisition leaving the fake
untouched; and one end-to-end test asserting the full order — health,
preflight, locks, wakelock, arm, stop — as a single sequence, which is the
property the module doc's table claims and the property row 0 above exists
for. 16 tests total, up from 11.

## What is not

Whether `systemctl` on the tablet behaves as its manual says, whether the
guardian script is right, and everything about the panel. No part of this has
run on the device. A green test here is not device qualification.

## What would make this wrong

- If `reset-failed` turns out not to clear the start rate limiter on this
  systemd version, the budget becomes the only defence and should be lowered.
- If the panel clear proves slow enough to matter, step 1 of the release
  competes with the watchdog deadline, and the budget must account for it.
