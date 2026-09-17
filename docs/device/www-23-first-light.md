# WWW-23 — first light, and the lock that made it cost two incidents

> **Corrected 2026-09-17 by WWW-29/WWW-30. Two claims on this page are
> withdrawn, and this log is kept as written rather than rewritten.**
>
> - **"Home is on the panel" is not supported.** Nobody looked at the glass
>   during this session. The bridge had detached its drawing buffer from the
>   one the vendor engine holds, so what the engine was given to present was
>   the all-white fill from `open` — every present between WWW-3 and WWW-30.
> - **The readback below does not establish which buffer the engine uses,** and
>   after the detach it did not even establish that the engine held our pixels.
>   `front` was read from this side's private copy, so it matched the frame that
>   was sent no matter what the engine did. `setBuffers` storing by
>   `QImage::operator=` is what actually settles it, read off the library in
>   WWW-29; see ADR-0009.
>
> Everything else here stands: the lock finding, the reboot post-mortem, the
> takeover and release sequence, the timings and the sysfs samples.

Home is on the panel. `paperctl open` presents the real shelf through the
vendor waveform engine, holds it, clears it and gives the display back with
stock verifiably unharmed — `NRestarts` 0 to 0, notebooks 59 to 59,
`/tmp/epframebuffer.lock` back to Xochitl 303 ms after the restart.

Getting there rebooted the tablet once. The cause was a lock nobody had looked
at closely enough, and it is the most important thing on this page.

## The run that worked

2026-09-17 13:49Z, image `20260827113527`, over Wi-Fi. The second of two
consecutive clean sessions; the first, at 13:40Z, was identical apart from the
readback.

```
panel 1620x2160 (vendor waveform engine)
  frame    sha256:0fb73b27198efb386d8d5dc906b190430e9ef465f7243b69e8b72cb6b0b08dd2
           (1620x2160, ink 318/1000)
  swap     1620x2160 at (0,0), waveform mode 3 Mono, full=1
  timings  open 1736ms, present 2526ms (call latency, not settle time),
           held 45s, clear 138ms
  t+  0s enabled=enabled  status=connected dpms=On  wake_lock=[paperclip-takeover]
         rails=[VCOM=enabled@0.507V VGH2=on VGL=on VNEG1=on@6.000V
                VNEG2=on@12.005V VNEG3=on@24.015V VPDD=on@3.000V
                VPOS1=on@6.000V VPOS2=on@12.005V VPOS3=on@24.015V] panel=36.0C
  t+ 15s enabled=disabled status=connected dpms=Off wake_lock=[paperclip-takeover]
         rails=[... all off ...] panel=31.0C
  t+ 30s enabled=disabled status=connected dpms=Off wake_lock=[paperclip-takeover]
         rails=[... all off ...] panel=31.0C
  readback front sha256:0fb73b27...b0b08dd2 == sent
  readback back  sha256:804c5351...9555117b != sent
  readback aux   sha256:804c5351...9555117b != sent
  engine holds the frame that was sent; presented without error,
  appearance on glass unverified

...panel tft: C0F
...panel fpl: AAB0AU
Loading waveforms from: /usr/share/remarkable/GAL3_AAB0AU_ID1C11_AC118TC1F2_AD1004-LHA_TC.eink
pmic: setting rails to 6.0, 12.0, 24.0, -6.0, -12.0, -24.0
pmic: setting PDD duration to 30000ms
temperature_hwmon: found temperature path: /sys/class/hwmon/hwmon8/temp1_input
shutdown: waiting for updates to complete...
shutdown: waiting for display to finish...

preflight   active pid 2430, failed [], /home mounted, notebooks 59, NRestarts 0
postflight  active pid 3405, failed [], /home mounted, notebooks 59, NRestarts 0
  NRestarts 0 -> 0; main start 13:40:02 -> 13:49:35
  /tmp/epframebuffer.lock names xochitl pid 3405 after 313ms
  stock is where it was found, and started once
EXIT=0
```

