# ADR-0020 — The Mac-side device transport, and where auto-discovery is still unproven

**Status:** accepted. The command-line surface, resolution order and argv
forwarding are exercised by unit tests (`tools/paperctl/src/transport/`); the
mDNS source has **not** been exercised against a tablet, because nothing on
the device side advertises the service it queries for yet. See "What is not
settled" below before citing this as evidence auto-discovery works.

Implements spec §4 (`paperctl` as the entry point) for WWW-33.

## Context

§4 makes `paperctl` the v1 entry point, and `docs/development.md` already
described a Mac-driven workflow — but `open`, `stock`, `setup`, `install`,
`upgrade` and `remove` were each written to run **on** whatever machine
invoked them. On a Mac, `open`/`stock`/`upgrade run`/`upgrade rollback`
refused outright (`NotOnDevice`); `setup` reported "this is not Linux" as a
blocking prerequisite; `install`/`remove` ran, but only against a local
`--root`, with no way to reach the tablet's own store. The gap was papered
over by a shell function in Calum's `~/.aliases` that forwarded subcommands
over `ssh` by hand — not a design, and dead code the moment this lands.

Calum's decision on the open question in the issue: **auto-discovery, plus
`paperctl devices` to list and pin one** — not discovery alone.

## Decision

### The strategy: forward the same argv, unchanged

Every device-touching command builds the exact argv a person would type
*on the tablet* and runs it there via the system `ssh` binary
(`ssh <host> -- /home/root/paperclip/bin/paperctl <argv...>`), with stdio
inherited so the tablet's own output reaches the terminal live. Nothing is
reinterpreted and nothing is staged across the link: `--catalog`, `--trust`
and a bundle path mean exactly what they mean when typed directly on the
tablet, because that is exactly what is about to run. This is the shell
alias's own strategy, moved into Rust and given a real resolution order.

`ssh` over an in-process SSH client, per the issue's own instruction: it
already carries the user's config, keys and agent, and a second
implementation of the protocol would be a second thing in this repository
that can get authentication wrong.

### Resolution order

Fixed, highest precedence first, and every `Discovered.source` in `paperctl
devices --output json` is one of these six words:

1. `--device <host>` on the command itself.
2. `PAPERCTL_DEVICE` in the environment.
3. A pin (`paperctl devices pin <host>`), stored in the one config file.
4. USB ethernet, the fixed `10.11.99.1` (WWW-20 established Wi-Fi as what
   survives autosleep; USB is still checked first because unlike Wi-Fi it
   needs no discovery step at all).
5. mDNS on the LAN, browsing `_paperctl._tcp.local.`.
6. The cached last-good host, written after every resolution that succeeds
   through sources 4 or 5.

The first three are taken on trust and never probed for reachability — a pin
is a promise to stop guessing, not a promise the tablet is awake right now,
and skipping probing is what makes "pinned" actually skip discovery rather
than fall through it silently. The last three are auto-discovery: each is
probed with a bounded timeout (2s per reachability check, 3s for the mDNS
browse), and the first one that answers wins. With nothing configured and no
tablet reachable, resolution fails inside single-digit seconds naming every
source it tried — well under the 15s the acceptance criteria asks for.

### Why mDNS is a dependency, not a hand-rolled query

Hand-parsing `dns-sd`/`avahi-browse` output, or rolling a raw UDP mDNS
client, are both real options and both fragile in ways a hardware-gated
feature cannot afford to debug blind. `mdns-sd` (MIT, pure Rust, no
`unsafe`, blocking API over an internal thread — no async runtime dragged
in) is used instead, built with `default-features = false` and gated behind
paperctl's own `discovery` feature so it is **not** part of the on-device
build: `tools/cross/build-device.sh` asks for `--no-default-features
--features vendor-engine`, which does not include it. The tablet never needs
to discover itself.

### `paperctl devices`

```
paperctl devices [--output table|json]      # list; empty is valid
paperctl devices pin <host>
paperctl devices unpin
```

