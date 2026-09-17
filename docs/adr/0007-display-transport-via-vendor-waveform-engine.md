# ADR-0007 — Presentation goes through the vendor waveform engine, not raw DRM

**Status:** accepted (Stage 2, WWW-20). Supersedes the display half of ADR-0006's
A3 caveat.

## Context

WWW-20 set out to identify the framebuffer packing so `platform/device` could
present pixels through DRM/KMS directly. It established two things on hardware,
and then spent three sessions of Calum's physical attention failing to establish
a third.

Confirmed, and still true:

- DRM master is exclusive; stopping `xochitl.service` frees it, and release back
  to stock is clean.
- The in-kernel `remarkable-cumulus-panel` driver performs the whole EPD
  high-voltage power sequence on CRTC enable — `VCOM` goes from `disabled`/0
  users to `enabled`/1 user, every other rail gains a consumer. No Qt involved.
- A non-Qt process therefore *drives the panel electrically*, and the effect is
  visible: Calum photographed artifacts on the glass.

Not established, and the reason this ADR exists: **no pattern ever rendered
legibly.** Every photograph showed the stock UI still intact with faint bands or
hairlines over it. A requested full-width horizontal bar came back as a vertical
bar down the left edge — not a wrong guess among several packings, but what
writing linearly into a non-linear buffer looks like.

The premise was wrong. The DRM connector advertises `405x1084`, and
`405 x 4 = 1620` invited the reading that the panel is 4bpp with two rows packed
per framebuffer row. It is not.

## What the prior art says

Searching before theorising would have cost one query and saved three sessions.
Two working third-party projects target this exact device.

[**rmweb**](https://github.com/exp78/rmweb) — a WebKit browser for the Paper
Pro. Its [`docs/device-profile.md`](https://github.com/exp78/rmweb/blob/master/docs/device-profile.md)
states that the panel is E Ink Gallery 3, **1620 x 2160 in 32-bit ARGB8888**;
that the DRM mode is a *packed transport*; that the `405x1084 -> 1620x2160`
packing is **"undocumented / not publicly reverse-engineered"** and lives inside
closed `libepaper.so` plus the kernel panel driver and DTS; and, in as many
words, "this is why we avoid direct DRM at first." Its pipeline is
`CPU-render -> epaper QPA -> e-ink`.

[**quill**](https://github.com/MaximeRivest/quill) — MIT-licensed, clean-room,
described as a takeover display host giving "direct e-ink engine access with a
stable C ABI". It stops xochitl and drives `libqsgepaper`'s `EPFramebuffer`,
and calls itself the lowest-latency third-party path "short of reverse-
engineering the FPGA transport frame format" — which nobody, including its
author, has done.

So the packing is not an open gate we can close with more photographs. It is
closed firmware that no third party has opened.

## Verified on our own device

Both libraries are present on image `20260827113527`:

- `/usr/lib/plugins/platforms/libepaper.so` — the Qt QPA platform plugin, and
  what `xochitl` actually maps.
- `/usr/lib/plugins/scenegraph/libqsgepaper.so` — exports the `EPFramebuffer`
  ABI. `xochitl` does **not** map this one.

`libqsgepaper.so` exports, among others:

```
EPFramebuffer::instance()
EPFramebuffer::setBuffers(std::tuple<QImage, QImage>, QImage*)
EPFramebuffer::swapBuffers(QRect, EPScreenMode, QFlags<UpdateFlag>)
EPFramebuffer::swapBuffers(const QRegion&, const EPScreenModeMap&, QFlags<UpdateFlag>)
EPFramebuffer::ghostControl(EPFramebuffer::GhostControlMode)
EPFramebuffer::checkLockFile()
EPFramebuffer::handleCrash()
```

with backends `EPFramebufferAcep2` and `EPFramebufferSwtcon`, a `WaveformTable`
class, and the strings `"Loading waveforms from: %s"`, `"Using user defined
waveform file."` and `"Failed to lock epframebuffer. Is there another
EPFramebuffer instance?"`.

Three things follow directly. The ABI is **C++ with Qt types**, not C — which is
why Quill wraps it. There is an explicit **`ghostControl`**, so ghosting is the
engine's concern and not something we hand-roll. And `checkLockFile` is the
advisory lock WWW-1 found, confirming the Host must participate in that registry
rather than merely hold DRM master.

Licence: `/usr/share/common-licenses/libqsgepaper/recipeinfo` records
`LICENSE: CLOSED`. Proprietary — linkable on the device, not redistributable, so
it can never be vendored into this repository.

## Decision

`platform/device` presents through the **vendor waveform engine**, not raw DRM.

Primary path: link `libqsgepaper.so`'s `EPFramebuffer` behind a narrow C ABI in
`platform/device/native`, exposing a pixel buffer plus a swap taking a rectangle
and a waveform mode — the shape §9 already specifies, and the shape Quill
arrived at independently. Quill is MIT, so borrowing the *interface shape* is
permitted; this ADR is the provenance record. No Quill code is copied.

Fallback: the `epaper` QPA plugin with ARGB8888 through Qt, as rmweb does.
Simpler, less waveform control. Adopt if the `EPFramebuffer` ABI proves unstable
across firmware — it is a C++ ABI from a closed library, so that risk is real.

The `405x1084 <-> 1620x2160` packing is **explicitly out of scope**. We do not
reverse-engineer it.

## Consequences

- `Canvas` produces 1620 x 2160 ARGB8888, which is what the engine wants. The
  "pack to 4bpp" conversion ADR-0006 said had no owner **does not exist** and
  should not be written. Note the aux buffer byte order Quill reports is
  `B, G, R, 0xFF`; confirm on hardware before trusting it.
- Grey levels are not limited to 16. This is a colour panel with waveform modes;
  the palette question becomes which mode to use, not how to quantise to 4 bits.
- WWW-3 needs `libqsgepaper.so` pulled from the device and a cross-compilation
  setup against the reMarkable SDK, because we must link a proprietary aarch64
  C++ library built against Qt 6.10.3.
- A takeover session must hold a wakelock on `/sys/power/wake_lock`. WWW-20
  found the mechanism; Quill supplies the reason — a mid-session suspend
  otherwise resumes into a *second* xochitl instance contending for the panel.
  That makes it a correctness requirement, not a latency optimisation.
- Keep `full=0` for partial updates, or the vendor backend escalates a small
  rectangle to a whole-panel refresh.
- No hardware alpha: preblend transparency in software. Consistent with
  ADR-0002, which the rmweb profile independently corroborates — the i.MX8M Mini
  has a Vivante GC7000 UltraLite, but stock ships no driver, so CPU rasterisation
  is the only option rather than merely the chosen one.

## What would make this wrong

- If `libqsgepaper.so`'s C++ ABI turns out to be unusable from a non-Qt process,
  or unstable across a firmware update. The fallback covers this.
- If someone does reverse-engineer the FPGA transport format. Then raw DRM
  becomes available again and would remove a proprietary dependency — but it is
  not a v1 undertaking.

## Provenance

Prior art consulted 2026-09-17: `exp78/rmweb` (`docs/device-profile.md`) and
`MaximeRivest/quill` (MIT). Device facts above were verified independently on
Calum's tablet rather than taken on trust.
