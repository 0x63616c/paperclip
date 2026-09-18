# Domain glossary

Terms this codebase already uses with a specific meaning. Written down so a
reader — human or agent — does not have to reconstruct one from five files at
once. Each entry cites where the concept is decided, not every place it is
used.

**Stock** — the tablet's own reMarkable software (Xochitl), as it ships from
the factory. Paperclip's standing requirement is that stock remains usable and
recoverable at all times; `paperctl stock` is the independent recovery path
back to it (`platform/device/src/stock.rs`).

**Takeover** — the act of switching the display from stock to a Paperclip
session and back, as one ordered, safety-critical operation. Owned by a single
type, `paper_device::takeover::Takeover`, because the order of acquiring and
releasing display ownership is "the whole of the safety argument" (ADR-0011).

**Hold** — presenting one screen on the panel and keeping it there for a
duration, as opposed to an interactive session. `paper_device::hold` runs a
takeover, one present, a timed hold, and a clean release, and reports what
that hold may and may not claim about the pixels it produced
(`platform/device/src/hold.rs`).

**Session** — a running foreground program (Home, Chess, the App Store, or a
loopback dev session) that the host supervises. Distinct from a *takeover*,
which only concerns display ownership; a session is what runs once the
takeover has the display (`platform/host/src/state.rs`, `tools/paperctl/src/session.rs`).

**Supervisor** — the host state machine (`paper-host`) that makes a custom
foreground session recoverable: it enforces deadlines, tracks failures, and
guarantees a path back to stock even if the session or the machine underneath
it fails (`platform/host/src/lib.rs`, §10).

**Foreground** — whichever of {stock, a specific session} currently owns the
display. `Machine::foreground` is a single authoritative value; nothing else
in the supervisor decides who owns the screen
(`platform/host/src/state.rs`).

**Readiness rung** — how far a supervised process has gotten, published to its
status file as it reaches each one (`process` → `control` → `protocol` →
`device-adapter` → `home` → `ready`). Exists because process startup alone —
"the unit has a pid" — is not health evidence; the rung says how far past that
a supervisor got before it stopped getting further
(`platform/host/src/readiness.rs`, §13).

**Settle** — an e-ink panel reaching a stable, non-transitional visual state
after a waveform finishes driving it. `Panel::clear` "drives the panel white
and waits for the waveform to settle"; the distinction between a call's
*latency* and a waveform's *settle time* is made explicit wherever a hold
reports timings, because they are not the same number
(`platform/device/src/panel.rs`, `platform/device/src/hold.rs`).

**Release** (as in package release) — one signed, versioned unit of an app or
of the platform itself: a release descriptor names a version and the archive
that carries it, with that archive's exact size and digest
(`platform/packages/src/release.rs`, §12). Not to be confused with *release*
as in `Takeover::release` (giving the display back) — the two are unrelated
uses of the same English word, disambiguated by context in the code the same
way this glossary just did.

**Install transaction** — the ordered, durable sequence that turns a verified
release into an installed app or platform version: validate metadata, stage
into a bounded scratch directory, verify signature and structure, extract
executing nothing, commit the complete directory durably (marker written
last, tree fsynced, then renamed), and activate only when the target is
stopped (`platform/packages/src/install.rs`, §12).

**Capability** — a named permission an app can be granted (e.g. storage,
network). **Grant** — the act of associating capabilities with an installed
app, performed exclusively by the host's own install policy and never by
anything a package declares about itself: a `paper.toml` has no capability
field and rejects one as a parse error if present
(`platform/packages/src/capability.rs`, §11 — "an app cannot grant itself
anything").

**Damage** — the region of the screen an app claims actually changed after an
update, as opposed to the whole panel. Defaults to `Damage::Full`; Sudoku is
the worked example of an app claiming a single cell instead
(`docs/app-contract.md`, ADR-0021).

**Waveform** — the sequence of voltage frames the vendor engine drives an
e-ink pixel through to reach a new state; not a value latched from a
framebuffer. Different waveforms trade speed for fidelity, and choosing one is
the single knob with the largest effect on both
(`platform/device/src/waveform.rs`).

**Gesture** — a single pointer's motion, tracked through its phases (begin,
move, end) and, only for the touchscreen, one of potentially several
concurrent contacts. The pen and the preview's mouse are always exactly one
contact, so an app that does not care about multitouch can ignore contact ids
entirely (`platform/protocol/src/input.rs`).

**System gesture** — a gesture the compositor claims for itself rather than
passing to the focused app: the **escape pinch** (two touch contacts closing
together) and an **edge swipe** (a contact that began near one of the
panel's four edges and has since travelled inward past a threshold).
Confirmation is spatial, not time-based — `PointerEvent` carries no
timestamp — so a long press is simply a contact that never crosses that
movement threshold, not a separately timed gesture
(`platform/compositor/src/gesture.rs`, ADR-0034).

**Run log** — the record `paperctl open` and `paperctl deploy` write for each
attempt, read back by `paperctl logs`. Distinct from a systemd journal: it is
Paperclip's own record of what a Mac-initiated run against the tablet did,
kept next to the device config so `paperctl doctor` and `paperctl logs` have
something to read even with no device reachable right now
(`tools/paperctl/src/transport/runlog.rs`, WWW-34).

**Boot counter** — the durable, power-loss-surviving count of consecutive
boots that failed to reach `SessionState::Home`, distinct from a systemd
restart count (which resets every boot) and from the platform updater's own
one-attempt grading (which answers "did this transaction commit", not "does
this boot"). Three failures and autostart stops trying until reset
(`platform/boot/src/counter.rs`, ADR-0008's WWW-53 amendment).

**Autostart** — starting Paperclip at boot without the Mac, through the one
unit in this project installed to the device's root filesystem rather than
generated at session start (`paperclip-launcher.service`). Bounded by the
*boot counter* above; disabled with `paperctl autostart disable` over SSH,
which is also the boot-time skip's other half
(`platform/boot/src/units.rs`, ADR-0008's WWW-53 amendment).
