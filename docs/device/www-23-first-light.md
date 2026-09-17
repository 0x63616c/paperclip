# WWW-23 — first light, and the gate that stopped it

Stage 5 asks for the real Home shelf on the actual panel, presented by a single
repeatable command. The command exists, runs on the tablet, and renders the
right frame there. **It did not present anything**: `paperctl open` refused in
preflight because `xochitl.service` still carries `NRestarts=2` of a permitted
4, which is the refusal WWW-3's near-miss put there on purpose.

Nothing was taken over. Xochitl's PID, `NRestarts` and the wakelock were
identical before and after every command in this session.

## What ran on the device

Image `20260827113527`, 2026-09-17 05:30–05:45Z, over Wi-Fi, unattended.

### Preflight

```
MainPID=37449  ActiveState=active  SubState=running  NRestarts=2
ExecMainStartTimestamp=Thu 2026-09-17 04:09:08 UTC
systemctl --failed: empty
/home is a mountpoint          notebooks: 53
/sys/class/drm/card0-LVDS-1: enabled=disabled status=connected
/sys/power/wake_lock: udev.charger        uptime 12:45
/tmp/epframebuffer.lock: pid 37449, xochitl, imx8mm-ferrari
```

The vendor registry named the running Xochitl, so WWW-3's release fix is still
holding across a reboot-free 12-hour uptime.

### `paperctl open --dry-run`, on the tablet

```
screen   home (real render of the Home shelf, paper_home::render — not a test pattern)
frame    sha256:1b659ca03007557f1f88b2698d51bcd61513ac5d11c89f70b3a5c9e5520d4d94
         (1620x2160, ink 318/1000)
plan     waveform mode 3 Mono, full=1, hold 60s, sampled every 5s
panel 1620x2160 (memory)
  swap     1620x2160 at (0,0), waveform mode 3 Mono, full=1
  timings  open 0ms, present 103ms (call latency, not settle time), held 0s, clear 1ms
  t+  0s enabled=disabled status=connected dpms=Off wake_lock=[udev.charger]
         rails=[VCOM=disabled@0.507V VGH2=off VGL=off VNEG1=off@6.000V
                VNEG2=off@12.005V VNEG3=off@24.015V VPDD=off@3.000V
                VPOS1=off@6.000V VPOS2=off@12.005V VPOS3=off@24.015V] panel=43.0C
  not presented: the frame went to a memory panel, not to the glass
dry run: the display was not touched and stock was not stopped
```

Two things this establishes, and one it does not.

**The binary runs on the tablet and renders the right frame.** The digest is
byte-identical to the Mac's, so the cross-compiled aarch64 build, the bundled
stroke font and the software rasteriser all produce the same 1620x2160
ARGB8888 the desktop screenshot does. §9's "same UI code on both backends" is
now checkable rather than asserted.

**The observation probes work on real sysfs.** The EPD rails are exactly the
ones the vendor engine logs as "setting rails to 6.0, 12.0, 24.0, -6.0, -12.0,
-24.0" (WWW-3), all reading `off` with stock idle, plus `VPDD` at 3.0V and
`VCOM` disabled at 0.5076V. Panel temperature 43.0C from the engine's own hwmon.
A takeover therefore has a real before/during/after comparison to report, not a
row of dashes.

It establishes **nothing about the glass**. The dry run drives a `MemoryPanel`
and says so.

### `paperctl open --hold 60`, on the tablet

```
paperctl: the display session did not complete
  caused by: xochitl has 2 of 4 restarts left in this window. Refusing to take
  the display: two more failures reach OnFailure and a serial-console emergency
  shell.
rc=1
```

Afterwards, unchanged: `MainPID=37449`, `NRestarts=2`, `ActiveState=active`,
`/sys/power/wake_lock` still holding only `udev.charger`. The refusal happens
before the wakelock and before the stop, so a refused run costs the tablet
nothing — which is the property ADR-0011 wanted and this is the first time it
has been demonstrated on hardware rather than in a test.

## `NRestarts` does not decay, and the guard's advice was wrong

The refusal used to end "Wait 600s for the window to clear, or reboot." This run
disproves the first half. The two restarts happened at 04:09; the reading was
taken at 05:30, some eighty minutes later, with systemd's own 600-second rate
limiter long since empty — and `NRestarts` still read 2.

`NRestarts` is cumulative since the unit was last reset. It is not the rate
limiter. Waiting never clears it. What clears it is
`systemctl reset-failed xochitl.service`, or a reboot.

