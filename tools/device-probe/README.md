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

## Modes

- default — six diagnostic scenes: a packing-independent flash and grey ramp, a
  coarse framebuffer-space checkerboard, then the same asymmetric figure drawn
  under each of three candidate 4bpp packings.
- `--fiducials` — append five numbered crosses at known panel coordinates, for
  input calibration.
- `--hold S` — hold one scene with a one-second ticker, to observe what happens
  to a custom display session over time.

## Geometry

The DRM connector advertises `405x1084`, which is the genuine LVDS timing from
the device tree, not the panel. `CREATE_DUMB` at 32bpp yields pitch 1620 and
size 1,756,080. The panel is 1620 x 2160 at 4bpp, so one framebuffer row of 1620
bytes carries 3240 pixels — two panel rows. Which two, and in what order, is
what the three `enum packing` candidates test.
