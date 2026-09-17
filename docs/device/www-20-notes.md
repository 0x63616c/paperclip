# WWW-20 — device session findings

Evidence captured 2026-09-17 on Calum's Paper Pro, image `20260827113527`,
kernel `6.12.49+git-imx8mm-ferrari`. Raw logs alongside this file.

## The panel is driven by the kernel, not by vendor userspace

A static C binary issuing raw DRM ioctls, with no Qt and no `libepaper.so`,
enabled the CRTC and the kernel performed the entire EPD power sequence:

| Rail | Before modeset | During modeset |
|---|---|---|
| `VCOM` | `disabled`, 0 users | `enabled`, 1 user |
| `VPOS1/2/3`, `VNEG1/2/3`, `VGH2`, `VGL`, `VPDD` | 1 user | 2 users |
| connector `card0-LVDS-1` | `disabled/Off` | `enabled/On` |

Sustained across 67 one-second samples (79s), then released cleanly. The rails
are owned by the in-kernel `remarkable-cumulus-panel` driver, whose device-tree
node holds `vcom-supply` … `vpdd-supply` and `vcom-voltage`. The EPD PMIC is a
`g2194-regulator` at i2c `1-0048`.

So powering and scanning out the panel does **not** require the Qt stack. What
`libepaper.so` still owns is pixel formatting and waveform selection
(`EPFramebufferAcep2`, `EPFramebufferSwtcon`).

## There is an FPGA display bridge, reprogrammed on every resume

`rm-cumulus-bridge` at i2c `2-0022`, a Lattice CrossLink-NX driven through
`/sys/class/fpga_manager/fpga0` (`Lattice sysCONFIG FPGA Manager`, state
`operating`), bitstream `/lib/firmware/cumulus_bridge.bit`.

Every single `PM: suspend exit` in the kernel ring buffer is followed by:

```
rm-cumulus-bridge 2-0022: FPGA programming took 73..80 ms
```

The bridge loses its configuration in suspend and the kernel reloads it on
resume. Cost is ~77ms before the panel can be presented to, on every wake.

## Suspend is governed by autosleep, and cannot be requested directly

- `/sys/power/autosleep` = `mem`; suspends are `PM: suspend entry (deep)`.
- Because autosleep is enabled, `echo mem > /sys/power/state` returns
  **`EBUSY`**. Paperclip cannot trigger a suspend, and does not need to.
- To *inhibit* suspend, use `/sys/power/wake_lock` / `wake_unlock`. Both are
  mode `0660`, group `xochitl`, so an unprivileged process in that group can
  hold a wakelock. Verified by taking and releasing `paperclip-gate3-test`.
- While the tablet is on charge a `udev.charger` wakelock is held, so it does
  not suspend at all. 265 completed suspends over 31,167s of uptime — about one
  every two minutes on average, not the once-per-1-2-seconds WWW-1 inferred.
  Individual cycles are short: entry to exit is typically 200-300ms, aborted by
  a wakeup IRQ from `elants_spi` (touch), `max77963-inokb` (charger) or the
  power key.

## `/home` is real disk encryption, unlocked by Xochitl

`/dev/mapper/home-encrypted-disk` (ext4) — plus `swap-encrypted-disk`, and
`persist` mounted **read-only** at `/var/lib/remarkable`. `/usr/bin/pincode-rs`
exists. `home.mount` pulls in `xochitl-home-ready.target`, and its drop-in says
"xochitl waits for the target in-process after mounting home".

Consequence: Xochitl must run before `/home` is available, so Paperclip cannot
be the first thing on screen after boot and must not attempt to replace the PIN.

## Control channels

- USB CDC (`10.11.99.1`) **does not survive autosleep**; the host interface goes
  quiet and recovers only after the gadget re-enumerates.
- Wi-Fi (`192.168.0.180`) works and survives suspend. WWW-1 recorded Wi-Fi SSH
  as never established; it is available. Use it for anything spanning a suspend.

## Input coordinate spaces are one space, not two

Pen (`event2`) is 11180 x 15340; touch (`event3`) is 2064 x 2832.

```
11180 / 2064 = 5.416666…
15340 / 2832 = 5.416666…   (both exactly 65/12)
```

The two digitizer spaces are therefore the *same* physical space at a fixed
65/12 scale — one calibration against the panel yields both transforms. Neither
matches the panel's 1620 x 2160 aspect (0.729 vs 0.750), so the digitizer's
active area is taller than the visible panel and the mapping needs a measured
offset, not just a scale.

## Still open

- Whether the pixels written under any candidate packing are legible on the
  panel. Requires a person looking at the tablet.
- The panel-to-digitizer transform, which requires taps on rendered fiducials.
- What happens to a custom display session across an actual suspend. Could not
  be forced: `/sys/power/state` is `EBUSY` under autosleep and the tablet was on
  charge, so no suspend occurred during the 26s window the session was held.

## Ghosting, and whether a static scanout can render at all

Three photographs of the tablet, taken across three held patterns, all show the
same thing: a **crisp, legible stock Xochitl UI** with faint artifacts laid over
it — heavy grey bands and wedges after the first pass, fine vertical hairlines
after the later ones. None of them shows the figure that was drawn.

Two conclusions, in increasing order of importance.

**Releasing the display leaves residue.** Xochitl's own repaint does not clear
it. `clear_panel()` — full-field inversions slow enough for a waveform to
complete — removes most of it; six cycles at 400ms left hairlines, so it is now
ten at 600ms. Whether that is sufficient is unverified. Treat a flush before
releasing DRM master as a §9 requirement, and note that the flush only runs on
the normal exit path: a crashed adapter still leaves ghosts.

**The panel may not be renderable from a static framebuffer at all.** If the
scanout were driving the panel, the stock UI would be *gone* during a hold, not
sitting there legibly underneath. E-paper pixels are driven to a state by a
sequence of voltage frames, not by a value latched from a framebuffer, and the
vendor's implementation is named `EPFramebufferSwtcon` — a *software* timing
controller, i.e. the host generates that sequence. Scanning out a static buffer
would then disturb the panel's charge, producing exactly these bands and
hairlines, without ever switching a pixel to a target state. Consistent with
this: the only scene that produced a clearly visible effect was FLASH, the one
scene that was *changing*.

`--hold-flat` tests this directly and without any packing assumption: full-field
black, white, then black again, a minute each. If the screen does not go solid,
static scanout cannot render here, no packing is correct because packing was
never what was wrong, and §9's Qt/`libepaper` bridge is the only way to put a
controlled image on this panel rather than merely a preference.

Result pending Calum's observation.