The message now says that. It does **not** clear the counter itself, and that is
deliberate: this code cannot distinguish a stale counter from a tablet genuinely
two crashes from an emergency shell, and a guard that resets the number it
guards on can never refuse anything. `Stock::start_once` already runs
`reset-failed` before each start, so the operation is routine — deciding that
*this* reading is stale enough to discard is not.

## What is needed to finish WWW-23

One of, then re-run `paperctl open --hold 120`:

- `ssh remarkable-wifi systemctl reset-failed xochitl.service` — clears
  `NRestarts` to 0 without touching anything else. The rate limiter is already
  empty, so this restores the full budget rather than borrowing against it.
- A reboot, which clears it the same way but leaves the tablet locked until the
  passcode is entered, and a locked tablet has no `/home`, no dropbear host key
  and therefore no SSH at all (WWW-1).

Either is Calum's call.

## The command

```sh
paperctl open [--screen home] [--hold 60] [--waveform mono-quality]
              [--partial] [--sample-every 5] [--dry-run]
```

Sequence, all of it `paper_device::hold`'s and none of it new:

1. `StockHealth` preflight; refuse unless healthy and more than two restarts
   remain. **This is where it stopped tonight.**
2. Digest the frame and refuse a blank one — before anything is stopped.
3. Wakelock, `Takeover::acquire` (watchdog armed, vendor lock state captured,
   start budget checked, Xochitl stopped cleanly).
4. Open `VendorPanel`; one full-panel present; sample sysfs through the hold.
5. `panel.clear()`, then `Takeover::release` — vendor locks back first, then
   stock, then the wakelock.
6. Re-read health, compare, and poll `/tmp/epframebuffer.lock` until it names
   the Xochitl that just started.

Steps 3 to 6 have not run. Everything in them is WWW-3's, which completed three
round trips on this tablet, but no run of *this* command has reached them.

### Why the first paint is `full=1`

§9 and the session skill both say keep `full=0`, because the vendor backend
escalates a full update to the whole panel however small the rectangle. That
reasoning is about *partial* updates and does not apply here: this present is
the whole panel anyway, and it is the first paint over a completely different
image — stock's UI — which a partial-mode update would leave ghosting
underneath. So the default is one deliberate full flash, and `--partial` exists
for the case where the panel already shows a Paperclip screen.

This is a judgement, not a measurement, and the first real run should look at it.

## Cross-compilation, and two things it cost

```
tools/cross/build-device.sh --bin paperctl
```

The script now builds either a `paper-device` example (as before) or a binary,
and `--bin paperctl` uses `--no-default-features --features vendor-engine`: no
windowing stack, and no code path that can sign a release (§12 — verified,
`getrandom` is absent from the dependency graph).

```
ELF 64-bit LSB pie executable, ARM aarch64, dynamically linked
NEEDED  libqsgepaper.so libQt6Core.so.6 libQt6Gui.so.6 libstdc++.so.6
        libgcc_s.so.1 libm.so.6 libc.so.6
RPATH   /usr/lib/plugins/scenegraph
highest glibc symbol required: GLIBC_2.34   (device has 2.39)
```

**`cargo::rustc-link-arg` is scoped to the emitting package's own targets.**
`platform/device/build.rs` emits `-Wl,--allow-shlib-undefined` — needed because
`libqsgepaper.so` pulls in Qt Quick, Qml and DBus that GNU ld chases and the
build container does not have — and `paper-device`'s own example got it while
`paperctl` did not, failing at its final link on several hundred Qt symbols.
`tools/paperctl/build.rs` repeats the flag under the same feature.

**The vendor library is not on the device's loader path.** It lives in Qt's
scenegraph *plugin* directory; Xochitl finds it by asking Qt, and a plain ELF
that merely links it does not. The first deployment died at exec with
`libqsgepaper.so: cannot open shared object file`. Both build scripts now emit
`-Wl,-rpath,/usr/lib/plugins/scenegraph`, because remembering an
`LD_LIBRARY_PATH` is not the single command §4 asks for.

Both flags are link flags and nothing else: §8's boundary is about importing Qt
types, device paths and systemd, and a build script emitting a flag imports
none of them.

## To run it

```sh
# Mac
tools/cross/build-device.sh --bin paperctl
scp target/device-container/release/paperctl remarkable-wifi:/home/root/paperclip/bin/

# Tablet — already deployed at /home/root/paperclip/bin/paperctl
paperctl open --hold 120
```

Nothing is written to the root filesystem and no unit is installed. The binary
lives under `/home/root/paperclip`, and the only file `open` writes anywhere is
the start record at `/tmp/paperclip-xochitl-starts`, on tmpfs.
