# ADR-0023 — `paperctl logs`, `doctor` and `deploy`

**Status:** accepted, with one mechanism superseded by ADR-0025 (WWW-46) —
see the note below. The record shape, the retention count, and every other
decision here are unchanged.

Every command here is exercised by unit tests
(`tools/paperctl/src/logs.rs`, `doctor.rs`, `deploy.rs`) against fakes;
nothing in this ADR has run against a real tablet, because none of it needs
one to be correct — see "What still needs hardware" below.

Implements spec §4 (`paperctl` as the entry point) for WWW-34.

> **WWW-46 update:** `transport::runlog::wrap`, described below as what
> `open` and `deploy` call, no longer exists. The run-log it wrote is now
> `platform/telemetry::run_log`, installed once as a `tracing::Layer` around
> a span `main` opens for every dispatch — see ADR-0025. This is what closed
> the gap the rest of this ADR does not mention: `wrap` was only ever called
> from two of what are now nine device-touching commands. The record format,
> `RETAIN`, and everything `paperctl logs` prints are unchanged.

## Context

A set of personal shell functions on Calum's Mac had grown up around gaps in
`paperctl`: one that read a fixed `/tmp` path after a Mac-forwarded `open`
for what happened, two that checked `xochitl.service`'s health and which
transport route resolved, and one that cross-compiled `paperctl` and copied
it to the tablet by hand. None of this was in the specification, and it
shadowed the documented CLI closely enough that typing the real command name
by habit failed twice. Those functions have been deleted; this ADR is where
the three pieces of real, missing capability they covered land instead.

WWW-33 already built the Mac-side transport and its resolution order
(`--device`, `PAPERCTL_DEVICE`, a pin, USB, mDNS, the cache); this issue
builds on top of it rather than re-deriving it.

## Decision

### `paperctl logs`

Every long device operation records one run through
`transport::runlog::wrap`: an id, a Unix-seconds start time, the subcommand,
which device it targeted (when resolution got that far), an exit code, and
the path of the record itself. Records are individual JSON files under
`runs/` next to `paperctl devices`' own config file — one existing directory
convention, not a second one to keep synchronised with it. The newest 20 are
kept; `logs` prints the newest by default, `--last N` an older one, `--list`
every retained record, `--output json` the same data as JSON. An empty or
missing run log is a named error (`no runs retained yet; looked in <path>`),
never a quiet empty list — the previous ad-hoc path failing silently, by
being wrong, is exactly the failure mode this replaces.

`logs --device`/`PAPERCTL_DEVICE` filters by which tablet a run targeted, but
deliberately only through `--device`, the environment variable, a pin, or
the cache — not USB or mDNS. Reading local history must not depend on live
network discovery; the two sources that need a reachable tablet to mean
anything are left out, so the four remaining keep their relative order.

### `paperctl doctor`

Resolves a device through the identical `discover::resolve` order WWW-33
already established (and the identical `DeviceSource` enum `paperctl
devices --output json` reports), then — unlike every command that only
forwards a subcommand — confirms reachability itself even for `--device`,
`PAPERCTL_DEVICE` and a pin, which `resolve` otherwise trusts blind because a
pin is allowed to name a sleeping tablet. A diagnostic's whole job is
answering whether the tablet answers, so it cannot inherit that trust.

When reachable, three more read-only SSH queries follow, over the same
transport, none of them able to change device state:

- `systemctl show xochitl.service -p ActiveState -p NRestarts` — reusing
  `paper_host::units::XOCHITL_UNIT` rather than a second hardcoded string.
  `NRestarts > 0` is a finding, not a pass — WWW-23's own reboot is why.
- `cat` on the vendor display lock registry, parsed with the same
  `paper_device::session::DisplayLockHolder` the takeover code already uses,
  not a second ad-hoc parser.
- `<remote paperctl> --version`, compared against this binary's own
  `CARGO_PKG_VERSION`.

The result is one of three verdicts, and the exit code carries it:
`0` healthy, `1` reachable but degraded (a restart, the lock held, or the
device binary missing or mismatched), `2` unreachable. Three states needed a
dispatch path outside the uniform "any error is exit 1" one `main` uses for
every other command; `doctor` alone is intercepted before it reaches that
path.

**The reachability budget is not `open`'s.** WWW-35 raised
`discover::PROBE_TIMEOUT` to 30 seconds so a sleeping tablet's Wi-Fi has time
to wake on the first probe. `doctor` and `deploy` cannot spend that and still
answer "no device reachable" inside their own 15-second bound (acceptance
criterion 8), so they resolve against a separate, shorter
`discover::QUICK_PROBE_TIMEOUT` (4 seconds) instead. The accepted cost: a
diagnostic against a sleeping tablet reads as unreachable rather than waiting
to find out, and the fix is to wake it first, not to widen the budget.

### `paperctl deploy`

The dev-loop replacement for cross-compiling `paperctl` and copying it to
the tablet by hand: `tools/cross/build-device.sh --bin paperctl` locally,
then one remote shell command over the WWW-33 transport —
`cat > <path>.new && chmod +x <path>.new && mv <path>.new <path>` — rather
than a second protocol. `ssh host 'cat > path' < file` does what `scp` would
without adding a binary this repository would then have to trust; the
project's own instruction to prefer the system `ssh` binary over a second
implementation of anything extends naturally to not reaching for a second
program at all when `ssh` already carries a file over stdin.

The move is the last step so there is never a half-written `paperctl` at the
live path for something else to try to run. `--dry-run` prints both command
lines and reaches no device at all — not even a resolution attempt — because
the text of the remote command does not depend on which host it will
eventually run against.

`deploy` never detaches. Only `open`'s long hold needs to survive a dying
SSH session (WWW-23); a binary copy takes seconds, and blurring that
distinction would be the kind of thing this ADR exists to avoid.

This is the dev-loop deploy of the tool itself — not `upgrade` (§13,
signed releases through the install transaction) and not WWW-24's lifecycle
ergonomics. It signs nothing and goes through no catalog.

## What still needs hardware

Every acceptance criterion machine-checkable from a Mac with no tablet
attached is covered by the unit tests named above. Not yet exercised against
a real Paper Pro:

- Whether `doctor`'s three SSH queries return the shapes assumed here
  against the actual device (the lock registry's shape is already confirmed
  by WWW-20's capture; the `systemctl show` and `--version` shapes are not
  yet confirmed against this specific tablet).
- Whether `deploy`'s install command actually replaces a running
  `paperctl` cleanly under real network conditions.

Calum's own hardware checklist is in the WWW-34 issue's final comment.

## Consequences

- `runlog`, `discover::QUICK_PROBE_TIMEOUT` and the `SshRunner::run_capture`/
  `run_with_stdin` primitives are new, reusable surface: any future
  diagnostic or file-transfer need reaches for these rather than a fourth
  bespoke SSH invocation.
- `paperctl devices`' `OutputFormat` enum moved to `transport` so `logs` and
  `doctor` share it rather than each declaring an equivalent one.
