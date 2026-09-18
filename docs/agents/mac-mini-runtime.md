# WWW-54 — the Mac mini as an agent runtime

Pre-flight for moving the Software Engineer agent's runtime from the MacBook
Pro to a Mac mini (Mac14,3, macOS 15.6, arm64, user `calum`, Tailscale name
`homelab`). Confirmed 2026-09-17: this host can check out Paperclip, build
it, cross-compile for the tablet, and push, with nothing beyond Homebrew and
`rustup`/`zig` installed.

## PATH: the one thing that bites first

The agent shell here is not a login shell. Neither `/opt/homebrew/bin` nor
`~/.cargo/bin` is on `PATH` by default — `cargo`, `rustup`, `zig`, `brew`
itself all read as "command not found" until you export them. That is a PATH
problem, not a missing install; check with `command -v <tool>` before
concluding a tool needs installing. Every command that needs the toolchain
has to carry:

```sh
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
```

## What's installed, and where

Present before this issue: `git` (`/usr/bin/git`, Apple's), Homebrew 7.0.3
(`/opt/homebrew/bin/brew`), `node`/`npm` (`/opt/homebrew/bin`), `python3`
(`/usr/bin/python3`), `jq` (`/usr/bin/jq`), and `gh` 2.101.0
(`/opt/homebrew/bin/gh`) — installed but **deliberately not authenticated**;
see GitHub auth below.

`rustup`/`cargo`/`rustc` (`~/.cargo/bin`) were already present when this run
started. The pinned toolchain and the `aarch64-unknown-linux-gnu` target in
`rust-toolchain.toml` install themselves on first build — nothing to do by
hand.

Installed by this run:

```sh
HOMEBREW_NO_AUTO_UPDATE=1 brew install zig
```

`zig` 0.16.0 landed at `/opt/homebrew/bin/zig`, pulling `llvm@21` and
`lld@21` as dependencies. It's what `.cargo/config.toml` points the
`aarch64-unknown-linux-gnu` linker at (`tools/cross/aarch64-linux-gnu-cc`) —
see "Building for the tablet" in `docs/development.md` for why zig rather
than the reMarkable SDK.

**Not installed, and not needed for the documented loop or the aarch64
build:** `cmake`, `pkg-config`, GNU `timeout`. `cargo test --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all
--check`, `cargo check --workspace --target aarch64-unknown-linux-gnu`, and
`cargo build --release -p paper-device --example device-report --target
aarch64-unknown-linux-gnu` all completed without them. They'd only matter
for `platform/device`'s `vendor-engine` feature, which links Qt from the
reMarkable SDK (`platform/device/native/README.md`) — out of scope here and
not attempted.

## GitHub auth: deploy key, not `gh`

`~/.ssh/id_ed25519_github` is an SSH deploy key scoped to
`0x63616c/paperclip` only, wired up in `~/.ssh/config` for `github.com`. A
`git-receive-pack` probe confirmed it has write access, and this issue's own
commit is the second proof. Plain `git clone`/`fetch`/`push` against
`git@github.com:0x63616c/paperclip.git` work with no further setup.

`gh` is installed but has no credential and none should be added — no
`gh auth login` (interactive, and this key isn't a `gh` credential anyway).
Anything that needs `gh` (PRs, gh-side issue comments, checks) doesn't work
from this host and isn't needed: this project pushes straight to `main`, no
PRs.

## What still needs a human

Nothing, for the scope this issue covers. Qt/SDK setup for the
`vendor-engine` feature (needed only for device-display work under
`remarkable-device-session`) still needs a human to pull the reMarkable SDK
and `libqsgepaper.so`, per `platform/device/native/README.md` — that's a
separate, already-documented gate, not something this probe was asked to
close.
