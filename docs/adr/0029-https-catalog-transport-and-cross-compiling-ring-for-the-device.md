# ADR-0029 — HTTPS catalog transport, and cross-compiling ring for the device

**Status:** accepted (WWW-68)

## Context

WWW-62 made releases appear on GitHub; nothing on the tablet could fetch one.
`paper_packages::catalog::Transport` (ADR-0026 already leans on its shape for
`plan-release`) is "bytes in, no opinion about them" — the only implementation
was `FileTransport`, a directory reader. This ticket is the other
implementation: bytes over HTTPS, for GitHub Releases per WWW-62 and
ADR-0026's tag convention (`<app-id>-v<version>`, `platform-v<version>`).

Prior art consulted per the project's standing research-before-theorising
instruction: neither
[quill](https://github.com/MaximeRivest/quill) nor
[rmweb](https://github.com/exp78/rmweb) touch catalog transport — they are
display-host and browser projects — so there was nothing to borrow here
beyond the general principle both already demonstrate: read the vendor's own
documented interfaces rather than reverse-engineering from first principles.
ADR-0026 is the closer precedent and is where this ADR's "what it costs"
section starts.

## Decision

### `HttpsTransport`, in `platform/packages/src/catalog.rs`

A second `Transport` next to `FileTransport`, built on
[`ureq`](https://docs.rs/ureq) 3.4 with its `rustls` backend
(`default-features = false, features = ["rustls"]`). `rustls` brings its own
certificate verifier and its own compiled-in Mozilla root store
(`rustls-webpki-roots`) plus `ring` as the crypto provider — deliberately, so
the device does not need a working, current CA bundle of its own to verify a
GitHub certificate. There is no way to construct one with verification off;
that was the ticket's hard requirement, and the type has no method that
would let a caller ask for it.

Rejected: `native-tls` (defers to the platform's own TLS + cert store, which
on a personal reMarkable image is not a thing to trust the freshness of), and
shelling out to `curl` the way `xtask::plan_release` does (ADR-0026). `curl`
is right for a Mac-side, run-once planning step; it is wrong for code that
ships to the tablet and runs inside the same process that already holds
`TrustedKeys` — a subprocess is one more thing to sandbox, one more
assumption about what is on `PATH` inside a minimal Yocto image, and buys
nothing here that a typed client does not already give for free (bounded
reads, no shell quoting).

Synchronous throughout, matching the rest of this codebase (`paper-telemetry`
states "no async runtime" outright; nothing else pulls one in either) — one
blocking `agent.get(url).call()`, same shape as `FileTransport::open`, capped
at `limit + 1` bytes with the identical `Read::take` trick so a file of
exactly `limit` bytes is not indistinguishable from one truncated at the
boundary. `Catalog<T>`, `CatalogView`, rollback (`CatalogIndex::serial`) and
the offline cache fallback are unchanged and untouched — the whole point of
the trait, proven by testing every one of those behaviours a second time
through `HttpsTransport` against a local `tiny_http` server rather than
`FileTransport` against a directory (`platform/packages/src/catalog.rs`,
`mod https_tests`).

### Certificate verification has a real test, not a assertion

A self-signed certificate for `127.0.0.1`
(`platform/packages/testdata/self-signed-{cert,key}.pem`, checked in — a
throwaway fixture, not a secret, guards nothing) served from a bare
`rustls::ServerConnection` on a raw socket. `HttpsTransport::fetch` against it
fails, because no trust store anywhere accepts it. That is the proof
verification is on: not a doc comment claiming so, a client that actually
refuses a certificate it has no reason to trust.

### `http-transport`: a feature nobody depends on yet

`paper-packages`' `default = ["publishing", "http-transport"]`, same trick
`publishing` already uses: every real consumer (`apps/home`, `apps/settings`,
`apps/app-store`, `platform/host`, `tools/paperctl`) depends on
`paper-packages` through the workspace alias, which pins
`default-features = false`, so none of them link `ureq`/`rustls`/`ring`
merely by existing. The feature is only on when `paper-packages` is built as
itself — `cargo test -p paper-packages`, `cargo clippy --workspace` — which is
what lets `cargo test --workspace` exercise `HttpsTransport` without handing
it to a single app. Confirmed empirically:
`cargo build -p paperctl --no-default-features --release` plus
`tools/assert-no-signing-path.sh` (the `signing-boundary` job) still reports
no signing symbol, and its dependency tree carries no `ureq`/`rustls`/`ring`.

Per the ticket's relaxed dependency on WWW-50 (the `Network` capability):
nothing in this repository constructs an `HttpsTransport` outside this
crate's own tests. WWW-50 landed during this ticket, but its own ADR-0028 is
explicit that `apps/app-store`'s `paper-packages` dependency is "not
analogous to Settings' — it is the install transaction itself", and that
moving it host-side "needs its own ADR, under the packaging review gate"
(WWW-7, not `ship-without-review`). `apps/app-store/src/source.rs::
PackagesSource::catalog` therefore still hardcodes `FileTransport`, untouched
by this ticket on purpose — wiring a real HTTPS catalog in means picking
that dependency up, which is reviewed work this ticket has no standing to do
under its own workflow. Whether that wiring goes through
`Capability::Network` (which exists in `paper_protocol::Capability` already,
independent of WWW-50's own no-grant system-services tier) or through the
host once package operations cross the protocol is a decision for whoever
picks up that ADR.

### Cross-compiling `ring` needed a wrapper fix

`ring` is the first device-build dependency with C/assembly sources of its
own. `cc-rs` invokes whatever compiler `CC_aarch64_unknown_linux_gnu` names
and always appends `--target=aarch64-unknown-linux-gnu` — the four-component
rustc triple — when it detects a clang-family compiler. `zig cc`'s target
parser rejects that string outright (`unable to parse target query
'aarch64-unknown-linux-gnu': UnknownOperatingSystem`; it wants the
three-component form the existing wrapper already hardcodes,
`aarch64-linux-gnu.2.36`). `tools/cross/aarch64-linux-gnu-cc` was, until now,
only ever invoked as a *linker* (its own doc comment said so), where this
never came up — rustc's linker invocation does not add a competing
`--target=`. Fixed by having the wrapper strip any incoming `--target=*`
before appending its own, so its three-component form is the only one zig
sees, and by adding `CC_aarch64_unknown_linux_gnu` (`relative = true`, since a
build script's working directory is its own crate's, not the workspace root)
to `.cargo/config.toml` next to the existing `linker` entry.

## What it costs

- A second cryptographic implementation in the device binary whenever
  `http-transport` is enabled: `ring` alongside the `ed25519-dalek`/`sha2`
  this crate already links for signature verification. There is no way to
  speak TLS without one; `rustls`/`ring` is what `ureq` documents as its
  best-supported pairing, so the choice here is only which second stack, not
  whether there is one. Not paid today — see the feature-gating decision
  above — but it is the true cost the day something requests the feature.
- `ring`'s cross-compile requirement is now load-bearing for `just
  clippy-device` and `just ci`, both of which build `paper-packages` with its
  own default features. Anyone deleting or renaming
  `tools/cross/aarch64-linux-gnu-cc`, or the new `.cargo/config.toml` `[env]`
  entry, breaks the device-target checks the moment `http-transport` is
  touched, with a cc-rs error that gives no hint the fix lives in a shell
  script two directories away. This ADR is that hint.
- `getrandom` now resolves to two major versions in `Cargo.lock` — 0.4 (this
  crate's own `publishing` feature) and 0.2 (`ring`'s dependency, via
  `rustls`). Harmless (Cargo links both; nothing shares state across them),
  but worth naming so a future dependency audit does not mistake it for
  duplication to fix.
- `HttpsTransport::url` does not percent-encode path segments before joining
  them onto `base`, matching `FileTransport::resolve`'s equivalent
  no-encoding stance: every path this method ever receives is either a fixed
  constant (`INDEX_FILE_NAME`) or a component already validated by
  `RelativePath` and, for release descriptors, drawn from an index that
  passed signature verification before `Catalog::release` ever calls
  `Transport::fetch` again. If a future caller ever hands this method an
  attacker-controlled string ahead of verification, that assumption needs
  revisiting.

## What would make this wrong

- When ADR-0028's deferred `apps/app-store` work is picked up and the App
  Store call site (`apps/app-store/src/source.rs`) is wired to
  `HttpsTransport` behind `Capability::Network` (or a host-side equivalent),
  this ADR's "nothing constructs one outside this crate's own tests" sentence
  becomes stale and should be updated in that ticket, not left to rot here.
- If a later ticket needs the device build to link `http-transport` by
  default (rather than opt-in per consumer), the feature-gating section
  above — and the binary-size cost it defers — needs re-deciding with real
  numbers from that build, not the "not paid today" hand-wave here.
- If `ring` ever needs a newer glibc symbol than the wrapper's pinned 2.36,
  or a cross-compile flag the `--target=` stripping fix does not anticipate,
  the fix in `tools/cross/aarch64-linux-gnu-cc` is the place to revisit, not
  a new one-off wrapper.
- No device session has exercised any of this — see the ticket's own
  acceptance criteria. A real fetch against `api.github.com` and a real
  release asset from a session on the tablet (or at minimum the Mac, network
  path only) is the evidence that would turn "compiles and passes local
  tests" into "the tablet can actually fetch a release."
