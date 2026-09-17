# 0008 — The supervisor's shape, and where it is proven

Stage 4 (WWW-4), §10 and §11.

## Decision

Five things, and they hang together.

**1. A pure decision core and a thin Linux runtime.** `state`, `facilities`,
`units`, `report` and `progress` are platform-free. `linux::*` does signals,
cgroups, sysfs and systemd, and compiles only for `target_os = "linux"`.
Events go into the state machine and `Action`s come out; the runtime executes
them and nothing else.

**2. Units are generated from a probe, not from a wish list.** `Facilities`
describes what a machine actually enforces, and a directive is written only
when it does something there. The report has a verdict — `ineffective` — for
"accepted and does nothing", which is the only way to describe the Paper Pro's
memory controller honestly.

**3. Recovery does not live in the failing process.** `OnFailure=` on both the
supervisor unit and the session unit starts
`paperclip-restore-stock.service`, in a process that was not involved in the
failure. `paperctl stock` is its entry point and needs no host, no Home, no
socket and no network.

**4. `Failed` is terminal, and a spent failure budget refuses requests.**
Nothing the machine *observes* leaves either state; only `paperctl stock
--force` does.

**5. Stock control is not re-implemented.** `platform/device` (WWW-3) owns
what may be done to Xochitl — the start budget, `reset-failed` before every
start, one guarded retry, then refuse — and the supervisor calls it. The
supervisor's own restore budget is therefore **one**, not two.

**6. The proof is a binary run in a real VM, not a `#[test]`.**
`tests/failure-harness` is `paperclip-failure-harness`, run deliberately, as
root, on aarch64 Linux with systemd. It writes a report naming the machine.

## Why

**On the split.** "The §10 table is decided correctly" and "the §10 table is
enforced" are different claims, and this project's standing rule is that local
tests are never qualification. Separating them in the code makes them separate
in the evidence too: 32 Mac tests say the decisions are right, and 17 VM cases
say the machine does them. Neither can be quoted as the other.

**On probing.** `MemoryMax=` is accepted on the Paper Pro and enforces nothing,
because systemd is `hybrid` and the memory controller is not delegated
(WWW-11). A unit carrying it would document a protection that does not exist,
and the next person to read it would believe it. The probe also caught two
things nobody had written down: systemd on both the VM and the device is built
without the BPF framework, so `IPAddressDeny=` does nothing, and
`RestrictAddressFamilies=` is seccomp — so on the Paper Pro **there is no way
to deny an app the network at the unit level at all.** The network capability
is a record of a decision there, not a boundary, and `docs/isolation.md` says
so.