Zero `another instance is already running` lines for the whole boot, across both
sessions.

### The rails came on for the update, and went off for the hold

The `t+0s` sample lands inside the waveform: connector `enabled`, `dpms=On`, and
every EPD rail on — VCOM enabled, VPOS/VNEG at 6, 12 and 24 V, VGH, VGL and VPDD
on. That is the engine's own `pmic: setting rails to 6.0, 12.0, 24.0, -6.0,
-12.0, -24.0` seen from sysfs rather than from its log line.

By `t+15s` the connector is `disabled` and every rail is off, and it stays that
way. That is what a correct e-ink hold looks like: the panel keeps the image
with no power, and the rails are driven only while a waveform runs. Panel
temperature falls 36C to 31C across the hold.

This is the closest thing to positive evidence available without a camera: the
high-voltage rails were genuinely driven for the duration of one update, at the
moment a full-panel update was requested.

### The readback, and which buffer the engine actually uses

Asked for on the issue: digest what was sent, digest what the engine holds,
compare, and report both. `paperclip_ep_readback` does that for all three
planes, reading through `QImage::constBits()` so the measurement cannot detach
the image and change what it is measuring.

**`front` came back byte-identical to the frame that was sent.** `back` and
`aux` both came back as `804c5351…`, which is the digest of an all-white
1620x2160 ARGB8888 buffer — computed independently on the Mac, and exactly what
`paperclip_ep_open` fills all three planes with. So the engine presents from
`front`, the plane the caller already draws into, and the `UNVERIFIED` note that
has sat in `paperclip_ep.cpp` since WWW-3 is now answered. ADR-0009 records it.

> **Withdrawn (WWW-29).** The conclusion is right and the reasoning was not.
> `paperclip_ep_buffer` returned `QImage::bits()`, which detaches a shared
> image, so by the time anything was drawn `front` was a private copy the
> engine had never seen. This readback compared that copy against itself: it
> would have matched however the engine behaved, and `back` and `aux` staying
> white said only that nothing wrote to them. Which plane the engine presents
> from was settled instead by disassembling `setBuffers`. The bridge now caches
> the pixel address before `setBuffers` and refuses a detached front readback
> with `PAPERCLIP_EP_DETACHED`, which is what makes the paragraph below true
> rather than merely plausible.

**What the match supports.** The engine accepted our pixels and is holding them.
That rules out the one failure otherwise invisible from inside the process — a
silent fallback reporting success over a blank or substituted frame.

**What it does not.** Anything about photons. The claim string reflects exactly
this and weakens itself automatically if no plane matches:

| | claim |
|---|---|
| no vendor engine | not presented: the frame went to a memory panel |
| presented, no plane matches | presented without error, but no engine buffer holds the frame that was sent |
| presented, `front` matches | engine holds the frame that was sent; presented without error, appearance on glass unverified |

**The buffer was a real render of the Home shelf**, through the same `Screens`
code `paperctl screenshot` and `paperctl preview` use. There is no fallback
path: a screen that will not render fails the command, and a frame that
rasterises to bare background refuses the takeover before Xochitl is stopped.
Both processes render it independently and the parent refuses a report whose
digest disagrees with its own.

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

- **Nothing here has seen the glass.** The readback closes the gap between "the
  API did not error" and "the engine holds our image", and that is as far as it
  goes. Photons need a camera.
- **`held` is reported from the plan, not measured.** With suspends stretching
  `thread::sleep` that is the wrong number to print — the 13:29Z session's
  120-second hold took several wall-clock minutes and said `held 120s`. Measure
  it against `SystemTime`, which does not stop across a suspend.
- **Why the wakelock does not prevent autosleep.** Above. Until that is
  understood, a hold longer than a minute or so is not reliable, and neither is
  `DetachedWatchdog`'s deadline.
- **`--hold` has no upper bound and no resume handling.** A session that
  suspends mid-hold resumes with the bridge FPGA reprogrammed and no re-present.
