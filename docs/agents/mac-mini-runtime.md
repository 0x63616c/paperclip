# WWW-54 — the Mac mini as an agent runtime

Pre-flight for moving the Software Engineer agent's runtime from the MacBook
Pro to a Mac mini (Mac14,3, macOS 15.6, arm64, user `calum`, Tailscale name
`homelab`). Confirmed 2026-09-17: this host can check out Paperclip, build
it, cross-compile for the tablet, and push, with nothing beyond Homebrew and
`rustup`/`zig` installed.

## PATH: fixed, not a standing hazard

`export PATH="$HOME/.cargo/bin:$PATH"` is appended to `~/.zshenv` on this
host (previous file backed up at `~/.zshenv.bak`). `~/.zshenv` — not
`~/.zshrc` — is sourced by *every* zsh, including non-interactive ones, so
`zig`, `gh`, `brew` (`/opt/homebrew/bin`) and `cargo`/`rustup`
(`~/.cargo/bin`) all resolve by default now, even in a bare `zsh -c
'command -v cargo'`. That's the fact a future agent needs if this ever
breaks again: check `~/.zshenv` first, not `~/.zshrc`.

If a run ever lands in a shell that skips `~/.zshenv` (a non-zsh shell, or
one started with flags that suppress startup files), fall back to
exporting explicitly:

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

## GitHub auth: two credentials, two jobs

`~/.ssh/id_ed25519_github` is an SSH deploy key scoped to
`0x63616c/paperclip` only, wired up in `~/.ssh/config` for `github.com`. A
`git-receive-pack` probe confirmed it has write access, and this issue's own
commits are further proof. Plain `git clone`/`fetch`/`push` against
`git@github.com:0x63616c/paperclip.git` run on this key — it's what `git`
push uses, and remains the normal path for landing commits on `main`.

`gh` is separately authenticated, as the `0x63616c` user account (`gh auth
login` run interactively over SSH by Calum): `gh auth status` reports
`✓ Logged in to github.com account 0x63616c`, git protocol `https`. Use it
for anything that needs the GitHub API rather than a bare push — PRs, gh-side
issue comments, checks (though this project pushes straight to `main`, no
PRs, so there's little call for it yet). The credential lives in
`/Users/calum/.config/gh/hosts.yml` **in plain text** — the session had no
TTY for a keychain — worth knowing before that file is ever pasted anywhere.

## What still needs a human

Nothing, for the scope this issue covers. Qt/SDK setup for the
`vendor-engine` feature (needed only for device-display work under
`remarkable-device-session`) still needs a human to pull the reMarkable SDK
and `libqsgepaper.so`, per `platform/device/native/README.md` — that's a
separate, already-documented gate, not something this probe was asked to
close.
