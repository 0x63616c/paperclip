# ADR-0026 — Declared release manifest, and the publish planner

**Status:** accepted (WWW-61)

## Context

A release trigger is "a declared version that has not been published yet."
Half of that sentence had nowhere to live: apps declare their version in
`apps/<app>/paper.toml`, but the platform did not declare one anywhere.
`paperctl upgrade package` took version, protocol and the two state numbers
entirely on the command line (`tools/paperctl/src/upgrade.rs`), so nothing in
the repository was there for a pipeline to read or a person to bump.

The other half — deciding what needs publishing — could not be a `git diff`
against the previous commit. That trigger is wrong after a force-push, after a
push containing several commits, on a re-run of a red build, and after a
revert; declared-vs-published is none of those things, because it is
idempotent and self-healing by construction.

## Decision

### `release.toml`, at the repository root

```toml
[release]
version = "0.1.0"
protocol = "1.0"

[release.state]
writes = 1
readable_back_to = 1
```

`writes` and `readable_back_to` are spelled out rather than shortened to
`state_version`/`rollback_to_state`. The doc comment on `PackageArgs` is
emphatic that the two are one editing mistake apart — leaving them equal is
what makes the updater snapshot state before activating (ADR-0019) — and a
name that says what each number means is harder to swap by accident than a
position in an argument list, or two adjacent keys with similar short names,
ever was.

This is **not the crate version**. Every crate in the workspace carries
`[workspace.package] version = "0.1.0"` because Cargo requires one; nothing
reads it as a release number, and the two are not meant to track each other. A
platform release bundles four independently-built binaries under one version
(§13); a crate's own SemVer says nothing about that, and forcing them to move
together would mean bumping unrelated crates just to ship a release with no
code change in them.

`paperctl upgrade package` reads this file. `--version`, `--protocol`,
`--state-version` and `--rollback-to-state` stay as CLI flags, but each now
**overrides** the matching field instead of being the only source: given, it
wins; absent, the file settles it. Kept rather than removed, because a
one-off build — testing a version bump before committing it, or a CI
environment adding metadata — is still a real need, and the file remains the
thing that answers "what does the next ordinary release mean" without one.

### `cargo xtask plan-release`

Reads every `apps/<app>/paper.toml` and `release.toml`, and reconciles their
versions against the GitHub release list
(`https://api.github.com/repos/0x63616c/paperclip/releases`, unauthenticated).
The fetch sits behind one trait, `ReleaseSource`, with exactly the shape
`paper_packages::catalog::Transport` already uses elsewhere in this
repository — bytes/data in, no opinion about them — so every scenario in the
acceptance criteria is a call to the pure comparison function
(`build_plan`) against a fixture `Vec<PublishedRelease>`, and no test in
`xtask` touches the network. `GithubReleaseSource`, the real implementation,
shells out to `curl` rather than adding an HTTP client dependency: `curl` is
already how this repository reaches the network from a shell script
(`tools/vm-harness`, `tools/cross`), it is present on every GitHub Actions
runner and every Mac this runs from, and a process call with no new crate is
one less thing in the supply chain for a step that changes nothing on disk.

Output is a table of `(name, version, tag, action)`, `Action` being `Publish`,
`UpToDate` or `Conflict`; `--format json` gives a workflow step something to
parse. Publishing nothing is the normal case, so the exit code is `0`
whenever nothing is a `Conflict` — including when every entry needs
publishing. It is non-zero only for `Conflict`, because that is the one case
the ticket calls a mistake someone needs to see rather than something to skip
quietly.

### The tag convention

Proposed, not yet exercised by a real publish step: apps publish under
`<app-id>-v<version>` (`dev.calum.chess-v0.2.0`), the platform under
`platform-v<version>`. Whichever ticket writes the actual publish workflow is
what validates it against a real GitHub release.

### What "identical bytes" means before anything is built

`plan-release` runs before anything is built — the ticket is explicit that
this pass does not build or sign — so it cannot compare package bytes the way
`paper_packages::publish::Publisher::publish` does one layer down (ADR-0013):
those bytes do not exist yet. What it *can* read from the tree is the
declared manifest itself, verbatim: `apps/<app>/paper.toml`'s bytes, or
`release.toml`'s bytes for the platform. `content_digest` hashes those the
way `paper_packages::Digest` spells one (`sha256:<hex>`), and a publish step
is expected to record that same digest in the GitHub release body as a
`paperclip-digest: sha256:<hex>` line.

A declared version whose tag is unpublished needs publishing. One whose tag
exists with a matching digest line is up to date. One whose tag exists with a
*different* line, or none at all, is a `Conflict` — surfaced rather than
silently accepted, matching what ADR-0013 already guarantees one layer down
("republishing identical bytes is a no-op, and republishing a version with
different bytes is refused"). Until a later ticket teaches the actual publish
step to write that line, every already-published tag is unverifiable and
reads as a `Conflict` rather than being silently trusted — the safe direction
for half a contract, and why `cargo xtask plan-release` against the real,
currently-empty GitHub release list reports six `Publish` entries and no
`Conflict` today.

## What it costs

- Two independent, small TOML readers exist for the same file —
  `paperctl::release_manifest::DeclaredRelease` reads the whole shape;
  `xtask::plan_release` reads only `[release].version`, because that is all
  the planner needs and a struct shared by two readers with different needs
  is the premature abstraction, not the duplication. If a third reader needs
  the full shape, that is the point to reconsider.
- The digest convention is invented here, for a publish step that does not
  exist yet in this repository. It is a real commitment: whatever ticket
  writes `paperctl publish` against GitHub Releases (or a CI step that
  creates them) has to write the `paperclip-digest:` line, or every declared
  version will read as a `Conflict` forever.
- `plan-release` shells out to `curl` rather than linking an HTTP client.
  That is simpler and dependency-free today, at the cost of depending on a
  binary being on `PATH` rather than a typed Rust API; if this planner ever
  needs auth headers, pagination beyond one page, or retries, an HTTP crate
  becomes the better trade.

## What would make this wrong

- If a later ticket decides platform and crate versions *should* track each
  other, `release.toml`'s `version` needs to become derived rather than
  declared, and this ADR's "not the crate version" section is the one to
  revisit.
- If the publish step ends up not using GitHub Releases at all — publishing
  straight to the private LAN catalog with no GitHub-side record — the whole
  premise of `plan-release` (GitHub as the "already published" source of
  truth) needs a different `ReleaseSource` implementation, not a different
  trait.
- If the tag convention or the digest line collide with something GitHub
  Actions or another tool already writes into this repository's releases,
  both need renaming before a real publish step ships.
