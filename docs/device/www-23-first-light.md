# WWW-23 — first light, and the lock that made it cost two incidents

Home is on the panel. `paperctl open` presents the real shelf through the
vendor waveform engine, holds it, clears it and gives the display back with
stock verifiably unharmed — `NRestarts` 0 to 0, notebooks 59 to 59,
`/tmp/epframebuffer.lock` back to Xochitl 303 ms after the restart.

Getting there rebooted the tablet once. The cause was a lock nobody had looked
at closely enough, and it is the most important thing on this page.

## The run that worked

2026-09-17 ~13:40Z, image `20260827113527`, over Wi-Fi.

```
panel 1620x2160 (vendor waveform engine)
  frame    sha256:0fb73b27198efb386d8d5dc906b190430e9ef465f7243b69e8b72cb6b0b08dd2
           (1620x2160, ink 318/1000)
  swap     1620x2160 at (0,0), waveform mode 3 Mono, full=1
  timings  open 2019ms, present 2363ms (call latency, not settle time),
           held 45s, clear 146ms
  t+  0s enabled=disabled status=connected dpms=Off wake_lock=[paperclip-takeover]
         rails=[VCOM=disabled@0.507V VGH2=off VGL=off VNEG1=off@6.000V
                VNEG2=off@12.005V VNEG3=off@24.015V VPDD=off@3.000V
                VPOS1=off@6.000V VPOS2=off@12.005V VPOS3=off@24.015V] panel=36.0C
  t+ 10s ... panel=33.0C
  t+ 20s ... panel=34.0C
  t+ 30s ... panel=31.0C
  t+ 40s ... panel=31.0C
  presented without error, appearance unverified

...panel tft: C0F
...panel fpl: AAB0AU
Loading waveforms from: /usr/share/remarkable/GAL3_AAB0AU_ID1C11_AC118TC1F2_AD1004-LHA_TC.eink
pmic: setting rails to 6.0, 12.0, 24.0, -6.0, -12.0, -24.0
pmic: setting PDD duration to 30000ms
temperature_hwmon: found temperature path: /sys/class/hwmon/hwmon8/temp1_input
shutdown: waiting for updates to complete...
shutdown: waiting for display to finish...

preflight   active pid 1804, failed [], /home mounted, notebooks 59, NRestarts 0
postflight  active pid 2430, failed [], /home mounted, notebooks 59, NRestarts 0
  NRestarts 0 -> 0; main start 13:38:16 -> 13:40:02
  /tmp/epframebuffer.lock names xochitl pid 2430 after 303ms
  stock is where it was found, and started once
EXIT=0
```

**The claim, at exactly its strength: presented without error, appearance
unverified.** The engine loaded a panel-specific waveform table selected from
this panel's own lot and TFT ids, drove the EPD rails, accepted a full-panel
ARGB8888 frame and ran its ordered shutdown. Nothing in the process saw the
glass. A wrong byte order would look identical from here (§17).

**The buffer was a real render of the Home shelf**, through the same `Screens`
code `paperctl screenshot` and `paperctl preview` use. There is no fallback
path: a screen that will not render fails the command, and a frame that
rasterises to bare background refuses the takeover before Xochitl is stopped.
Both processes rendered it independently and agreed on the digest, which is
checked before the report is accepted.

### Rails read `off` during the hold, and that is what it should say

Every EPD rail reports `off` at every sample. That is not a panel that failed to
light: e-ink holds an image with no power, and the rails are driven only while a
waveform runs. The engine's own `pmic: setting rails to ...` line is from the
present. The sampler reports what sysfs said and does not interpret it.

Panel temperature fell 36C to 31C across the hold, which is the engine's own
hwmon and the only thing here that moves.

## The incident: `checkLockFile()` takes a lock nothing gives back

At 13:26Z an earlier attempt drove `xochitl.service` into four consecutive
`SIGABRT`s, hit `StartLimitBurst=4`, and `rm-emergency.sh` rebooted the tablet.

```
xochitl[41104]: rm.framebuffer  Failed to lock epframebuffer. Is there another EPFramebuffer instance?
xochitl[41104]: another instance is already running
xochitl[41104]: default         Failed to initialize SWTCON.
systemd[1]: xochitl.service: Main process exited, code=dumped, status=6/ABRT
  ... restart counter 1, 2, 3, 4 ...
systemd[1]: xochitl.service: Start request repeated too quickly.
systemd[1]: xochitl.service: Failed to enqueue OnFailure= job, ignoring: Unit remarkable-fail.service not found.
systemd[1]: Starting reMarkable Emergency service...
unknown: /usr/sbin/rm-emergency.sh invoked
systemd[1]: dropbear@...: Unit process 40959 (paperctl) remains running after unit stopped.
```

That last line is the diagnosis. `paperctl` was **still alive** while Xochitl
tried four times to start.

