# WWW-23 — first light, and the run that could not happen

Stage 5 asks for the real Home shelf on the actual panel, presented by a single
repeatable command. The command exists, is cross-compiled and links against the
device's own vendor library. **It has not been run on the tablet**, because the
tablet was not reachable from the Mac during this session.

## The device was unreachable

2026-09-17, ~05:20Z. Both recorded control channels, tried repeatedly across
the session:

| Channel | Result |
|---|---|
| USB CDC, `10.11.99.1` | no interface at all. `ifconfig` lists no `10.11.99.x` address on the Mac, and `networksetup -listallhardwareports` shows no USB ethernet port. The cable is not connected, or the gadget is not enumerated. |
| Wi-Fi, `192.168.0.180` | `ssh` times out; ICMP silent; the ARP entry for that address is `incomplete`. |

`10.11.99.1` was retried rather than concluded from one refusal, per the
`remarkable-device-session` preflight note. A sweep of `192.168.0.0/24` found
28 live hosts and no reMarkable: the only dropbear banner on the LAN,
`192.168.0.217`, rejects the recorded device key and is some other appliance.

The likeliest explanation is the one WWW-1 already established and this run did
not need to re-derive: a locked tablet has no `/home` mounted, therefore no
dropbear host key, therefore **no SSH at all**. Calum is asleep and the tablet
is asleep with him. Nothing here indicates a fault.

No takeover was attempted, no Xochitl restart was spent, and the device was not
touched in any way. `NRestarts` stood at 2 of 4 after WWW-3; whether the window
has since cleared is a fact only the tablet can report, and `paperctl open`
reads it in preflight and refuses on its own if it has not
(`StockHealth::allows_takeover`, ADR-0011).

## The command

```sh
paperctl open [--screen home] [--hold 60] [--waveform mono-quality]
              [--partial] [--sample-every 5] [--dry-run]
```

Sequence, all of it `paper_device::hold`'s and none of it new:

1. `StockHealth` preflight; refuse unless healthy and more than two restarts
   remain.
2. Digest the frame and refuse a blank one — **before** anything is stopped.
3. Wakelock, `Takeover::acquire` (watchdog armed, vendor lock state captured,
   start budget checked, Xochitl stopped cleanly).
4. Open `VendorPanel`; one full-panel present; sample sysfs through the hold.
5. `panel.clear()`, then `Takeover::release` — vendor locks back first, then
   stock, then the wakelock.
6. Re-read health, compare, and poll `/tmp/epframebuffer.lock` until it names
   the Xochitl that just started.

### Why the first paint is `full=1`

§9 and the session skill both say keep `full=0`, because the vendor backend
escalates a full update to the whole panel however small the rectangle. That
reasoning is about *partial* updates and does not apply here: this present is
the whole panel anyway, and it is the first paint over a completely different
image — stock's UI — which a partial-mode update would leave ghosting
underneath. So the default is one deliberate full flash, and `--partial` exists
for the case where the panel already shows a Paperclip screen.

This is a judgement, not a measurement. It is exactly the kind of thing the
first real run should look at.

### The digest

`paperctl open` prints a SHA-256 of the exact ARGB8888 handed to the engine,
before it takes the display and again in the report. Tonight's Mac render of
the Home shelf, for the morning run to be compared against:

```
sha256:1b659ca03007557f1f88b2698d51bcd61513ac5d11c89f70b3a5c9e5520d4d94
  1620x2160, ink 318/1000
```

Produced by `cargo run -p paperctl -- open --dry-run` at commit time. It will
change whenever the shelf's content does — the shelf shows app versions — so it
is a comparison for *this* build, not a constant.

A device run reporting the same digest establishes that the same pixels were
sent. It establishes nothing about what the panel did with them.

## Cross-compilation

```
tools/cross/build-device.sh --bin paperctl
```

The script now builds either a `paper-device` example (as before) or a binary,
and `--bin paperctl` uses `--no-default-features --features vendor-engine`: no
windowing stack, and no code path that can sign a release (§12).

```
ELF 64-bit LSB pie executable, ARM aarch64, dynamically linked
NEEDED  libqsgepaper.so libQt6Core.so.6 libQt6Gui.so.6 libstdc++.so.6
        libgcc_s.so.1 libm.so.6 libc.so.6
highest glibc symbol required: GLIBC_2.34   (device has 2.39)
```

Exactly the library set the WWW-3 `takeover` example resolved, so nothing new
has to be present on the tablet.

**One thing this cost a linker error to learn.** `cargo::rustc-link-arg` is
scoped to the targets of the package whose build script emits it.
`platform/device/build.rs` emits `-Wl,--allow-shlib-undefined` — needed because
`libqsgepaper.so` pulls in Qt Quick, Qml and DBus that GNU ld chases and the
build container does not have — and `paper-device`'s own example got it while
`paperctl` did not, failing at its final link on several hundred Qt symbols.
`tools/paperctl/build.rs` now repeats the flag under the same feature. It is a
link flag and nothing else: §8's boundary is about importing Qt types, device
paths and systemd, and a build script emitting a flag imports none of them.

## To run it in the morning

```sh
# Mac
tools/cross/build-device.sh --bin paperctl
ssh remarkable-wifi 'mkdir -p /home/root/paperclip/bin'
scp target/device-container/release/paperctl remarkable-wifi:/home/root/paperclip/bin/

# Tablet
/home/root/paperclip/bin/paperctl open --hold 120
```

Nothing is written to the root filesystem and no unit is installed. If SSH
still refuses, the tablet is locked: wake it and enter the passcode, which is
what mounts `/home` and starts dropbear.
