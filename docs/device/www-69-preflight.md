# WWW-69 — staging for the WWW-55 session, without touching the tablet

Everything below runs **on the Mac only**. Nothing here spends the tablet's
start budget, stops or starts `xochitl`, or writes to the device — those are
WWW-55's job, not this ticket's. Where a step has to land on the tablet, it
is written down here and marked as a WWW-55 step, not executed.

## Already done — do not redo

- The signing key exists: id `3318ab0d8c332ba5`, secret
  `~/.paperclip/paperclip.key`, public `~/.paperclip/paperclip.pub`, on both
  the laptop and the Mac mini (WWW-63).
- The public half is **already on the tablet**, verified 2026-09-18 03:37Z
  (WWW-55): `/home/root/paperclip/keys/paperclip.pub`, sha256
  `c792b281...ff7d69`, key id `3318ab0d8c332ba5`. Copying it again is not a
  staging step this ticket or WWW-55 needs to repeat.
- `main`'s `release.toml` declares platform version `0.1.0`, protocol `1.0` —
  matching the published `platform-v0.1.0` tag, so no version override is
  needed when packaging below.

## What has to exist on the Mac

Three GitHub releases exist (WWW-62): `platform-v0.1.0`,
`dev.calum.chess-v0.2.0`, `dev.calum.sudoku-v0.1.0`. All three are
**unsigned** — CI cannot hold the signing key (§12) — so a machine holding
the key has to assemble and sign them before anything reaches the tablet.
WWW-63 proved this chain by hand for the Chess package; the commands below
reproduce it for all three artifacts. Not executed here: this workdir has no
copy of `~/.paperclip/paperclip.key` to sign with.

### 1. The platform bundle

```sh
mkdir -p ~/paperclip-release/platform-0.1.0 && cd ~/paperclip-release/platform-0.1.0
gh release download platform-v0.1.0 --repo 0x63616c/paperclip --dir .
tar xzf platform-components.tar.gz
shasum -a 256 -c SHA256SUMS
```

The archive already unpacks to `bin/paperclip-host`, `bin/home`,
`bin/app-store`, `bin/settings` — the exact layout `paperctl upgrade package`
expects at `--source` (`platform/updater/src/bundle.rs`: "every required
component is expected at `bin/<name>`"). From the repo root, so
`--release-manifest`'s default (`release.toml`, relative) resolves:

```sh
cd <repo-root>
cargo run --release --manifest-path tools/paperctl/Cargo.toml -- \
    upgrade package \
    --source ~/paperclip-release/platform-0.1.0 \
    --key ~/.paperclip/paperclip.key \
    --out ~/paperclip-release/paperclip-0.1.0.tar.gz
```

### 2. The app catalog

```sh
mkdir -p ~/paperclip-release/apps && cd ~/paperclip-release/apps
gh release download dev.calum.chess-v0.2.0  --repo 0x63616c/paperclip --dir .
gh release download dev.calum.sudoku-v0.1.0 --repo 0x63616c/paperclip --dir .

paperctl check dev.calum.chess-0.2.0.paperpkg
paperctl check dev.calum.sudoku-0.1.0.paperpkg

paperctl publish dev.calum.chess-0.2.0.paperpkg  --catalog ~/catalogs/home --key ~/.paperclip/paperclip.key
paperctl publish dev.calum.sudoku-0.1.0.paperpkg --catalog ~/catalogs/home --key ~/.paperclip/paperclip.key

paperctl check ~/catalogs/home --trust ~/.paperclip/paperclip.pub
```

`--name` is left at its default (`calum-home`, `tools/paperctl/src/
packaging.rs`'s `DEFAULT_CATALOG`); both apps publish into the same catalog
under that one name. This is the same five-command chain WWW-63 already ran
by hand for Chess — reproduced here for both apps, not reverified.

### Toolchain note carried from WWW-63

WWW-63 found that a local rebuild of the app archive does not byte-match
CI's (`zig cc` wrapper vs `aarch64-linux-gnu-gcc`, same commit, different
bytes). The commands above **install the CI-built `.paperpkg` as-is** —
`paperctl check` verifies it opens and its entrypoint runs at the platform's
protocol, not that it matches a local rebuild — so that mismatch does not
block this staging path. It only matters if a rebuild-and-compare step is
added later (WWW-63's own scope).

## What still has to land on the tablet — a WWW-55 step, not run here

Only `FileTransport` ships (`platform/packages/src/catalog.rs`); there is no
HTTP catalog transport yet. So the catalog the App Store installs Sudoku from
has to be a path the tablet's filesystem can see directly, not a URL. Two
things need to reach the device before evidence items 3–5 (App Store
install, and the platform upgrade) are attemptable:

```sh
scp ~/paperclip-release/paperclip-0.1.0.tar.gz remarkable-wifi:/tmp/
scp -r ~/catalogs/home remarkable-wifi:/home/root/paperclip/catalog
```

Both write to the device — the first precondition WWW-69's own hard limits
rule out doing here. They belong at the start of the WWW-55 session, before
`tools/device-acceptance/run.sh --from none --to 0.1.0 ...` and before the
on-device App Store is pointed at `--catalog /home/root/paperclip/catalog`.

## Evidence checklist — one artifact per ticket WWW-55 closes

Derived from WWW-55's own five numbered steps (§ "What to run, and what
evidence each produces") — restated here as the concrete artifact each one
has to produce, not a new set of requirements:

| Ticket | Step | Artifact that closes it |
|---|---|---|
| WWW-6 | Home + Chess on the glass | Photo of Home's shelf showing Chess; photo of the board mid-game after tap-switching between apps; a photo (or `paperctl` state check) of the same position surviving a restart |
| WWW-6 | Sleep / wake | Photo of the frame immediately before sleep and immediately after wake, visually unchanged |
| WWW-38 | App Store installs Sudoku end to end | Terminal/log capture of the install running through `AppStoreApp` on the device (not `PackagesSource`), plus a photo of Sudoku's shelf tile and the app running afterward |
| WWW-39 | Sudoku's one-cell damage claim | Two photos (or a vendor-engine log line naming the damage region) showing a single digit entry refreshes one cell, not the panel |
| WWW-8, WWW-41 | §17 platform upgrade run | The three `paperctl upgrade status` outputs `tools/device-acceptance/run.sh` already asserts on — healthy commit, refused-corrupt, rolled-back-failure — pasted verbatim |

Every row also needs `systemctl show xochitl` (`ActiveState`, `NRestarts`)
and `systemctl --failed`, before and after, per WWW-55's unattended rule 1 —
one capture covers all five rows, not one per row.

## What only a device session can do

Nothing above is device-qualified by anything in this ticket. Left entirely
to WWW-55:

- Running `tools/device-acceptance/run.sh --check-only` for real, against the
  real tablet, to confirm the four preconditions before spending any start
  budget.
- The two `scp` transfers above.
- Every step in the evidence checklist — all five require the real panel,
  the real vendor engine, or a real install transaction; none is reproducible
  in the VM harness or on the Mac.