**On recovery living elsewhere.** §10 says not to rely on destructors, signal
handlers, shell traps or the crashed process. The prior art here does exactly
that — quill's `takeover.sh` restores Xochitl from a shell `trap ... EXIT INT
TERM`, which is right for a script you run by hand and wrong for a supervisor,
because `SIGKILL` runs no trap. The `host-crash` case kills the supervisor with
`SIGKILL` precisely so that whatever brings the display back is provably not
the supervisor.

**On not re-implementing stock control.** WWW-3 and WWW-4 ran in parallel and
both grew a way to bring Xochitl back. Keeping both would have been worse than
merely redundant: `StartBudget` is persisted precisely because systemd's rate
limiter counts *across processes*, so two private counters would each believe
they were inside `StartLimitBurst=4` while together exceeding it — and
exceeding it is the thing that puts the tablet on a serial console. One budget,
one policy, several callers. The same arithmetic sets the supervisor's restore
budget to one: `Stock::restore` already does two guarded starts, so a
host-level retry multiplies rather than adds, and two would be exactly four.

**On terminal meaning terminal.** The first version recorded that the failure
budget was spent and then honoured the next request anyway. The VM caught it:
`repeated-failures` failed with *"a request after the budget was honoured; that
is a restart loop"*. Recording exhaustion while still acting on requests is a
restart loop with extra steps, and only a test that asked for a fourth session
was ever going to notice.

**On the watchdog being derived rather than chosen.** The supervisor unit
carries `WatchdogSec=`, petted from the main loop and nowhere else — a
heartbeat on a spare thread would report health for a supervisor that had
stopped supervising, which is the same mistake §10 names for apps. The first
value was 20 s, and the VM showed what that costs: a restore blocks for two
guarded starts with a 25 s wait each, so systemd killed the supervisor *while
it was recovering*, fired `OnFailure=`, and the state machine never reached
`Failed` — no diagnosis recorded, status file left stale at `switching`. The
value is now computed from `paper_device::stock::RESTORE_TIMEOUT` so the two
cannot drift, and a test asserts the inequality. A wedged supervisor now takes
80 s to be noticed; during a restore the backstop was never the watchdog
anyway.

**On a binary rather than a test.** A `#[test]` would be run by `cargo test
--workspace` on a Mac, where it can only skip — and a skipped test is
indistinguishable from a passing one in the output people actually read.

**On a VM rather than a container.** Tried the container first. OrbStack and
Docker run systemd under LXC, and LXC ships a generator that writes a global
drop-in setting `ProtectSystem=no`, `NoNewPrivileges=no` and empty
`ReadWritePaths=` for every service, regenerated on each `daemon-reload`. The
harness reported that no filesystem boundary held — the same thing it would
report for a genuinely broken unit file. QEMU with a stock Debian cloud image
costs three minutes of setup and answers the question.

## What it costs

- **Two places to look.** A failure can be in the decision or in the execution,
  and the two suites are read separately. The `Action` list is the seam, and
  keeping it small is what stops that from getting worse.
- **A control file rather than a socket.** Deliberate — §10 wants recovery to
  work without a healthy host socket, and the simplest way to keep that true is
  not to have one — but it is a poor fit for anything richer than "switch to
  this owner". Stage 6's protocol will need more, and will have to keep the
  recovery path independent of whatever it adds.
- **`/tmp` is writable by every session.** Display ownership is registered in
  `/tmp/epframebuffer.lock`, `ProtectSystem=strict` makes `/tmp` read-only, and
  `PrivateTmp=yes` would hide the registry the session has to join. The VM
  found this the direct way: sessions started, reported ready, never took the
  panel, and every switch timed out into recovery. Recorded in the isolation
  report rather than papered over.
- **A VM to maintain.** `tools/vm-harness/` builds it from a stock image, and
  it is disposable.
- **A slower watchdog.** 80 s rather than 20 s, because the restore underneath
  is synchronous. Making the restore asynchronous would buy it back, at the
  price of a state machine that has to model a recovery in flight.

## What would make it wrong

- **A device where `MemoryMax=` works.** Then the profile is stale and the
  memory row should be re-derived, not edited.
- **Xochitl's unit gaining a working `OnFailure=` target.** Much of the
  caution here — two restore attempts, `reset-failed` instead of `restart`, a
  protected pid set — exists because a failed Xochitl currently means a serial
  console. If reMarkable ships `remarkable-fail.service`, that calculus
  changes.
- **`platform/device` gaining a retry loop, or losing the shared budget.**
  Decision 5 depends on `Stock::restore` doing exactly two guarded starts and
  `StartBudget` being persisted. If either changes, `FailurePolicy::restore_attempts`
  is wrong and `HOST_WATCHDOG` is wrong with it.
- **Real IPC between host and apps (Stage 6).** A socket the recovery path
  depends on would undo decision 3. The rule to keep: `paperctl stock` must
  work when everything else is dead.
- **A session that legitimately stops its main loop.** The progress witness
  assumes a drawing loop that ticks. A genuinely event-driven app that sits
  idle for six seconds would be killed as hung, and the witness would need a
  way to say "idle on purpose" — which a hung app could also say, which is why
  it is not there yet.
