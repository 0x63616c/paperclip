# Recovery

**Status: nothing in this document has been exercised on the tablet.** Stage 3
wrote the device primitives and Stage 4 wrote the supervisor that sequences
them; neither has taken the display on hardware. Stage 4's half is demonstrated
against real systemd in an aarch64 Linux VM
(`docs/device/www-4-failure-harness.md`), which proves the Linux enforcement
and nothing about Xochitl. This file records the policy, so the work starts
against a written standard rather than inventing one under pressure.

## Hard device rules

Not advice. Each was established by evidence and each has already cost
something.

- **Never let `xochitl.service` fail.** Only ever stop it cleanly. Its
  effective `OnFailure=` is `emergency.target remarkable-fail.service`, and
  **`remarkable-fail.service` does not exist on this image** — so a crashed
  Xochitl, or four failed starts within 600 s (`StartLimitBurst=4`), drops the
  device into `systemd-sulogin-shell` on a serial console. That is a tablet
  that looks dead and needs a power cycle. See ADR-0008.
- **Any script that stops Xochitl must restore it on every exit path**,
  including errors. Guard against the busybox `date +%s%N` class of failure
  that left stock down unintentionally during WWW-1: no `set -e`, no `set -u`,
  no `%N`.
- **A takeover session must hold `/sys/power/wake_lock`** and release it on
  every exit path. Without it a mid-session suspend resumes into a *second*
  Xochitl contending for the panel. Correctness, not optimisation.
  `paper_device::WakeLock` releases in `Drop`, so an unwinding host still
  lets the tablet sleep again.
- **Clear the panel before releasing the display.** WWW-20 photographed what
  skipping it looks like: bands and hairlines laid over a stock UI that
  Xochitl's own repaint does not remove. `Panel::clear` does this. Note that
  it runs on the ordinary exit path only — **a crashed adapter still leaves
  residue**.
- **Presentation goes through the vendor waveform engine**, never raw DRM
  (ADR-0007).
- **Paperclip installs nothing on the root filesystem.** Binaries under
  `/home/root/paperclip`, units runtime-only in `/run/systemd/system`
  (ADR-0008).
- **Never write to `/data`** — device identity state — or to any read-only
  filesystem.

## Standing constraints

These apply to every run in the Paperclip project, not only to recovery:

- **Never enable Developer Mode.** It can erase data. If integration appears to
  require it, stop and raise it as a separate decision.
- **Never downgrade firmware.** Same rule, same escalation.
- The tablet is production hardware. Inspection is read-only until a verified
  backup exists.
- Any destructive or state-changing device operation needs Calum's explicit
  approval first.
- No operation may delete notebooks, credentials, or user data.

## Requirement

Stock reMarkable functionality — Xochitl — must remain usable and recoverable
at all times. This is a release condition, not an aspiration. A Paperclip
session that cannot hand the display back is a failed session regardless of
what it rendered.

## Stop conditions

Stop, mark the affected issue `blocked`, and escalate rather than proceeding,
when any of these is true:

- Stock Xochitl cannot be reliably restored after a custom session takes the
  display **on the device**. The VM says the sequencing is right; it says
  nothing about Xochitl.
- A hardware gate a later stage depends on came back **could-not-determine**.
  An unestablished gate is not a passed gate.
- Progress appears to require Developer Mode or a firmware downgrade.
- A verified backup does not exist and Stage 3 or later is about to take the
  display.
- Sleep/lock handoff cannot be made reliable on the target firmware. Per §5
  that means v1 is not ready for unattended normal use — report it rather than
  shipping around it.


## The states, and the deadlines on them

Stage 4 (`platform/host`). `Stock`, `Switching`, `Home`, `App`, `Recovering`,
`Failed`. One foreground owner at a time, every transition on a deadline, and
every transition completed by an observation rather than by a call returning.

**Starting is not arriving.** A switch to Home or an app completes only when
the process has reported ready *and* display ownership has been observed to
move. A switch to stock completes only when Xochitl is active *and* the panel
has been released. Either half alone leaves the machine in `Switching` until
the deadline takes it to `Recovering`.

**`Failed` is terminal, and so is a spent failure budget.** Nothing the machine
observes leaves either. Only `paperctl stock --force` does, because it is a
human saying so.

