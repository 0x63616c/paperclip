# ADR-0022 — App entrypoint binaries, and stdio as the launch transport

**Status:** accepted (WWW-42), extended to Home, Settings and the App Store
(WWW-60). **Not exercised on hardware.** The entrypoints this ADR adds have
been built for `aarch64-unknown-linux-gnu`, packaged, signed, published,
installed into a scratch store and confirmed to be aarch64 ELF executables.
Nothing here has run as a launched process on the tablet: the Mac has no way
to execute an aarch64 binary, and no host process on the device has spawned
one yet (WWW-4's supervisor).

## Context

Every app is a library crate. `paperctl run`/`paperctl dev` host `ChessApp`,
`SudokuApp`, `HomeApp` and `SettingsApp` in-process, behind `DevApp`
(`tools/paperctl/src/session.rs`), connected to `paper_sdk::run` over a
loopback `UnixStream::pair()`. That proves the protocol end to end, but it
proves it between two ends of the same process — never between two processes,
which is what the app contract (§8, ADR-0016) actually specifies and what
`paperctl package` assumes exists: every `paper.toml` names an `entrypoint`
(`bin/chess`, `bin/sudoku`, ...) and nothing in the workspace builds one.
`paperctl package apps/chess` and `paperctl package apps/sudoku` both fail —
identically, on the same missing file — which means neither app has ever
actually been installed from a package with a real payload.

Two questions had no answer before this stage: how does an app become an
executable at all, and how does a process launched from that executable reach
the host that speaks `paper_protocol` to it.

## Decision

### A `[[bin]]` target per catalog app, sharing the crate that already exists

`apps/chess/Cargo.toml` and `apps/sudoku/Cargo.toml` each grow a `[[bin]]`
pointing at a new `src/main.rs`, alongside the existing `src/lib.rs`. The
binary is nearly nothing:

```rust
fn main() -> ExitCode {
    let outcome = paper_sdk::run(ChessApp::new(), io::stdin(), io::stdout(), LocalSurfaces::new());
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-chess: {error}");
            ExitCode::FAILURE
        }
    }
}
```

This is deliberately the same lifecycle `DevApp::Chess` already gets inside
`paperctl` — the same `App` impl, the same `paper_sdk::run` loop — on the far
side of a real process boundary instead of a thread. No new code path needed
inventing; the entrypoint is the existing contract, wired to `main`.

Only Chess and Sudoku get one. Home, Settings and the App Store ship *with*
the platform release (project decision, 2026-09-16, §13) rather than through
the catalog, so nothing packages them with `paperctl package`, and their
`paper.toml` files still naming `bin/home` etc. is a latent inconsistency this
stage does not resolve — see "What is not established".

### The launch transport is the process's own stdin/stdout

`paper_sdk::run` takes any `Read`/`Write` pair; it does not know or care what
they are. The options were a Unix domain socket at a path passed on the
command line, or the pipes a parent process gets for free when it spawns a
child. Stdio was chosen:

- It needs no new argument convention, no socket directory, and no cleanup —
  a spawned child already has a reader and a writer the moment it exists.
- It is what a future supervisor gets for free from `std::process::Command`
  (or the equivalent unit configuration), so nothing here is thrown away when
  WWW-4 builds the real launcher.
- `codec::write_message` already calls `flush()` after every message
  (`platform/protocol/src/codec.rs`), which is what makes this safe against
  `Stdout`'s internal `LineWriter`: a length-prefixed binary message has no
  reason to contain a newline, and without an explicit flush a message could
  sit in the buffer indefinitely waiting for one.

An app must never print anything else to stdout — there is no framing that
distinguishes a stray `println!` from a protocol message, and one would be
read as a corrupt frame. Nothing in `paper_sdk` or either app does; this is a
rule for the next entrypoint someone writes, not a check anything enforces
(see `docs/app-contract.md`'s opening: the SDK is not a security boundary).

### The surface is still `LocalSurfaces`, not a host mapping

`Hello.surface` describes a drawing surface, and ADR-0016 already named the
real transport for it as a file descriptor passed with the launch connection
— WWW-4's work, unproven on this firmware. This stage does not build that.
Both entrypoints open a `paper_sdk::LocalSurfaces` provider, exactly what the
desktop preview and every existing test use: the app allocates its own
buffer and publishes into it. A conformance-test host, or a future real one,
reads `Hello.surface` and gets the same descriptor either way — what differs
is who owns the memory behind it, which is invisible to the app.

### Staging is a script, not a manual step

`archive::build` (`platform/packages/src/archive.rs`) reads exactly what the
manifest declares from the source directory it is pointed at; it does not
build anything. So `apps/chess` needs `bin/chess` sitting next to `paper.toml`
before `paperctl package apps/chess` has a payload to read. `tools/
package-app.sh <chess|sudoku>` does the cross-compile
(`--target aarch64-unknown-linux-gnu`, release profile) and copies the result
to `apps/<app>/bin/<app>`. Neither app links Qt or the vendor waveform engine
— pixels never cross the wire (ADR-0016 §3) — so this needs none of
`tools/cross/build-device.sh`'s Docker/Qt-headers/vendor-library machinery,
only the `aarch64-linux-gnu-cc` (`zig cc`) wrapper `.cargo/config.toml`
already points the target at.

`apps/*/bin/` is gitignored: it is what the script produces, not something
committed, the same relationship `/target/` already has to the rest of the
workspace.

### WWW-60: the same `[[bin]]` + `main.rs` shape for Home, Settings and the App Store

`platform/updater/src/manifest.rs`'s `REQUIRED_COMPONENTS` names four
binaries a platform release must carry; only `paperclip-host` existed. Home,
Settings and the App Store get the shape above extended to them —
`apps/home/src/main.rs`, `apps/settings/src/main.rs`,
`apps/app-store/src/main.rs`, each a `[[bin]]` beside the crate's existing
`src/lib.rs` — not a new pattern, because Chess and Sudoku already proved the
lifecycle: `paper_sdk::run` over stdio, `LocalSurfaces`, `ExitCode` mapping.

What differs is construction. `ChessApp::new()` and `SudokuApp::new()` take
nothing; `HomeApp::new(screen)`, `SettingsApp::new(host)` and
`AppStoreApp::new(screen, source)` all need something built first, and each
app's own doc is explicit that building it is *not* the library's job:

- `HomeApp`'s doc: "whoever launches Home builds the entry list and hands it
  over" — enumerating apps is a host-side concern.
- `PackagesSource::for_app`'s doc: the App Store "can only be built by
  handing it an `InstalledApp` that host policy granted `Capability::Packages`
  ... An App Store that was not granted package management gets a
  `SourceError::NotPermitted`."

So that construction stays in each `main.rs`, not in `paper_home` or
`paper_app_store`. Pushing it into the library would have been the more
reusable-looking move — `tools/paperctl/src/session.rs` already builds a
`HomeScreen` and a `PackagesSource` the same way for its own dev/run sessions
— but reusability was not the deciding question here: the library crates
deliberately keep "who decides what Home shows" and "who grants package
management" outside themselves, and a convenience constructor that let the
App Store grant itself `Capability::Packages` from inside its own crate would
have quietly moved that boundary rather than just avoided a few duplicated
lines. `apps/app-store/src/main.rs`'s `own_source` and
`tools/paperctl/src/session.rs`'s `app_store_source` are the same handful of
lines in the two places a real grant currently has to come from — there is no
host yet to hand it down from the outside (WWW-4) — and each is a "host
stand-in" for a different launch path, not a copy that should have been one
function.

Home's own shelf, for the same reason, does not read Chess's manifest the way
`session::home_screen` does for `paperctl dev`/`paperctl run`. Chess is a
catalog app, installed and versioned independently of the platform release;
Settings and the App Store are not — all three of Home, Settings and the App
Store ship together as one tested release (§13) — so `apps/home/src/main.rs`
embeds only its own, Settings' and the App Store's `paper.toml` at compile
time. A shelf that also shows catalog apps needs a runtime survey
(`paper_packages::inventory`) driven by something that exists once WWW-4
lands; nothing does yet, so it is left off rather than hardcoded to a version
that can drift from what is actually installed.

Staging is `tools/stage-platform.sh`, not `tools/package-app.sh` grown a
third case. A platform bundle's source directory
(`platform/updater/src/bundle.rs`) is `bin/paperclip-host`, `bin/home`,
`bin/app-store`, `bin/settings` all in one place; `package-app.sh` stages one
catalog app at `apps/<app>/bin/<app>`, for a different command
(`paperctl package apps/<app>`). Making one script cover both would mean an
argument that changes what directory shape it produces for two things staged
for two different consumers — a worse abstraction than two scripts that each
do one job. `stage-platform.sh` needed no Docker/Qt either, at the time: none
of the four components linked `paper-device`'s `vendor-engine` feature
(`paperclip-host` because that feature is off by default; the three apps for
the same ADR-0022 reason Chess and Sudoku do not), so the `aarch64-linux-gnu-cc`
wrapper was enough. **No longer the whole story**: ADR-0040 (WWW-86) adds a
fifth required component, `paperclip-compositor`, which *does* need
`vendor-engine` and is staged through `tools/cross/build-device.sh`'s
Docker/Qt path instead. The four components named here are unaffected.

Verified: `cargo tree -p paper-home -p paper-app-store -p paper-settings`
carries no `paper-device` edge; `cargo build --release --target
aarch64-unknown-linux-gnu -p paper-home --bin home -p paper-settings --bin
settings -p paper-app-store --bin app-store` produces three stripped aarch64
ELF PIEs; `./tools/stage-platform.sh` followed by `paperctl upgrade package
--source target/platform-stage --version 0.1.0 --protocol 1.1
--state-version 1 --key <key> --out <out>` produced a signed
`platform-0.1.0.paperpkg` whose tar listing carries `platform.toml`,
`platform.toml.sig` and all four `bin/` entries.

## Consequences

- `paperctl package apps/chess` and `paperctl package apps/sudoku` work from a
  clean checkout, once `tools/package-app.sh` has staged their binaries.
  `paperctl check`, `publish`, `install` and `rollback` were already proven
  against a stand-in payload (WWW-42's own report); this stage re-runs that
  round trip with the real cross-compiled binary and confirms the installed
  entrypoint is an aarch64 ELF that answers `Hello` with `Ready` and, on a
  `PrepareToExit`, replies `Saved` before exiting — `apps/chess/tests/
  entrypoint.rs` and `apps/sudoku/tests/entrypoint.rs` assert exactly that,
  spawning the built binary rather than linking the app in-process.
- `docs/packaging.md` and `docs/development.md` are corrected: the five-command
  publishing flow needs a staging step first, and "install an app that is not
  Chess is now a path with something in it" was not true until this stage —
  neither app had ever built a payload before it.

## What is not established

- **Nothing has run on the tablet.** Every check above — the ELF header, the
  `Hello`/`Ready`/`Saved` handshake, the install into a scratch `--root` — ran
  on the Mac, against a cross-compiled binary this build cannot execute
  itself. Running it for real needs a host process on the device to launch
  it, which does not exist yet, or a manual SSH invocation, which this stage
  did not perform: copying and executing a new binary on the tablet is a
  state-changing device operation the project's standing constraints gate on
  Calum's approval, and building that approval into a script was out of scope
  for what this issue asked for.
- **Home, Settings and the App Store now build** (WWW-60): each has a
  `[[bin]]` and cross-compiles to an aarch64 ELF with no new dependency on
  `paper-device`, and `paperctl upgrade package` accepts a directory staged
  with all four required components. What is still not established: nothing
  has run as a launched process, on the tablet or off it — `paper_sdk::run`
  over stdio is exercised by `apps/chess/tests/entrypoint.rs`-style spawn
  tests for Chess and Sudoku but not yet for these three, and the App Store
  entrypoint's self-granted `Capability::Packages` (`own_source` in
  `apps/app-store/src/main.rs`) is the same "no supervisor to ask" stand-in
  `paperctl` already uses, not a real host decision — that is WWW-4's gap,
  not this stage's to close.
- **The FD-mapped surface is still open.** `LocalSurfaces` is correct for
  every test and the desktop preview; whether a real launched process can
  receive a mapping from a real host, on this firmware, is unmeasured (see
  ADR-0016's own "what would make this wrong").

## What would make this wrong

- **If pen/touch coalescing ever needs the app to see a message before the
  socket layer has fully drained a batch**, stdio's per-message flush is the
  same cost a Unix socket would pay — this is not a reason to change it, but
  it is the assumption behind calling it free.
- **If a future entrypoint needs bidirectional back-pressure or shutdown
  semantics finer than "the writer end closes"**, a Unix domain socket carries
  those (e.g. `shutdown(SHUT_WR)` independent of the pipe halves) where a pair
  of OS pipes does not. Nothing built so far needs it.
- **If Home, Settings or the App Store ever needs to be installable through
  the catalog rather than shipped with the platform**, `tools/
  package-app.sh` gets a case for it — the `[[bin]]` + `main.rs` shape
  already matches what that script expects.
- **If Home ever needs to show a catalog app on its shelf**, the shelf
  construction in `apps/home/src/main.rs` is where that survey belongs, not a
  reason to have it read another app's `paper.toml` at compile time the way
  it deliberately does not for Chess today.
