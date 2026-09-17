# device-probe

A static aarch64 probe used to establish display and power facts on Calum's
reMarkable Paper Pro (`imx8mm-ferrari`, `remarkable,cumulus-panel`). It exists to
answer hardware gates with evidence; it is not part of the Paperclip runtime and
nothing in `platform/` should depend on it.

## Build

```
zig cc -target aarch64-linux-musl -static -O2 -Wall -Wextra -o panelprobe panelprobe.c
```

No libdrm, no Qt, no dynamic linking — the probe issues raw DRM ioctls so that a
result cannot be attributed to vendor userspace.

## Safety contract

Every path that touches the tablet must leave stock Xochitl running:

- `panelprobe` saves the CRTC state before modesetting and restores it on every
  exit path, including `SIGINT`/`SIGTERM`/`SIGHUP` and an `alarm()` watchdog
  (`--watchdog`, default 240s).
- `run-gate1.sh` / `run-gate3.sh` install a `trap ... EXIT INT TERM HUP` restore
  that restarts `xochitl.service` and asserts stock health afterwards.
- Neither script uses `set -e`, `set -u`, or `date +%s%N`. busybox `date` has no
  `%N`; that combination is what left stock stopped for two extra minutes during
  WWW-1, because the script aborted after the `systemctl stop`.
- `run-gate3.sh` additionally arms a detached guardian that restarts Xochitl
  unconditionally after 200s, so losing the control channel cannot strand stock.

Run the probe over **Wi-Fi**, not USB: the USB CDC gadget does not survive the
tablet's autosleep, while Wi-Fi does (WWW-20).

## `pull-vendor-lib.sh`

```sh
./pull-vendor-lib.sh ~/paperclip-vendor [ssh-host]
```

Copies `/usr/lib/plugins/scenegraph/libqsgepaper.so` off the tablet and records
its SHA-256, the firmware build it came from, and its licence line. Three reads
and a copy; nothing on the device is modified and no service is touched.

The library is proprietary (`LICENSE: CLOSED`) and must never be committed, so
the script refuses a destination inside a git work tree. `platform/device`
links it via `PAPERCLIP_VENDOR_LIB_DIR`; see
`platform/device/native/README.md`.

Keep the recorded digest. SWUpdate replaces the whole rootfs slot on every OS
update, so a changed digest is the signal to re-run
`platform/device/native/check-abi.sh` before trusting anything that links it.

## `evname.c`, and its Rust successor

`evname` prints the `/dev/input` mapping and classifies each node from its
advertised axes. `platform/device`'s `device-report` example does the same
thing in Rust, plus the wakelock and display-lock checks, and is the one to
reach for now — this stays as the dependency-free fallback.

## `panelprobe` modes

- default — six diagnostic scenes: a packing-independent flash and grey ramp, a
  coarse framebuffer-space checkerboard, then the same asymmetric figure drawn
  under each of three candidate 4bpp packings.
- `--fiducials` — append five numbered crosses at known panel coordinates, for
  input calibration.
- `--hold S` — hold one scene with a one-second ticker, to observe what happens
  to a custom display session over time.

## Geometry, and why the packing modes are dead code

The DRM connector advertises `405x1084`, the LVDS timing from the device tree.
`CREATE_DUMB` at 32bpp yields pitch 1620 and size 1,756,080, and `405 x 4 =
1620` invites the reading that the panel is 4bpp with two rows packed per
framebuffer row.

**That reading is wrong.** The panel is 1620 x 2160 **ARGB8888**; the DRM mode
is a proprietary packed *transport* whose `405x1084 -> 1620x2160` mapping is
undocumented and has not been reverse-engineered by anyone. See ADR-0007 and
rmweb's `docs/device-profile.md`.

The three `enum packing` candidates and the geometry scenes are therefore
retained only as the record of a refuted hypothesis. They do not describe this
hardware, and nothing should be built on them. What remains useful here is the
DRM master / power-sequence / release evidence and the `--hold-flat` mode.
