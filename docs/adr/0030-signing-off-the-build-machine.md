# ADR-0030 — Signing off the build machine

**Status:** accepted (WWW-63), **the "never a GitHub runner" rule superseded
by ADR-0041** — Calum made an informed decision to move the key into a GitHub
Actions secret, after being told this ADR's tradeoff in so many words. The
`paperctl sign-release` command this ADR built, its rebuild-and-compare
refusal, and `--trust-ci-build` are all unchanged and still exactly what
ADR-0041 invokes — only *where* the key is allowed to live changed.

## Context

WWW-62 made `.github/workflows/release.yml` build and publish app archives
and platform components to GitHub as unsigned assets — CI never holds the
signing key (§12: the secret half must never be reachable from anywhere that
could hand it to an attacker who compromises a shared runner, and a key
GitHub can read makes a signature prove only that GitHub said so). WWW-63 is
the other half: a command that takes what CI published, signs it with the key
that lives only on the Mac and the Mac mini
(`~/.paperclip/paperclip.key`, mode 0600, never on GitHub), and attaches the
signatures back to the same release.

ADR-0013 already made the signing surface tiny — a few hundred bytes of TOML
per release, detached signatures, never the archives directly — so "sign
somewhere else" costs little on its own. What this ticket had to answer is
narrower and more concrete: given CI's bytes, how much of them should the
signing machine independently re-derive before it signs, rather than simply
trusting whatever arrived?

