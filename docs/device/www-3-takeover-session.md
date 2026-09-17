# WWW-3 — the display round trip, on hardware

Three consecutive takeover sessions on Calum's Paper Pro, 2026-09-17 ~04:10Z,
image `20260827113527`, unattended. Stock verified healthy after every one.

## What ran

`platform/device`'s `takeover` example: preflight, wakelock, stop Xochitl,
open the vendor waveform engine, render the **real Home screen** through
`paper_sdk::Canvas` (not a test pattern — §9 wants the same UI code on both
backends), swap it four ways with timings, clear the panel, restore stock,
re-read stock's health and compare.

```
preflight:  StockHealth { active: true, pid: 37169, failed_units: [], home_mounted: true, notebooks: 54 }
wakelock held: paperclip-takeover
stock stopped; display should be free
panel 1620x2160, opened in 1224ms
  swap mono-quality full:    2179ms
  swap colour full:           188ms
  swap mono-ink partial:      136ms
  swap 300x300 partial ink:     3ms
  clear:                       86ms
stock restored: Ok(())
postflight: StockHealth { active: true, pid: 37360, failed_units: [], home_mounted: true, notebooks: 54 }
VERIFIED: stock is where it was found
--- exit 0 ---
```

Afterwards: `xochitl` active, `systemctl --failed` empty, connector back to
`disabled`, `/sys/power/wake_lock` holding only `udev.charger` — our wakelock
released cleanly. Notebook count 54 before and after, all three times.

## The engine engaged the real panel

Vendor output during the session, which is what distinguishes this from a
process that merely ran:

```
...panel tft: C0F
...panel fpl: AAB0AU
Loading waveforms from: /usr/share/remarkable/GAL3_AAB0AU_ID1C11_AC118TC1F2_AD1004-LHA_TC.eink
pmic: setting rails to 6.0, 12.0, 24.0, -6.0, -12.0, -24.0
pmic: setting PDD duration to 30000ms
temperature_hwmon: found temperature path: /sys/class/hwmon/hwmon8/temp1_input
shutdown: waiting for updates to complete...
shutdown: waiting for display to finish...
```

The EPD high-voltage rails were driven, a panel-specific waveform table was
loaded, and the engine ran its own ordered shutdown on release.

## Two guesses the device refuted

**The screen-mode encoding was wrong.** The first run printed

```
Invalid screen mode being set: 260
```

`260` is `0x100 | 4` — the bridge had been packing the content type into bit 8.
`EPScreenMode` is the mode number alone; mono and colour share one numbering.
Corrected, and the second run produced no such line.

**The waveform-table guard was panel-specific.** `preflight()` required the four
`/usr/share/remarkable/ct33_*.bin` files. Those exist on this tablet, but the
engine did not use them — it selected a `GAL3_AAB0AU_…eink` table from the panel
lot and TFT ids above. Requiring `ct33_*` by name would wrongly refuse a tablet
with a different panel. The guard now asks only that the vendor waveform
directory holds at least one non-empty candidate, and leaves the selection to
the engine.

## Reading the timings honestly

These are **API call latencies, not panel settle times**. Only the first
full-panel update appears to block: a 300×300 partial returning in 3 ms is a
queued request, not a completed refresh. The 2179 ms of the first swap probably
includes engine warm-up as well as the update itself.

So: useful as a §9 baseline for *call* cost, and explicitly not yet an answer
to "how long until the user sees it". Measuring that needs either a vendor
completion signal — `EPFramebuffer` exports a `framebufferUpdated(QRect)`
signal that we do not yet connect — or a camera.

## What this does and does not establish

**Does.** A non-Qt Rust process can take the display from stock through the
vendor waveform engine, present a real Paperclip screen, and give it back, with
stock verifiably unharmed. Three times. The takeover ordering, the wakelock, the
advisory locks, the clear-on-release and the health comparison all work on
hardware.

**Does not.** That the image on the glass is *correct*. Nothing here has seen
the panel. The engine accepted the buffers and drove the rails, which is
necessary and not sufficient — a wrong byte order or a wrong buffer choice would
look identical from this side.

Also untouched: input during a session, ghosting, memory and CPU, and behaviour
across a real suspend (the tablet was on charge, so it never suspended).

## Start budget

Three sessions consumed three of `xochitl.service`'s four starts in ten
minutes. `StartBudget` then refuses a fourth, which is why this pass stopped
here rather than running more experiments. Working as designed.
