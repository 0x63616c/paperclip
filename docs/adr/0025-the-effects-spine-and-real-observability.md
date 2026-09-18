# ADR-0025 — The effects spine and real observability

**Status:** accepted. Every crate named here is exercised by unit or
integration tests; nothing here has run against a real tablet — see "What
still needs hardware" below.

Implements WWW-46.

## Context

Four effects — a clock, a spawned process, a systemd unit, a filesystem
write — were implemented once per crate that needed one, with an
inconsistent amount of care: `platform/updater` put `Clock` and
`SessionControl` behind seams and grew 34 tests that run on a Mac with no
systemd; `platform/host/src/linux/runtime.rs`'s concrete `Systemd` had none.
Six crates had their own copy of a fake to compensate: `FakeService` and
`WatchingService` (`platform/device/src/takeover.rs`'s test module),
`FakeSsh` and a duplicated `FakeProber` (`tools/paperctl`), `FakeClock`
(`platform/updater/tests/upgrade.rs`), `MemoryPanel` and `SilentFallback`
(`platform/device`).

Separately, 276 `println!` and 45 `eprintln!` calls across `platform/`,
`apps/` and `tools/` carried every log line this project has ever produced —
no levels, no structure, and nothing that survives past a terminal's
scrollback. The 2026-09-17 reboot was diagnosed by reconstructing events from
memory and scattered prints; it should have been one `journalctl` command.

## Decision

### `platform/sys`: four traits, one real adapter each

`Clock`, `Process`, `UnitControl`, `Storage`. `platform/updater`'s `Clock`
and `SystemClock` moved here unchanged — `health.rs` re-exports them under
their old path, so all 34 of that crate's tests kept passing without being
touched. `UnitControl`'s real adapter, `Systemctl`, is built **on** `Process`
rather than shelling out itself: its argv-building is a unit test against a
fake `Process`, not a manual reading of the source. `Storage`'s real
adapter, `Filesystem`, fsyncs before a commit rename, mirroring
`paper_packages::store::commit_directory`'s durability without either crate
depending on the other.

`SessionControl` (wakelock, display probe, one implicit unit) stays in
`platform/updater`. It is a legitimate domain-specific composition, not a
fifth general effect — what moved to `platform/sys` is the *pattern*
`SessionControl`/`Clock` proved: a small trait, one real adapter, a fake that
never touches systemd.

### `platform/testing`: fakes, one copy each — with one Cargo limit found

`FakeClock`, `FakeProcess`, `FakeUnitControl`, `FakeStorage` (for
`platform/sys`'s traits) and `FakeServiceControl` (for
`platform/device::stock::ServiceControl`, replacing `FakeService` and
`WatchingService` with one fake plus an `on_start` hook the "vendor lock is
put back before stock is started" regression test now uses instead of its
own bespoke wrapper type).

**Not moved, and why:** a package's `#[cfg(test)] mod tests` unit tests
recompile that package with `--test`, producing a second, distinct
compilation of it from the one a dev-dependency links against. A fake that
implements a trait from the package whose *own unit tests* want to use it
creates two incompatible copies of that trait, and rustc refuses with
"multiple different versions of crate X" — confirmed empirically while
building this change, first inside `platform/sys` itself (its `Systemctl`
tests moved to `platform/testing`, which can use `paper_sys` normally
without the cycle) and again for `platform/device`'s takeover tests, which
is why `FakeServiceControl` exists in `platform/testing` but
`platform/device`'s own `takeover.rs` still keeps its local
`FakeService`/`WatchingService` — moving them would require converting
those tests to integration tests and making their private helpers (`locks`,
`scratch`) public, which is a larger and separately-justified change.
Integration tests (a separate crate under `tests/`) link the package's
ordinary artifact and do not hit this limit — `platform/updater`'s
`tests/upgrade.rs` and `tools/paperctl`'s `tests/catalog.rs` both consume
`platform/testing` fakes exactly this way.

`MemoryPanel` is not a fake at all despite `platform/device/src/panel.rs`'s
own description of it as one: `paperctl open --dry-run` and every non-Linux
build's desktop path use it as their real `Panel` implementation. Moving it
to a dev-dependency-only crate would break both. It stays in
`platform/device`.

`tools/paperctl`'s `FakeSsh` stays where it is: `SshRunner`/`Prober` are
`paperctl`-internal traits with no `lib` target for `platform/testing` to
depend on, and `paperctl` is the only consumer. The one real duplicate found
there — `discover.rs`'s own private `FakeProber`, alongside the one already
shared in `transport/test_doubles.rs` for `doctor` — is fixed: the shared
one gained call-recording and an `with_mdns_candidates` builder, and
`discover.rs`'s copy is deleted.

### `platform/telemetry`: `tracing`, three sinks

`init_pretty` (fmt layer to stderr, `RUST_LOG`-filtered) for the Mac,
`init_journald` (Linux only, a `[target.'cfg(...)']` dependency so a Mac
build never needs a journal) for the device.

**The run-log is a `tracing::Layer`, not a function to call.**
`tools/paperctl/src/transport/runlog.rs`'s `wrap` (ADR-0023) is deleted;
`RunLogLayer` watches every span named `paperctl_run` and writes a record
when it closes. `main` opens exactly one such span around `dispatch(cli)`,
naming the subcommand from `Command::name()`; every subcommand that resolves
a device calls `paper_telemetry::run_log::record_device` to fill in the
`device` field before the span closes. This is what makes "every remote
dispatch site records a run" true of `stock`, `setup`, `install`,
`upgrade run`, `upgrade rollback`, `remove` and `run` (previously invisible)
alongside `open` and `deploy` (previously the only two that remembered to
call `wrap`) — the layer is installed once, at the one dispatch point, and
cannot be forgotten by a ninth command that gets added later.

`doctor`'s three-way exit verdict (0/1/2) does not survive a round trip
through `std::process::ExitCode`, which exposes no way to read a value back
out of it. `dispatch` therefore returns `Result<u8, CommandError>`, and
`main` builds the real `ExitCode` from that `u8` at the very end — the same
number both becomes the process's exit status and is recorded on the span.

**Spans over the sequences that matter.** `takeover` (`Takeover::acquire`
through the struct's `Drop`, held as a stored `EnteredSpan` for the whole
session so the settle gate and lock capture in `release` nest under it —
not just the entry point), `install` (`PackageManager::install`), `upgrade`
(`Upgrade::run` and `Upgrade::rollback`), `session` (one span for the whole
of `paperclip-host`'s `Supervisor::run`), `present`
(`paper_device::panel::present`, which also records the e-ink counters
below).

**The e-ink counters** (`platform/telemetry::counters::EINK`, a process-wide
static): frames presented, damage area, waveform (recorded as
`"{content:?}:{mode}"` rather than `paper_device::Waveform` itself, so
`platform/telemetry` never has to depend on `platform/device`), full-refresh
count, and present latency. `full_refresh` counts what was *requested*
(`Refresh::Full`), not a confirmed vendor-engine escalation — no software
seam can observe that from this side of the FFI boundary (ADR-0009).

**`Diagnostic` wiring** (`platform_telemetry::diagnostic::record`) converts
a tagged `DiagnosticRecord` into a `tracing` event at the right level, with
`app`, `version` and `session` attached. There is nowhere in
`platform/host` that currently reads an `AppMessage` off a connection at all
— that receive loop does not exist yet — so this is the pipeline's far end
with nothing upstream feeding it. It is ready for whichever issue builds
that loop.

**`println!`/`eprintln!` reduction.** The workspace-wide count (platform/ +
tools/, the same grep both before and after, tests and build scripts
excluded from the count but not from the search) went from 249 to 242.
`platform/host/src/linux/runtime.rs`'s five `eprintln!` calls — the
supervisor's own event log, the ones a `journalctl` filter on the `session`
span above now replaces — became `tracing::info!`/`warn!`/`error!`.
`paperclip-host.rs`'s startup errors did too, except the two that can fire
*before* `init_journald` runs or when it fails, which must stay `eprintln!`
on pain of logging a telemetry failure through the telemetry that just
failed. `tools/paperctl`'s own `println!`/`eprintln!` (222 of the 242) were
deliberately left alone: they are the CLI's output and error-reporting
surface, which the acceptance criterion's own wording carves out, not
logging. `platform/device`'s two example binaries and both crates' build
scripts were left alone for the same reason — a build script's `println!`
is Cargo's build protocol, not a log line, and touching it would be a
different kind of bug.

## What it costs

- Six crates (`platform/sys`, `platform/testing`, `platform/telemetry`, plus
  `platform/device`, `platform/packages`, `platform/host` picking up new
  dependencies) instead of none. The alternative — every crate keeps
  inventing its own seam — is the state this ADR replaces.
- `platform/testing`'s self-referential-dependency limit (above) means the
  consolidation is real but not total: `platform/device`'s own
  `FakeService`/`WatchingService` remain local. Documented rather than
  forced.
- `paper_packages::install`, `paper_updater::upgrade`'s file operations, and
  `platform/host`'s `Systemd` struct were **not** migrated onto
  `paper_sys::Storage`/`UnitControl` in this change — only `Clock` was, and
  only in `platform/updater`. `Storage` and `UnitControl` exist, are real,
  and are tested, but nothing in production calls them yet. That migration
  is real, separately-sized work against code whose current correctness
  (durability, rollback, the systemd rate-limit guard) is exactly what makes
  it risky to touch inside the same change that introduces the seam.

## What would make this wrong

- A future crate reaching for its own `Clock`/`Process`/`UnitControl`/
  `Storage` fake instead of `platform/testing`'s — the thing this ADR exists
  to stop happening a seventh time.
- `platform/telemetry::diagnostic::record` still having no caller a full
  stage later: that would mean the "wire the pipeline" half of this ADR
  shipped a dead end rather than a foundation.
- A `println!`/`eprintln!` count in `platform/` that grows back past 242
  outside a build script, an example, or `tools/paperctl`'s own CLI output.