Config is one TOML file, `~/.config/paperctl/config.toml`
(`$PAPERCTL_CONFIG_DIR` or `$XDG_CONFIG_HOME/paperctl` override the
directory), holding only `pinned` and `cache` — both plain strings, both
optional. `pin` validates the host is shaped like something `ssh` could take
(non-empty, no whitespace or control characters, no shell metacharacters)
**before** touching the file, so a rejected pin leaves it byte-for-byte
unchanged; it does not check reachability, for the same reason resolution
does not.

### `open` runs detached; nothing else does

WWW-23 found that a live SSH session does not reliably survive a long hold.
Every other device-touching command is a single blocking `ssh` call. `open`
instead starts `setsid sh -c '<invocation> > <log> 2>&1 < /dev/null; echo
EXIT=$? >> <log>' &` over `ssh` — the same detach shape WWW-23 already
proved by hand — and the Mac side polls the log for the `EXIT=` marker with
exponential backoff, independent of whether the SSH process that started it
is still alive.

### The local execution path is untouched

Every subcommand keeps its `#[cfg(target_os = "linux")]` implementation
exactly as it was; this only replaces what used to happen in the
`#[cfg(not(target_os = "linux"))]` arm. `install` and `remove` keep meaning
"here" by default — a scratch `--root` on the Mac for local package testing
still works with no flag — and only reach the device when `--device` is
given explicitly. `open`, `stock` and `setup` have no local-Mac meaning at
all, so for them a bare invocation now attempts auto-discovery rather than
refusing immediately.

## Consequences

- `tools/paperctl/src/transport/` is new: `config` (the pin/cache file),
  `discover` (the resolution order, `Prober` trait, `SystemProber`),
  `remote` (the SSH execution engine, `SshRunner` trait, `SystemSsh`), and
  `devices` (the CLI surface). Every device-touching command gained a
  `--device` flag via the shared `transport::DeviceArgs`, compiled only on
  the non-Linux side — it does not exist in the on-device build, rather than
  existing and doing nothing there.
- `Prober` and `SshRunner` are trait objects specifically so the resolution
  order and the exact remote command line are unit-testable without a
  network or a real `ssh` process; see `transport::discover`'s and
  `transport::remote`'s test modules, and the `remote_argv` tests beside
  each command's own `Args` struct.
- **WWW-48 update:** the eight `remote_argv()` methods this ADR originally
  described as inherent methods are now one trait,
  `transport::dispatch::RemoteCommand` (`remote_argv()` plus `shape()` —
  `Blocking` or `Detached { hold }`, replacing the `open`/`run` special case
  called out above). `transport::dispatch::dispatch(device, command)` is the
  one place that runs resolve → print the `device   <host> (<source>)`
  banner → record it in the run log → `run_blocking_with`/`run_open_with` —
  the sequence every device-touching command used to repeat by hand, nine
  times over. `deploy` and `doctor` do not implement `RemoteCommand` (neither
  is blocking-or-detached against the tablet's own `paperctl`: `deploy` sends
  a binary, `doctor` reads status), but both reach the same seam —
  `remote::resolve_and_announce[_with_timeout]` for the banner,
  `remote::default_runner()` in place of naming `SystemSsh` — so the banner
  stays written in exactly one place regardless. No subcommand module names
  `SystemSsh` any more; only `transport::remote` does.

## What is not settled

- **The mDNS source has no hardware evidence.** Nothing in this repository
  — on either side — advertises `_paperctl._tcp.local.` yet. Until a later
  stage adds a responder to the device build, this source will reliably find
  nothing, which is a correct empty result, not a validated one. Do not cite
  `paperctl devices` finding a tablet over mDNS as evidence until that
  responder exists and has been tested against the actual Wi-Fi network the
  tablet is on.
- **The detach mechanism is proven for `open` specifically (WWW-23's own
  hardware run), not re-proven here.** This ADR reimplements the same shape
  in Rust rather than shelling a literal copy of the WWW-23 command line; the
  two have not been compared side by side on hardware.
- **USB reachability has not been re-checked against WWW-20's autosleep
  finding under this transport.** WWW-20 recorded that the USB CDC gadget
  does not survive autosleep at all; this ADR still probes it first because
  it is cheap to try and free when it fails, not because it is expected to
  usually succeed.