`EPFramebuffer::checkLockFile()` is not a query. It *acquires* the vendor lock,
and `libqsgepaper` exposes nothing that releases it. The engine is a singleton:
`paperclip_ep_close` deletes our handle and our `QCoreApplication`, and the
engine outlives both. Confirmed directly — a presenting process holds an open
descriptor on the lock for its whole life:

```
/proc/1127/fd/4 -> /tmp/epframebuffer.lock
```

So **a process that has opened the engine can never successfully start
Xochitl.** The new Xochitl reaches its own `checkLockFile()` about 1.8 seconds
in, finds ours, fails `SWTCON` and aborts.

WWW-3 survived this by accident: its example exited inside that 1.8 s window.
WWW-23 added a fifteen-second registry poll after the restart and turned the
race into a certainty. Restoring the lock *file's contents*, which is what
ADR-0011 added, was never the fix — the contention is a live descriptor, not a
stale string.

### The fix is process lifetime, not timing

`paperctl open` is now two processes.

| | Presenting half (`--present-only`) | Surviving half |
|---|---|---|
| opens the vendor engine | yes | **never** |
| stops / starts Xochitl | never | yes |
| holds the wakelock | no | yes |
| lifetime | the presentation, then exits | the whole session |

The parent stops Xochitl, runs the child, and only once the child has been
**reaped** does it restore the lock file and start stock. The guarantee is
process death, which releases every descriptor the vendor engine holds, rather
than a sleep long enough to hope.

Proven on hardware twice over. A presenter killed mid-hold with `SIGTERM`
produced a clean restart — `NRestarts` 0, zero `another instance is already
running` lines for the whole boot — where the old path produced four core dumps
and a reboot.

## What else the tablet said

**`OnFailure=` reboots; it does not strand the device.** The skill warns that
exceeding the start limit drops the tablet into `systemd-sulogin-shell` on a
serial console. On this image `remarkable-fail.service` does not exist, systemd
logs `Failed to enqueue OnFailure= job, ignoring`, and `rm-emergency.service`
runs `rm-emergency.sh`, which reboots. The tablet came back to stock on its own
with `/home` mounted and no notebooks lost. Still a failure, and still the thing
to avoid — but the worst case is a reboot, not a dead tablet.

**The wakelock did not prevent autosleep.** During the 13:29Z session the
kernel logged six `PM: suspend entry (deep)` / `Exit AUTOSLEEP` pairs while
`/sys/power/wake_lock` contained `paperclip-takeover` for the whole window. The
skill states the wakelock prevents this. On this evidence it does not, at least
off charge. Consequences: a hold measured with `thread::sleep` stretches by the
suspended time because `CLOCK_MONOTONIC` stops, and `DetachedWatchdog`'s
`sleep N` stretches with it — the watchdog is therefore **not** the wall-clock
guarantee it is documented to be. Recorded, not explained; establishing why is
its own piece of work.

**Each resume reprograms the bridge.** `rm-cumulus-bridge 2-0022: FPGA
programming took 72 ms` … `106 ms`, on every wake, corroborating the ~77 ms
figure in the session skill.

## Running it

```sh
# Mac
tools/cross/build-device.sh --bin paperctl
scp target/device-container/release/paperctl remarkable-wifi:/home/root/paperclip/bin/

# Tablet — detached, because SSH does not reliably survive a long hold
setsid sh -c '/home/root/paperclip/bin/paperctl open --hold 45 \
    > /tmp/paperclip-open.log 2>&1; echo EXIT=$? >> /tmp/paperclip-open.log' &
```

Run it over a live SSH session and the connection may time out mid-hold; the
13:21Z attempt died that way. Detaching it also means the report survives the
link. Note that stdout to a file is block-buffered, so the log appears all at
once at the end rather than progressively — poll for `EXIT=`, not for growth.

Nothing is written to the root filesystem. The binary lives under
`/home/root/paperclip`, no unit is installed, and the only files a session
writes are `/tmp/paperclip-xochitl-starts` and the presenter's report under
`/tmp`, both on tmpfs.

### Why the first paint is `full=1`

§9 and the session skill say keep `full=0`, because the backend escalates a full
update to the whole panel however small the rectangle. That reasoning is about
*partial* updates: this present is the whole panel anyway, and it is the first
paint over stock's completely different image, which a partial-mode update would
leave ghosting underneath. One deliberate full flash; `--partial` exists for
when the panel already shows a Paperclip screen.

## Still open

- **Nothing here has seen the glass.** The framebuffer readback asked for on the
  issue — digest what we sent, digest what the engine holds, compare — is not
  built. `paperclip_ep_open` hands the engine our own `QImage`s
  (`setBuffers(tuple(front, back), &aux)`), so a readback reads buffers we own
  and whether the engine detaches them under Qt's copy-on-write is not something
  a Mac can answer. It needs a bridge entry point and a device run.
- **`held` is reported from the plan, not measured.** With suspends stretching
  `thread::sleep` that is the wrong number to print.
- **Why the wakelock does not hold.** Above.