No prior art search applied here beyond ADR-0013 and ADR-0026 (the tag and
digest convention this ticket's tooling reads) — this is a decision about
this repository's own two build environments, not a display or protocol
question the reMarkable community projects (quill, rmweb) have any bearing
on.

### The finding that made "rebuild and compare" a real question

`.paperpkg` archives are deterministic by construction (ADR-0013: entries
sorted, uid/gid/mtime zeroed, modes assigned). That guarantee is about the
archive *format*, not about whatever compiler produced the entrypoint binary
packed inside it. `release.yml` links with a real cross GCC
(`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc`,
installed via `apt-get install gcc-aarch64-linux-gnu` on the runner); this
repository's own default (`.cargo/config.toml`) is `tools/cross/aarch64-
linux-gnu-cc`, a `zig cc` wrapper, because no Mac this runs on has a real
cross GCC on `PATH`. Verified 2026-09-18 against the real published Chess
0.2.0 release, same commit: CI's archive hashes to
`sha256:54e32eae5612...`; a local rebuild with the zig wrapper hashes to
`sha256:d921072fc34f...`. Different compilers, different bytes, from
identical source — not tampering, and not an archive-determinism bug.

## Decision

`paperctl sign-release <tag> --catalog <dir> --key <path> [options]`
(`tools/paperctl/src/release_sign.rs`) — one command, Mac-only, manual (a
person runs it; nothing polls or triggers it, per the ticket's own
instruction not to build that here). Behind the `publishing` feature with
`key`/`package`/`publish`/`check`, the other commands that can hold a
[`SecretKey`](../../platform/packages/src/signing.rs): the device build
(`--no-default-features`) does not compile this module at all, and
`tools/assert-no-signing-path.sh` / `just signing-boundary` confirm the built
binary carries no signing symbol, rather than that being assumed from the
`#[cfg]`.

It resolves the tag itself — `platform-v<version>` against `release.toml`,
`<app-id>-v<version>` by reading every `apps/*/paper.toml` with
[`Manifest::read_package`](../../platform/packages/src/manifest.rs), the
same tag shape `cargo xtask plan-release` derives (ADR-0026), but not shared
code with it: `xtask` is a separate binary crate `paperctl` does not depend
on, and re-reading two small TOML files through types this crate already has
was less than the cost of a new cross-crate dependency for one lookup.
Downloads what CI published for that tag (`gh release download`), decides
what to verify, signs by calling straight into the existing `package`,
`publish`, `check` and `upgrade package` command functions in-process (not
by re-invoking `paperctl` as a subprocess of itself), and uploads the result
back to the release with `gh release upload --clobber`.

### App half: rebuild and compare, by default, with a named escape hatch

The command always attempts a rebuild from source using whatever
`aarch64-unknown-linux-gnu` linker the calling environment has configured,
and refuses to sign unless the rebuild's archive byte-matches the one being
signed. On both Macs this runs on today that refusal fires on every
legitimate release, because of the toolchain gap above, not because anything
was altered. `--trust-ci-build` is the deliberate, explicit way past that: it
skips the rebuild and signs CI's bytes as published. Proven both ways against
the real `dev.calum.chess-v0.2.0` release (WWW-63's result comment has the
transcript): a validly-packaged archive with one byte changed inside it is
refused, with both digests printed; `--trust-ci-build` against the same
release signs cleanly.

Rejected: silently skipping the compare whenever `aarch64-linux-gnu-gcc` is
absent from `PATH`. A verification step that fails for a benign reason and
gets waved through without comment is worse than no verification step at all
— it trains whoever runs it to stop reading the message before the day it is
not benign. Refusing by default, with the message naming the known benign
cause and the two ways past it, keeps the operator in the decision instead of
the command making it for them silently.

Also rejected, for now: making CI itself link with the zig wrapper, so both
sides use the same (likely more reproducible) toolchain instead of trying to
match the Mac to CI's real GNU cross GCC. That would fix this at the root
rather than working around it, but it means changing `release.yml` — a file
WWW-62 already shipped and evidenced — and pinning a `zig` version CI and
every signing machine would have to agree on, which is a larger, riskier
change than this ticket's own "small" framing licenses. Worth reconsidering
if the mismatch keeps being the answer every time this command runs; not
decided here.

### Platform half: not rebuilt, ever

Rebuilding all four platform components (`paperclip-host`, `home`,
`app-store`, `settings`) is a full workspace release build, cross-compiled.
The Mac mini this is meant to run on day to day is an 8 GB M2 that already
swaps under lighter builds than that (WWW-63's own description). Attempting
it in this command risks turning a five-minute signing step into a machine
that has to be left alone for an hour, or that OOMs partway through — cost
disproportionate to what it would buy, given the app half already exercises
the rebuild-and-compare mechanism and proves it works. What the command does
instead: check the downloaded `platform-components.tar.gz` against its own
`SHA256SUMS` (catches corruption in transit, nothing about authenticity or
who built it — `verify_sha256sums` in `release_sign.rs`, unit-tested against
both a matching and a tampered file) and sign what CI built, every time,
saying so in its own output rather than presenting a checksum-consistency
check as if it were independent verification.

## What it costs

- Every app release signed with `--trust-ci-build` (which is every one signed
  on either of today's two Macs, until one of them gets a matching cross
  toolchain) trusts CI's build unverified for the compiled bytes — the same
  trust `docs/device/www-69-preflight.md` already recorded by hand for the
  three releases signed before this command existed. The archive's *shape*
  is still checked (`paperctl check` opens it the way a device would, before
  anything is signed); only "these exact bytes came from this exact source"
  is not re-derived.
- The platform half gets no independent verification of the binaries
  themselves at all, ever, by design. A compromised CI runner that swapped
  one of the four binaries for something else, while keeping
  `SHA256SUMS` internally consistent, would sign clean. This is the same
  trust boundary WWW-62 already accepted by building on GitHub-hosted
  runners with no vendor library and no secret in the workflow; this ADR
  does not widen it, but it does mean this command cannot be pointed to as
  the thing that would catch it.
- `sign-release` shells out to `gh` (download and upload), `tar` (platform
  components), and `tools/package-app.sh` (staging the app entrypoint before
  the in-process rebuild) via `std::process::Command`, the same pattern
  `paperctl deploy` already uses for its own cross-compile step
  (`tools/paperctl/src/deploy.rs`). `gh` needs its own `gh auth` session on
  the signing machine; nothing here carries a GitHub token itself.
- `PublishArgs`, `packaging::PackageArgs`, `CheckArgs` (`packaging.rs`) and
  `upgrade::PackageArgs` had their fields widened from private to
  `pub(crate)`, and `upgrade::package` from private to `pub(crate)`, so
  `release_sign.rs` can construct them and call straight into the existing
  command functions instead of duplicating what they do. No behavior of
  `paperctl key` / `package` / `publish` / `check` / `upgrade package`
  changed.

## What would make this wrong

- A cross toolchain that reproduces CI's `aarch64-linux-gnu-gcc` output
  byte-for-byte becomes available on a Mac (installed deliberately, or CI
  switches to the zig wrapper per the rejected option above). At that point
  `--trust-ci-build` should stop being the default path for every release
  signed here, and this ADR's "every release signed... trusts CI's build
  unverified" line becomes stale.
- The Mac mini turns out to handle a full workspace release build for
  `aarch64-unknown-linux-gnu` without swapping meaningfully (measured, not
  assumed) — `tools/stage-platform.sh` already builds all four components
  from source for local testing, so wiring it into a real compare would not
  need new build plumbing, only the resource budget this ADR currently says
  is not there.
- No device session has verified a release this command signed. The
  tablet-side verification recipe in `docs/packaging.md` is written to be
  precise enough for WWW-55 to run, not evidence that it works — that
  evidence is WWW-55's, not this ticket's.
