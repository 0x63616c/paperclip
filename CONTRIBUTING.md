# Contributing

Paperclip is a personal project built for one person's tablet (see the
project description at the top of this repository's issue tracker). It is not
looking for outside contributors, and issues and pull requests from anyone
other than the maintainer are unlikely to be reviewed. This document exists so
that anyone reading, forking, or building the code — including future-you —
knows what "correct" means here.

## Before changing anything

Read [`docs/development.md`](docs/development.md) and
[`docs/adr/`](docs/adr/README.md) first. The ADRs record *why* the code is
shaped the way it is, including several bets Stage 1 made before the device
survey; a change that looks like an obvious improvement may be undoing one of
those on purpose, or unknowingly repeating a mistake `docs/assumptions.md`
already recorded and withdrew.

## The loop

```sh
just ci
```

reproduces exactly what CI runs: `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo test --workspace`, a
rustdoc broken-intra-doc-link check, that the device half type-checks for
`aarch64-unknown-linux-gnu`, that `paperctl --no-default-features` builds (the
tablet has no windowing stack and must not hold a signing key — §12), a
`cargo-hack` feature-matrix, and the signing-boundary assertion. Run `just
--list` for every other recipe, including the desktop preview, screenshots,
packaging, and the VM failure harness.

All of it must pass before anything is pushed. Clippy runs with `-D warnings`
deliberately: `Cargo.toml`'s `[workspace.lints]` are the style rules from §7,
and a warning nobody has to fix is a rule nobody follows.

A git pre-commit hook (fmt) and pre-push hook (fmt, clippy, test) install
themselves into `.githooks/` the first time the workspace builds — see
[`docs/development.md`](docs/development.md#git-hooks-www-66). They catch the
same failures CI does, earlier.

## Style

- Concrete structs and enums. No trait-per-struct, no generic plugin
  factories, no global context object.
- Private modules by default; a crate's public surface is its `lib.rs`
  re-exports.
- `Result` for failure, with typed errors that name the field and say what
  was wrong with it.
- Comments explain *why*, never *how* — a comment that only restates the code
  above it is a comment to delete.
- No `paperclip_` prefix on things that are already inside a Paperclip crate.

See [`docs/development.md`](docs/development.md#style) for the rest.

## Tests

Default to writing a failing test before the fix or feature that makes it
pass. Two kinds of test live in this repository and they are not
interchangeable:

- **Everything under `cargo test --workspace`** proves a *decision* is
  correct — that a state machine, a parser, or a signature check does what it
  claims — and runs on a Mac.
- **`tests/failure-harness`**, run in an aarch64 Linux VM
  (`tools/vm-harness/`), proves a decision is actually *enforced* by real
  systemd. It is a binary, not a `cargo test` target, precisely so it cannot
  quietly skip on a Mac and leave a green line behind.

Neither is device qualification. **A running process is not proof the screen
is usable, and passing mocks or local tests is not device qualification** —
see [`docs/assumptions.md`](docs/assumptions.md) for what remains
unestablished until it has run on the actual tablet.

## Architecture decisions

A change that overturns something an ADR decided should update or supersede
that ADR in the same change, not leave it describing a decision the code no
longer makes — a stale ADR has already cost this project once (see the
project's own record of ADR-0009). `cargo xtask new-adr "<title>"` scaffolds a
new one and adds its row to `docs/adr/README.md`.

## Commit messages

Every commit begins with the issue key it addresses (e.g. `WWW-45: ...`).
Fuzz corpora, `artifacts/`, and anything under `target/` are not committed.