| Budget | Default | Why that number |
|---|---|---|
| Switch | 8 s | A session can start straight out of a resume, and every resume costs ~77 ms of kernel bridge reprogramming (WWW-20). Long enough for a cold start plus several suspends; short enough that nobody decides the tablet is dead. |
| Graceful stop | 5 s | Enough for a save-and-return-to-stock write (§5). |
| Force stop | 5 s | A `SIGKILL` sweep with no cgroup freezer has to re-read `cgroup.procs` several times. This is the budget for that loop, not for one signal. |
| Restore | 30 s before the supervisor calls it late | The restore *itself* is `platform/device`'s: two guarded starts with a 25 s wait each, then refuse. Xochitl unlocks dm-crypt and mounts `/home` on start (WWW-1 §5), so it is genuinely slow, and under-budgeting would declare a working restore failed. The supervisor asks **once** — the retry lives one layer down, and stacking them would be four starts against `StartLimitBurst=4`. |
| Supervisor watchdog | 80 s | Derived, not chosen: longer than a whole restore, because the supervisor performs one synchronously and a shorter watchdog kills it mid-recovery. See ADR-0012. |
| Progress stall | 6 s | About three refreshes. Below that a slow waveform reads as a hang. |

### Liveness is not responsiveness

A session publishes a counter it can only advance from the loop that draws.
The supervisor reads two independent facts: whether the process exists, and
whether the counter moved. A deadlocked app passes the first and fails the
second, and the second is what decides the hang row.

None of it is trusted. A session that wants to lie can write the file from a
timer — so the force-stop path does not consult the witness at all. A lying
session delays only its own termination.

## What happens when something breaks

Each row demonstrated in the VM harness; see the report for what was observed.

| Failure | What happens |
|---|---|
| App or Home crashes | Diagnostics captured, the rest of the session's process tree terminated, the panel released, stock restored. |
| App hangs | The progress deadline fires, `SIGTERM`, then a bounded `SIGKILL` sweep of the whole tree, then restore. |
| A child outlives its parent | The sweep reads the session's cgroup, not the parent-pid tree, so an orphan reparented to pid 1 is still found. |
| The display process dies | The session no longer holds the ownership registry; the dependent session is terminated rather than left holding nothing, and stock is restored. |
| The supervisor dies | systemd's `OnFailure=` starts `paperclip-restore-stock.service`, in a different process. This survives `SIGKILL`, which no destructor, signal handler or shell trap does — which is why it is the guarantee and `Drop` is only a convenience. |
| SSH disconnects | Everything was started by systemd and lives in `paperclip.slice`, not in a login scope. The terminal going away changes nothing. |
| Failures repeat | Three inside two minutes and the supervisor stops relaunching: it returns to stock, records the diagnosis, and *refuses the next request*. Recording exhaustion while still honouring requests is a restart loop with extra steps. |
| Xochitl will not start | `Failed`, explicitly. Logs preserved under the diagnostics directory, `paperctl stock` exits non-zero with `stock Xochitl was NOT restored`, and nothing claims a recovery happened. The wakelock is deliberately **kept**, so the tablet stays awake and reachable over SSH instead of suspending into a state nobody can diagnose. |
| Stock is near its start limit | The supervisor refuses to take the display at all, rather than starting a session it may not be able to hand back. WWW-3 measured the cost on the tablet: a Xochitl restart it did not survive cleanly consumed two of its four permitted starts. The refusal is counted as a session that failed to start, so repeated refusals spend the failure budget and stop. |
| Reboot | Stock. Units live only in `/run/systemd/system`, which is a tmpfs; nothing is enabled and nothing is installed on the root filesystem (ADR-0008). |

## The independent path

`paperctl stock` is the recovery entry point, and it is independent of:

- the supervisor — it runs in its own process, started by systemd's
  `OnFailure=`, so a supervisor killed with `SIGKILL` still triggers it;
- Home, the App Store, and every Paperclip socket — there is no host socket at
  all, by design; control reaches the supervisor through a file;
- the network and the Mac — it lives on the device. Running it over SSH is a
  *second* way in, not the only one.

It shares a library with the supervisor, and with `platform/device`'s `Stock`
and `StartBudget` — sharing the budget is the point, because systemd's start
limiter counts across processes and two private counters would defeat each
other. The VM harness proves the independence by killing the supervisor with
`SIGKILL` first.

## If you are holding a tablet that is behaving strangely

1. **Do not power-cycle it first.** If it is awake, SSH still works, and the
   logs you need are in `/run` and will not survive a boot.
2. `ssh root@<tablet>` then `/home/root/paperclip/bin/paperctl stock`. Success
   prints one line. Failure prints what went wrong and where the diagnostics
   are, and exits non-zero.
3. If that fails: `systemctl status xochitl.service` and
   `journalctl -u xochitl.service -n 200`. If it has hit its start limit,
   `systemctl reset-failed xochitl.service` then `systemctl start
   xochitl.service` — **`reset-failed`, never `restart`.** If two guarded
   starts have already failed, stop and power-cycle: past that point another
   attempt is likelier to strand the tablet than rescue it.
4. `cat /run/paperclip/diagnostics/last-failure.txt` — always the latest
   failure, at a path that can be read out over the phone.
5. A reboot is safe and returns the device to stock, because nothing of
   Paperclip's survives one. It costs you `/run`, so collect step 4 first.
6. Scrub before sharing. The journal and the diagnostics bundle can contain
   device tokens and Wi-Fi details; neither belongs on an issue or in this
   repository.

## What exists today

Two things, and neither is a working handoff.

The home screen draws a **Return to stock** tile. It is inert. It is there
because the shelf is the Stage 1 deliverable and the handoff is the shape the
shelf has to accommodate, not because any handoff works. See `assumptions.md`,
A7.

`platform/device` supplies the *primitives* a handoff needs, all tested on the
Mac and none exercised on the tablet:

| Primitive | What it does | Proven |
|---|---|---|
| `WakeLock` | takes and releases the kernel wakelock, on every exit path | on scratch files, not on `/sys/power` |
| `DisplayLocks` | reads the vendor advisory locks and says who holds the panel | against the exact bytes the tablet wrote |
| `Panel::clear` | drives the panel white with a settled waveform | on `MemoryPanel` only |
| `VendorPanel` | the vendor waveform engine behind a C ABI | opened and presented on the tablet; **nothing has seen the glass** |
| `device-report` | a read-only readiness report to run on the tablet | run on the tablet; every prediction held |
| `Stock` | stops and restores Xochitl, never kills it, one guarded retry | against a fake systemd, not against systemd |
| `StartBudget` | refuses a session near `StartLimitBurst` | on scratch files |
| `Takeover` | owns the acquire/release order, restores on every path | ordering, drop, **panic**, each failure branch, **and three round trips on hardware** |
| `DetachedWatchdog` | a `setsid` guardian that outlives `SIGKILL` | the mechanism, on aarch64 Linux; not its script |

`platform/host` supplies the *sequencing*, and unlike the table above it has
been run against a real service manager — in a VM, on a stand-in for Xochitl:

| Piece | What it does | Proven |
|---|---|---|
| `Machine` | the §10 state machine, deadlines and all | 32 tests on the Mac, and every row in the VM |
| `UnitSet` | generates the runtime units from a probe of what the platform enforces | asserted against the recorded device profile; applied and read back in the VM |
| `IsolationReport` | says what is enforced, what is partial, and what is merely accepted | `docs/isolation.md` |
| `SessionTree` | empties a session's cgroup, sweeping until it stays empty | orphans and a session that ignores `SIGTERM`, in the VM |
| `StockRecovery` | the independent restore, over `platform/device`'s `Stock` | with the supervisor `SIGKILL`ed first |
| `ProgressWatch` | main-loop progress, distinct from liveness | a live, signal-responsive process classified as hung |

## What is still owed here

- The verified backup procedure, and how "verified" is checked. (A backup from
  WWW-1 exists on the Mac; the *procedure* is not written down here.)
- ~~How the display is taken, and how it is given back — the sequencing.~~
  Written: `paper_device::Takeover` owns the order and ADR-0011 explains why it
  is that order. Still never run on the tablet.
- What happens on crash, on sleep, and on lock — the save-and-return-to-stock
  behaviour §5 chose over seamless resumption. WWW-21 owns the sleep and lock
  half.
- ~~The manual recovery path, written for someone holding a tablet that is not
  responding as expected.~~ Written above. Never needed in anger, which is the
  only test that counts.
- Evidence. Every row in the table above that says "never run".

Until those are written from evidence on the device, treat recovery as
unproven. A passing test in this repository is not device qualification, and a
process that starts is not a screen that works.
