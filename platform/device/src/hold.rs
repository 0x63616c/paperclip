//! Present one screen on the panel, hold it, and give the display back.
//!
//! WWW-3 proved every piece of this separately — the takeover ordering, the
//! wakelock, the vendor engine, the clear-on-release, the health comparison —
//! in an `example` that also measured four waveforms and read input. This is
//! the same sequence with the experiments removed and the evidence kept, so
//! that something above it can be a single command a person runs (§4).
//!
//! What it does, in order:
//!
//! 1. Read stock's health, and refuse unless it is healthy and has restarts to
//!    spare.
//! 2. Digest the frame **before** taking the display, so a blank or
//!    unexpectedly-changed buffer is caught while the tablet is still stock's.
//! 3. Wakelock, then [`Takeover`](crate::takeover::Takeover), then the vendor panel.
//! 4. One full present, then sample sysfs through the hold.
//! 5. Clear, release, re-read health, and check the vendor registry names
//!    Xochitl again.
//!
//! ## What a report from here may and may not claim
//!
//! It may claim the engine accepted the buffer, what sysfs said while it was
//! up, and that stock came back with `NRestarts` unmoved. It may **not** claim
//! the image was right: nothing in this process can see the glass, and a wrong
//! byte order looks identical from this side (§17). [`HoldReport`] is worded
//! so that a caller printing it verbatim cannot accidentally overclaim.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use paper_sdk::{Canvas, Size};
use sha2::{Digest as _, Sha256};

use crate::error::DeviceError;
use crate::panel::{Panel, Plane, present};
use crate::waveform::{PixelRect, Refresh, Waveform};

/// How long the panel is held by default, which is how long a person has to
/// pick the tablet up and look at it.
pub const DEFAULT_HOLD: Duration = Duration::from_secs(60);

/// How long sysfs is sampled apart during a hold.
pub const DEFAULT_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// Slack between the end of the hold and the out-of-process watchdog firing.
///
/// Wide enough that a slow open, a slow first waveform and a slow clear cannot
/// race it: the watchdog starting Xochitl underneath a live session is two
/// processes contending for the panel, which is worse than the hang it guards.
pub const WATCHDOG_MARGIN: Duration = Duration::from_secs(120);

/// How long to wait, after stock is back, for Xochitl to reclaim the vendor
/// registry. It writes `/tmp/epframebuffer.lock` early in its own startup, but
/// "early" is not "before `systemctl start` returns".
pub const REGISTRY_RECLAIM_TIMEOUT: Duration = Duration::from_secs(15);

/// What to present, how, and for how long.
#[derive(Debug, Clone)]
pub struct HoldPlan {
    /// How long the image stays on the panel before it is cleared.
    pub hold: Duration,
    /// The waveform the present uses.
    pub waveform: Waveform,
    /// Whether that present is a full flash.
    pub refresh: Refresh,
    /// How often sysfs is read during the hold.
    pub sample_every: Duration,
}

impl Default for HoldPlan {
    fn default() -> Self {
        Self::first_light()
    }
}

impl HoldPlan {
    /// The plan for putting a static screen in front of a person.
    ///
    /// [`Waveform::UI`] because the shelf is greyscale line art and settled
    /// UI is exactly what mode 3 is for (WWW-35 corrected the name; it was
    /// never a mono-specific table); [`Refresh::Full`] because this is the
    /// **first** paint over whatever stock had on the panel, and a partial
    /// update leaves the previous image ghosting under it. Every *subsequent*
    /// present in a session should drop back to [`Refresh::Partial`] — the
    /// vendor backend escalates a full update to the whole panel however small
    /// the rectangle, so `full=1` is a cost paid deliberately, once.
    pub fn first_light() -> Self {
        Self {
            hold: DEFAULT_HOLD,
            waveform: Waveform::UI,
            refresh: Refresh::Full,
            sample_every: DEFAULT_SAMPLE_INTERVAL,
        }
    }

    /// How long the watchdog is given before it restores stock itself.
    pub fn watchdog_budget(&self) -> Duration {
        self.hold.saturating_add(WATCHDOG_MARGIN)
    }

    /// The sample instants inside the hold, first one at zero.
    fn sample_schedule(&self) -> Vec<Duration> {
        let step = self.sample_every.max(Duration::from_millis(100));
        let mut at = Duration::ZERO;
        let mut schedule = vec![at];
        while at + step < self.hold {
            at += step;
            schedule.push(at);
        }
        schedule
    }
}

/// A fingerprint of the exact pixels handed to the engine.
///
/// The point is comparison across runs: "the morning run presented the same
/// bytes as the night run" is checkable, where "it looked the same" is not.
/// Hashed as the ARGB8888 the panel is given, little-endian, row-major — the
/// buffer, not the canvas's internal representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameDigest {
    digest: [u8; 32],
    size: Size,
    ink_per_mille: u32,
}

impl FrameDigest {
    /// Digests what [`present`] would copy into the panel buffer.
    pub fn of(canvas: &Canvas) -> Result<Self, DeviceError> {
        let size = canvas.size();
        let pixels = (size.width as usize)
            .checked_mul(size.height as usize)
            .ok_or_else(|| DeviceError::unexpected("canvas dimensions overflow a digest"))?;
        let mut buffer = vec![0u32; pixels];
        if !canvas.fill_argb8888(&mut buffer) {
            return Err(DeviceError::unexpected(
                "the canvas would not fill an ARGB8888 buffer of its own size",
            ));
        }
        Self::of_argb(&buffer, size, canvas.ink_coverage())
    }

    /// Digests a raw ARGB8888 buffer — the form a readback comes back in.
    ///
    /// `stride` is taken from `pixels.len() / size.height`, because a plane may
    /// be padded and the padding is not part of the picture: hashing it would
    /// make a readback incomparable with a canvas that has none.
    pub fn of_argb(pixels: &[u32], size: Size, ink: f32) -> Result<Self, DeviceError> {
        let rows = size.height as usize;
        let width = size.width as usize;
        if rows == 0 || width == 0 || pixels.len() < rows * width {
            return Err(DeviceError::unexpected(format!(
                "a {}x{} frame needs at least {} pixels, got {}",
                size.width,
                size.height,
                rows * width,
                pixels.len()
            )));
        }
        let stride = pixels.len() / rows;
        let buffer = pixels;
        let mut hasher = Sha256::new();
        // Row at a time: 3.5 million single-word updates is measurably slower
        // than 2160 row-sized ones, and the tablet is not a fast machine.
        let mut row = Vec::with_capacity(width * 4);
        for line in buffer.chunks(stride) {
            row.clear();
            for pixel in &line[..width] {
                row.extend_from_slice(&pixel.to_le_bytes());
            }
            hasher.update(&row);
        }
        Ok(Self {
            digest: hasher.finalize().into(),
            size,
            ink_per_mille: (ink * 1000.0).round().max(0.0) as u32,
        })
    }

    /// The digest, written the way §12 writes every other digest.
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.digest {
            out.push(nibble(byte >> 4));
            out.push(nibble(byte & 0x0f));
        }
        out
    }

    /// The extent the digest covers.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Non-background pixels per thousand.
    ///
    /// A cheap answer to "was that a real screen or an empty buffer": the Home
    /// shelf is line art on white and lands in the tens, a blank canvas is
    /// exactly zero, and a byte-order accident that filled the buffer with
    /// black would be a thousand.
    pub fn ink_per_mille(&self) -> u32 {
        self.ink_per_mille
    }

    /// Whether these pixels could plausibly be a drawn screen.
    ///
    /// Used to refuse a takeover rather than present a void, which is the one
    /// failure that would otherwise be indistinguishable from a working
    /// session in every log this process can write. Rejects frames with less
    /// than 1% ink to catch obviously wrong buffers (noise, glitches, or
    /// uninitialized memory). Real screens are at least 13% ink (Settings);
    /// this threshold is conservative to avoid rejecting legitimate screens
    /// while catching low-coverage anomalies.
    pub fn looks_drawn(&self) -> bool {
        self.ink_per_mille >= 10
    }
}

impl fmt::Display for FrameDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sha256:{} ({}x{}, ink {}/1000)",
            self.to_hex(),
            self.size.width,
            self.size.height,
            self.ink_per_mille
        )
    }
}

fn nibble(value: u8) -> char {
    char::from_digit(u32::from(value), 16).unwrap_or('?')
}

/// One read of the kernel's opinion of the display, taken during a hold.
///
/// Every field is whatever the file actually held, or `None` when the file was
/// not there. Nothing here is interpreted: a sample that could not find the
/// connector says so rather than reporting a default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplaySample {
    /// How far into the hold this was taken.
    pub at: Duration,
    /// The connector directory these values came from.
    pub connector: Option<PathBuf>,
    /// `/sys/class/drm/<connector>/enabled`.
    pub enabled: Option<String>,
    /// `/sys/class/drm/<connector>/status`.
    pub status: Option<String>,
    /// `/sys/class/drm/<connector>/dpms`.
    pub dpms: Option<String>,
    /// Everything currently holding `/sys/power/wake_lock`.
    pub wake_locks: Option<String>,
    /// EPD regulator rails, as `name=state`.
    pub rails: Vec<String>,
    /// Panel temperature in milli-degrees, as the engine's own hwmon reports it.
    pub temperature_milli_c: Option<i32>,
}

impl fmt::Display for DisplaySample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let show = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_owned());
        write!(
            f,
            "t+{:>3}s enabled={} status={} dpms={} wake_lock=[{}]",
            self.at.as_secs(),
            show(&self.enabled),
            show(&self.status),
            show(&self.dpms),
            show(&self.wake_locks)
        )?;
        if !self.rails.is_empty() {
            write!(f, " rails=[{}]", self.rails.join(" "))?;
        }
        if let Some(milli) = self.temperature_milli_c {
            write!(f, " panel={}.{}C", milli / 1000, (milli.abs() % 1000) / 100)?;
        }
        Ok(())
    }
}

/// Where a [`DisplaySample`] looks.
///
/// A struct rather than constants so a test can point it at a fake sysfs, and
/// so the connector is discovered once instead of on every sample.
#[derive(Debug, Clone)]
pub struct DisplayProbe {
    connector: Option<PathBuf>,
    wake_lock: PathBuf,
    regulators: PathBuf,
    hwmon: PathBuf,
}

impl Default for DisplayProbe {
    fn default() -> Self {
        Self::device()
    }
}

impl DisplayProbe {
    /// The real paths on the tablet.
    pub fn device() -> Self {
        Self::rooted(Path::new("/"))
    }

    /// The same layout under another root, which is how this is tested.
    pub fn rooted(root: &Path) -> Self {
        Self {
            connector: find_connector(&root.join("sys/class/drm")),
            wake_lock: root.join("sys/power/wake_lock"),
            regulators: root.join("sys/class/regulator"),
            hwmon: root.join("sys/class/hwmon"),
        }
    }

    /// The connector this probe found, if any.
    pub fn connector(&self) -> Option<&Path> {
        self.connector.as_deref()
    }

    /// Reads everything, now.
    pub fn sample(&self, at: Duration) -> DisplaySample {
        let read = |name: &str| {
            self.connector
                .as_ref()
                .and_then(|dir| trimmed(&dir.join(name)))
        };
        DisplaySample {
            at,
            connector: self.connector.clone(),
            enabled: read("enabled"),
            status: read("status"),
            dpms: read("dpms"),
            wake_locks: trimmed(&self.wake_lock),
            rails: self.rails(),
            temperature_milli_c: self.temperature(),
        }
    }

    /// EPD rails as the regulator class reports them.
    ///
    /// The vendor engine logs "setting rails to 6.0, 12.0, 24.0, -6.0, -12.0,
    /// -24.0" (WWW-3), and this is where those live: `VPOS1..3`, `VNEG1..3`,
    /// `VGH1/2`, `VGL`, `VCOM`, `VPDD`, all consumed by `cumulus-panel`.
    ///
    /// Picked by **consumer**, not by name. Each regulator directory holds a
    /// `consumer:platform:cumulus-panel` symlink, which is the kernel's own
    /// answer to "is this rail the panel's"; a name-matching guess would both
    /// miss `VPDD` and, on some other tablet, catch something that is not a
    /// rail. Observed on image `20260827113527`.
    ///
    /// Reported verbatim, including the gaps: most of these rails export an
    /// empty `state` because the driver implements no `is_enabled`, so the
    /// value falls back to `status` and then to `?`. An empty field is not a
    /// rail that is down.
    fn rails(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.regulators) else {
            return Vec::new();
        };
        let mut rails: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let dir = entry.path();
                let name = trimmed(&dir.join("name"))?;
                if !feeds_the_panel(&dir) {
                    return None;
                }
                let state = trimmed(&dir.join("state"))
                    .or_else(|| trimmed(&dir.join("status")))
                    .unwrap_or_else(|| "?".to_owned());
                match trimmed(&dir.join("microvolts")).and_then(|text| text.parse::<i64>().ok()) {
                    Some(microvolts) => Some(format!(
                        "{name}={state}@{}.{:03}V",
                        microvolts / 1_000_000,
                        (microvolts.abs() % 1_000_000) / 1_000
                    )),
                    None => Some(format!("{name}={state}")),
                }
            })
            .collect();
        rails.sort();
        rails
    }

    /// The panel temperature, from the hwmon the engine itself selects.
    fn temperature(&self) -> Option<i32> {
        let entries = fs::read_dir(&self.hwmon).ok()?;
        let mut candidates: Vec<(String, i32)> = entries
            .flatten()
            .filter_map(|entry| {
                let dir = entry.path();
                let value = trimmed(&dir.join("temp1_input"))?.parse().ok()?;
                let name = trimmed(&dir.join("name")).unwrap_or_default();
                Some((name, value))
            })
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        candidates
            .iter()
            .find(|(name, _)| {
                let lower = name.to_lowercase();
                lower.contains("epd") || lower.contains("eink") || lower.contains("panel")
            })
            .or_else(|| candidates.first())
            .map(|(_, value)| *value)
    }
}

/// Picks the panel connector out of `/sys/class/drm`.
///
/// LVDS first because that is what this tablet uses (`card0-LVDS-1`, WWW-1),
/// then any connected connector, so a firmware that renames it still produces
/// a sample instead of a row of dashes.
fn find_connector(drm: &Path) -> Option<PathBuf> {
    let mut connectors: Vec<PathBuf> = fs::read_dir(drm)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("status").is_file())
        .collect();
    connectors.sort();
    connectors
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("LVDS"))
        })
        .or_else(|| {
            connectors
                .iter()
                .find(|path| trimmed(&path.join("status")).as_deref() == Some("connected"))
        })
        .or_else(|| connectors.first())
        .cloned()
}

/// Whether a regulator directory names the e-paper panel among its consumers.
///
/// The kernel writes one `consumer:<device>` symlink per user of the rail, so
/// this is the device's own answer rather than a guess from the rail's name.
fn feeds_the_panel(regulator: &Path) -> bool {
    let Ok(entries) = fs::read_dir(regulator) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .map(str::to_lowercase)
            .is_some_and(|name| {
                name.starts_with("consumer:")
                    && ["panel", "cumulus", "epd", "eink"]
                        .iter()
                        .any(|needle| name.contains(needle))
            })
    })
}

fn trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// What the panel half of a session did, with nothing inferred.
#[derive(Debug, Clone)]
pub struct PanelWork {
    /// Whether these pixels could reach glass, or went into a [`MemoryPanel`].
    ///
    /// [`MemoryPanel`]: crate::panel::MemoryPanel
    pub real_panel: bool,
    /// The extent the panel reported.
    pub panel_size: Size,
    /// The frame that was sent.
    pub digest: FrameDigest,
    /// The rectangle, waveform and refresh of the one present.
    pub swap: String,
    /// How long `open` took.
    pub open: Duration,
    /// How long the present call took. **A call latency, not a settle time**
    /// (WWW-3): only the first full update appears to block at all.
    pub present: Duration,
    /// How long the image was left up.
    pub held: Duration,
    /// How long the clear took.
    pub clear: Duration,
    /// sysfs through the hold.
    pub samples: Vec<DisplaySample>,
    /// What each of the engine's buffers held once the swap had been made.
    pub readback: Vec<PlaneDigest>,
}

/// One plane, digested after the swap, next to the frame that was sent.
///
/// Asked for on WWW-23: move the claim from "the API did not error" to "the
/// engine is holding our image". The technique is the one
/// [libqsgepaper-snoop](https://github.com/pl-semiotics/libqsgepaper-snoop)
/// demonstrates — the buffers are reachable from inside the owning process —
/// minus the hooking, because we *are* the owning process and the bridge can
/// simply hand them back.
///
/// What a match buys, precisely: the engine accepted these pixels and is
/// holding them, which rules out a silent fallback presenting a blank or
/// substituted frame and reporting success. What it does not buy: anything
/// about photons. That still needs a camera (§17).
///
/// And it buys that only while the bridge's no-detach invariant holds — the
/// engine shares the front buffer, so reading it reads the engine's memory.
/// WWW-23's front match was taken before WWW-29 found the invariant broken, and
/// was therefore vacuous: this side's private copy agreeing with itself. The
/// bridge now returns `VendorStatus::Detached` rather than a digest it knows
/// is uninformative, so a `matches_sent` on `front` means what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaneDigest {
    /// Which buffer this is.
    pub plane: Plane,
    /// Its digest, or why it could not be read.
    pub digest: Result<FrameDigest, String>,
    /// Whether it equals the frame that was sent.
    pub matches_sent: bool,
}

impl fmt::Display for PlaneDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.digest {
            Ok(digest) => write!(
                f,
                "{:<5} sha256:{} {}",
                self.plane.name(),
                digest.to_hex(),
                if self.matches_sent {
                    "== sent"
                } else {
                    "!= sent"
                }
            ),
            Err(why) => write!(f, "{:<5} unreadable: {why}", self.plane.name()),
        }
    }
}

impl PanelWork {
    /// Which planes came back holding exactly what was sent.
    pub fn holding(&self) -> Vec<Plane> {
        self.readback
            .iter()
            .filter(|plane| plane.matches_sent)
            .map(|plane| plane.plane)
            .collect()
    }

    /// The honest one-line claim, and the only one this code is entitled to.
    ///
    /// Three strengths, and the difference between them is the whole point of
    /// reading the buffers back. None of them is about the glass.
    pub fn claim(&self) -> &'static str {
        if !self.real_panel {
            return "not presented: the frame went to a memory panel, not to the glass";
        }
        if self.holding().is_empty() {
            "presented without error, but no engine buffer holds the frame that was sent \
             — appearance unverified and the pixels are unaccounted for"
        } else {
            "engine holds the frame that was sent; presented without error, \
             appearance on glass unverified"
        }
    }
}

impl fmt::Display for PanelWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "panel {}x{} ({})",
            self.panel_size.width,
            self.panel_size.height,
            if self.real_panel {
                "vendor waveform engine"
            } else {
                "memory"
            }
        )?;
        writeln!(f, "  frame    {}", self.digest)?;
        writeln!(f, "  swap     {}", self.swap)?;
        writeln!(
            f,
            "  timings  open {}ms, present {}ms (call latency, not settle time), held {}s, clear {}ms",
            self.open.as_millis(),
            self.present.as_millis(),
            self.held.as_secs(),
            self.clear.as_millis()
        )?;
        for sample in &self.samples {
            writeln!(f, "  {sample}")?;
        }
        for plane in &self.readback {
            writeln!(f, "  readback {plane}")?;
        }
        write!(f, "  {}", self.claim())
    }
}

/// A [`PanelWork`] flattened so it can cross a process boundary.
///
/// It has to cross one. `EPFramebuffer` is a singleton inside
/// `libqsgepaper`, `checkLockFile()` *takes* the vendor lock rather than
/// merely reading it, and the library exposes nothing that gives it back:
/// `paperclip_ep_close` deletes our handle and our `QCoreApplication`, and the
/// engine — and its lock — outlive both. The only thing that releases it is
/// the process exiting.
///
/// So the process that presents cannot also be the process that starts
/// Xochitl, and what it learned has to come back as data rather than as a
/// return value. `lines` is the report it printed, carried verbatim so the
/// parent can reprint it without pretending to have observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelRecord {
    /// Whether those pixels could reach glass.
    pub real_panel: bool,
    /// The digest of the frame the presenter actually sent.
    pub digest: String,
    /// Whether an engine buffer came back holding exactly that frame —
    /// [`PanelWork::holding`] reduced to the one bit a caller can act on.
    ///
    /// `Option` rather than `bool` because "the readback said no" and "nobody
    /// read anything back" are different facts and a caller that exits on the
    /// first must not be handed the second dressed up as it. `None` is the
    /// honest value for a presenter that never digested the buffers — the
    /// interactive session in `paperctl run` presents many frames and reads
    /// none of them back — and for a record written by something older than
    /// this field.
    ///
    /// `Some(true)` says the engine accepted and is holding these pixels. It
    /// says nothing whatever about the glass; see [`PanelWork::claim`].
    pub holds_sent: Option<bool>,
    /// The presenter's own report, one line each.
    pub lines: Vec<String>,
}

impl PanelRecord {
    /// The record a presenter leaves behind.
    pub fn of(work: &PanelWork) -> Self {
        Self {
            real_panel: work.real_panel,
            digest: format!("sha256:{}", work.digest.to_hex()),
            // Every plane is read back on the `present_and_hold` path, so this
            // is a verdict rather than an absence: `false` here means the
            // readback ran and no buffer held the frame.
            holds_sent: Some(!work.holding().is_empty()),
            lines: work.to_string().lines().map(str::to_owned).collect(),
        }
    }

    /// Writes it where the parent will look.
    ///
    /// Deliberately a file rather than a pipe: the vendor engine writes its own
    /// diagnostics to stdout — panel lot ids, waveform table, pmic rails — and
    /// those belong in front of the operator, not parsed out of a stream.
    pub fn write(&self, path: &Path) -> Result<(), DeviceError> {
        let mut text = format!(
            "real_panel={}\ndigest={}\nholds_sent={}\n",
            self.real_panel,
            self.digest,
            match self.holds_sent {
                Some(true) => "true",
                Some(false) => "false",
                None => "unknown",
            }
        );
        for line in &self.lines {
            // The report is human text and may hold anything but a newline,
            // which is the one character the format cannot carry.
            text.push_str(&format!("line={}\n", line.replace('\n', " ")));
        }
        fs::write(path, text)
            .map_err(|source| DeviceError::io("write a panel record to", path, source))
    }

    /// Reads one back, or says why it could not.
    pub fn read(path: &Path) -> Result<Self, DeviceError> {
        let text = fs::read_to_string(path)
            .map_err(|source| DeviceError::io("read the panel record at", path, source))?;
        let mut record = Self {
            real_panel: false,
            digest: String::new(),
            holds_sent: None,
            lines: Vec::new(),
        };
        for line in text.lines() {
            match line.split_once('=') {
                Some(("real_panel", value)) => record.real_panel = value == "true",
                Some(("digest", value)) => record.digest = value.to_owned(),
                // Anything that is not the word `true` reads as unverified,
                // and a missing key as unknown: the default has to be the one
                // that makes a caller look harder, never the green one.
                Some(("holds_sent", "true")) => record.holds_sent = Some(true),
                Some(("holds_sent", "false")) => record.holds_sent = Some(false),
                Some(("holds_sent", _)) => record.holds_sent = None,
                Some(("line", value)) => record.lines.push(value.to_owned()),
                _ => {}
            }
        }
        if record.digest.is_empty() {
            return Err(DeviceError::unexpected(format!(
                "the panel record at {} names no digest; the presenter did not finish",
                path.display()
            )));
        }
        Ok(record)
    }
}

/// Draws `canvas` onto `panel`, holds it, then clears — the part that has no
/// opinion about Xochitl, systemd or wakelocks.
///
/// Split out so it can be driven against a
/// [`MemoryPanel`](crate::panel::MemoryPanel) on a Mac. That proves the
/// rectangle, the waveform and the digest; it proves nothing about the glass.
pub fn present_and_hold(
    panel: &mut dyn Panel,
    canvas: &Canvas,
    plan: &HoldPlan,
    probe: &DisplayProbe,
    real_panel: bool,
    mut sleep: impl FnMut(Duration),
) -> Result<PanelWork, DeviceError> {
    let digest = FrameDigest::of(canvas)?;
    if !digest.looks_drawn() {
        return Err(DeviceError::unexpected(
            "the frame is entirely background; refusing to present an empty panel",
        ));
    }
    let digest_hex = digest.to_hex();

    let panel_size = panel.size();
    let started = std::time::Instant::now();
    let rect = PixelRect::whole(panel_size);
    present(panel, canvas, rect, plan.waveform, plan.refresh)?;
    let present_took = started.elapsed();

    // Read back before the hold, not after: the question is what the engine
    // took from the swap, and a clear would answer a different one.
    let readback = Plane::ALL
        .iter()
        .map(|plane| {
            let digest = panel
                .readback(*plane)
                .and_then(|pixels| FrameDigest::of_argb(&pixels, panel_size, 0.0))
                .map_err(|error| error.to_string());
            let matches_sent = digest
                .as_ref()
                .is_ok_and(|read| read.to_hex() == digest_hex);
            PlaneDigest {
                plane: *plane,
                digest,
                matches_sent,
            }
        })
        .collect();

    // Sampled on a schedule rather than in a busy loop: each sample is a
    // handful of sysfs reads, and doing them ten times a second during a
    // minute-long hold is CPU the panel would rather have.
    let mut samples = Vec::new();
    let mut elapsed = Duration::ZERO;
    for at in plan.sample_schedule() {
        if at > elapsed {
            sleep(at - elapsed);
            elapsed = at;
        }
        samples.push(probe.sample(at));
    }
    if plan.hold > elapsed {
        sleep(plan.hold - elapsed);
    }

    let started = std::time::Instant::now();
    panel.clear()?;
    let clear_took = started.elapsed();

    Ok(PanelWork {
        real_panel,
        panel_size,
        digest,
        swap: format!(
            "{}x{} at ({},{}), waveform mode {} {:?}, full={}",
            rect.width,
            rect.height,
            rect.x,
            rect.y,
            plan.waveform.mode(),
            plan.waveform.content(),
            plan.refresh.full_flag()
        ),
        open: Duration::ZERO,
        present: present_took,
        held: plan.hold,
        clear: clear_took,
        samples,
        readback,
    })
}

#[cfg(target_os = "linux")]
pub use linux::{HoldReport, RegistryCheck, open_and_hold, open_and_run, present_only};

#[cfg(target_os = "linux")]
mod linux {
    use std::fmt;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime};

    use paper_sdk::Canvas;

    use super::{
        DisplayProbe, HoldPlan, PanelRecord, PanelWork, REGISTRY_RECLAIM_TIMEOUT, present_and_hold,
    };
    use crate::error::DeviceError;
    use crate::session::{EPFRAMEBUFFER_LOCK, VendorLockState, WakeLock};
    use crate::stock::{StartBudget, StockHealth, Systemctl};
    use crate::takeover::{DetachedWatchdog, Takeover, WAKELOCK_TAG};
    use crate::{Stock, is_real_device, open_panel};

    /// Whether Xochitl took the vendor registry back.
    ///
    /// The release path puts the file back as it was found, which is the fix
    /// for the double abort in WWW-3; this checks the *further* thing — that
    /// the Xochitl which then started has claimed it as its own. A `false`
    /// here with stock otherwise healthy means the next takeover should look
    /// at `/tmp/epframebuffer.lock` before it trusts anything.
    #[derive(Debug, Clone)]
    pub struct RegistryCheck {
        /// The PID the registry names now.
        pub holder_pid: Option<i32>,
        /// Xochitl's PID after the restore.
        pub xochitl_pid: Option<u32>,
        /// How long it took to agree, when it did.
        pub waited: Duration,
    }

    impl RegistryCheck {
        /// Whether the registry names the running Xochitl.
        pub fn agrees(&self) -> bool {
            match (self.holder_pid, self.xochitl_pid) {
                (Some(holder), Some(xochitl)) => holder as i64 == i64::from(xochitl),
                _ => false,
            }
        }
    }

    impl fmt::Display for RegistryCheck {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if self.agrees() {
                write!(
                    f,
                    "{EPFRAMEBUFFER_LOCK} names xochitl pid {:?} after {}ms",
                    self.holder_pid,
                    self.waited.as_millis()
                )
            } else {
                write!(
                    f,
                    "{EPFRAMEBUFFER_LOCK} names {:?}, xochitl is {:?} — NOT reclaimed within {}s",
                    self.holder_pid,
                    self.xochitl_pid,
                    REGISTRY_RECLAIM_TIMEOUT.as_secs()
                )
            }
        }
    }

    /// A whole session: preflight, takeover, present, hold, release, verify.
    #[derive(Debug, Clone)]
    pub struct HoldReport {
        /// Stock as it was found.
        pub before: StockHealth,
        /// Stock after the display was handed back.
        pub after: StockHealth,
        /// What the presenting process reported back.
        pub panel: PanelRecord,
        /// Whether the registry went back to Xochitl.
        pub registry: RegistryCheck,
        /// Health differences that mean this session did damage. Empty is the
        /// only acceptable value.
        pub regressions: Vec<String>,
    }

    impl HoldReport {
        /// Whether the tablet is where it was found.
        pub fn stock_restored_cleanly(&self) -> bool {
            self.regressions.is_empty() && self.after.is_healthy()
        }
    }

    impl fmt::Display for HoldReport {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            writeln!(f, "preflight   {:?}", self.before)?;
            for line in &self.panel.lines {
                writeln!(f, "{line}")?;
            }
            writeln!(f, "postflight  {:?}", self.after)?;
            writeln!(
                f,
                "  NRestarts {:?} -> {:?}; main start {:?} -> {:?}",
                self.before.n_restarts,
                self.after.n_restarts,
                self.before.main_start,
                self.after.main_start
            )?;
            writeln!(f, "  {}", self.registry)?;
            if self.regressions.is_empty() {
                write!(f, "  stock is where it was found, and started once")
            } else {
                write!(f, "  STOCK REGRESSED: {}", self.regressions.join("; "))
            }
        }
    }

    /// Presents on the panel and returns what it did — and **nothing else**.
    ///
    /// This is the half that opens the vendor engine, and therefore the half
    /// that must die before Xochitl is started again. It stops nothing, starts
    /// nothing, takes no wakelock and holds no opinion about systemd. Call it
    /// only from a process whose exit you control.
    pub fn present_only(canvas: &Canvas, plan: &HoldPlan) -> Result<PanelWork, DeviceError> {
        let probe = DisplayProbe::device();
        let opened = Instant::now();
        let mut panel = open_panel()?;
        let open_took = opened.elapsed();
        let mut work = present_and_hold(
            panel.as_mut(),
            canvas,
            plan,
            &probe,
            is_real_device(),
            thread::sleep,
        )?;
        work.open = open_took;
        // Closed here rather than left to the end of the function, so the
        // engine's own shutdown ("waiting for updates to complete...") has
        // happened before anything reads this result.
        drop(panel);
        Ok(work)
    }

    /// Runs a whole session on the tablet, presenting through `present`.
    ///
    /// ## Why presentation is a callback
    ///
    /// Because it has to happen in a different process, and this one must not
    /// know how that process is started.
    ///
    /// WWW-23 established the reason on hardware, expensively.
    /// `EPFramebuffer::checkLockFile()` is not a query — it *acquires* the
    /// vendor lock — and `libqsgepaper` exposes no way to release it. The
    /// engine is a singleton that outlives our handle and our
    /// `QCoreApplication`; only the process exiting frees what it holds. So a
    /// process that has opened the engine can never successfully start
    /// Xochitl: the new Xochitl reaches its own `checkLockFile()` about 1.8
    /// seconds in, finds ours, logs "another instance is already running",
    /// fails `SWTCON` and aborts — four times, into `StartLimitBurst`, and on
    /// this image `OnFailure=` reboots the tablet.
    ///
    /// WWW-3 survived this by accident: its example exited within that 1.8s
    /// window. Adding a fifteen-second registry poll after the restart turned
    /// the race into a certainty and rebooted the device. Timing was never the
    /// fix; process lifetime is.
    ///
    /// `restore_locks` therefore runs only after `present` has *returned*, and
    /// `present` is required to have reaped the presenting process before it
    /// does. Everything here — preflight, wakelock, stop, restart, verify — is
    /// in a process that never opens the engine at all.
    pub fn open_and_hold<P>(
        canvas: &Canvas,
        plan: &HoldPlan,
        present: P,
    ) -> Result<HoldReport, DeviceError>
    where
        P: FnOnce() -> Result<PanelRecord, DeviceError>,
    {
        let systemctl = Systemctl;
        let stock = Stock::new(Systemctl, StartBudget::shared());
        // `health` and `preflight` both need to leave something behind for
        // code below that runs after `Takeover::acquire` returns: the health
        // snapshot becomes `HoldReport::before`, and the digest becomes what
        // the post-release agreement check compares the presenter against.
        // Neither is recomputed — the ordering guarantee is about *when* the
        // gate runs, not about the caller reading twice.
        let mut before = None;
        let mut digest = None;
        let session = Takeover::acquire(
            stock,
            || {
                let health = systemctl.health();
                before = Some(health.clone());
                health
            },
            || {
                let computed = super::FrameDigest::of(canvas)?;
                if !computed.looks_drawn() {
                    return Err(DeviceError::unexpected(
                        "the frame is entirely background; refusing to take the display for an \
                         empty panel",
                    ));
                }
                digest = Some(computed);
                Ok(())
            },
            VendorLockState::capture,
            // Wakelock before the display, always: the window between the
            // stop and the wakelock is the window a suspend resumes into a
            // second Xochitl. It is held *here*, in the surviving process, so
            // a presenter that dies cannot take it with it.
            || WakeLock::acquire(WAKELOCK_TAG),
            DetachedWatchdog::new(),
            plan.watchdog_budget(),
            SystemTime::now(),
        )?;
        let before = before.expect("health always sets this before acquire can return Ok");
        let expected = format!(
            "sha256:{}",
            digest
                .expect("preflight always sets this before acquire can return Ok")
                .to_hex()
        );

        let record = present();

        // Release regardless of how the presentation went, and only now: the
        // presenter has returned, which is this function's guarantee that the
        // process holding the engine is gone.
        let released = Takeover::release(session, SystemTime::now());
        let record = match (record, released) {
            (_, Err(error)) => return Err(error),
            (Err(error), Ok(())) => return Err(error),
            (Ok(record), Ok(())) => record,
        };

        // The two processes rendered the frame independently. Agreeing is not
        // decoration: it is the check that the thing put on the panel is the
        // thing this process refused to take the display for if it were blank.
        if record.digest != expected {
            return Err(DeviceError::unexpected(format!(
                "the presenter sent {} but this process rendered {expected}; \
                 the two halves do not agree on what was shown",
                record.digest
            )));
        }

        let registry = wait_for_registry(&systemctl);
        let after = systemctl.health();
        let regressions = after.regressions_from(&before);
        Ok(HoldReport {
            before,
            after,
            panel: record,
            registry,
            regressions,
        })
    }

    /// Runs an interactive session: the same takeover, release and verify
    /// ordering as [`open_and_hold`], around a presenter that shows however
    /// many frames a person's taps produce instead of one static screen.
    ///
    /// The one thing this drops from [`open_and_hold`] is the digest
    /// agreement check. That check exists because `open`'s two halves render
    /// the *same* deterministic screen independently and can be compared; an
    /// interactive session's frames are chosen by whatever gets tapped, so
    /// there is no one canvas on this side to agree with. Refusing to present
    /// a blank first frame is therefore the presenter's own job — it is the
    /// half that actually has a canvas to look at.
    ///
    /// `budget` is the interactive session's own time allowance, not a
    /// display hold: it becomes the out-of-process watchdog's deadline the
    /// same way [`HoldPlan::watchdog_budget`] does, margin included, so a
    /// presenter that hangs is still bounded by a process outside it.
    pub fn open_and_run<P>(budget: Duration, present: P) -> Result<HoldReport, DeviceError>
    where
        P: FnOnce() -> Result<PanelRecord, DeviceError>,
    {
        let systemctl = Systemctl;
        let stock = Stock::new(Systemctl, StartBudget::shared());
        // `before` is captured out of the `health` closure for the same
        // reason `open_and_hold` does it — it becomes `HoldReport::before`
        // below, and must not be a second, later read of stock's health.
        let mut before = None;
        let session = Takeover::acquire(
            stock,
            || {
                let health = systemctl.health();
                before = Some(health.clone());
                health
            },
            // No digest agreement check here — see the doc comment above on
            // why an interactive session has no one canvas to compare
            // against.
            || Ok(()),
            VendorLockState::capture,
            // Wakelock before the display, always — see `open_and_hold`.
            || WakeLock::acquire(WAKELOCK_TAG),
            DetachedWatchdog::new(),
            budget.saturating_add(super::WATCHDOG_MARGIN),
            SystemTime::now(),
        )?;
        let before = before.expect("health always sets this before acquire can return Ok");

        let record = present();

        // Release regardless of how the session went, and only now: the
        // presenter has returned, which is this function's guarantee that the
        // process holding the engine is gone.
        let released = Takeover::release(session, SystemTime::now());
        let record = match (record, released) {
            (_, Err(error)) => return Err(error),
            (Err(error), Ok(())) => return Err(error),
            (Ok(record), Ok(())) => record,
        };

        let registry = wait_for_registry(&systemctl);
        let after = systemctl.health();
        let regressions = after.regressions_from(&before);
        Ok(HoldReport {
            before,
            after,
            panel: record,
            registry,
            regressions,
        })
    }

    /// Polls until the vendor registry names the running Xochitl, or gives up.
    ///
    /// Gives up quietly: this is evidence to report, not a reason to touch the
    /// tablet again. Anything this function did on a timeout would be another
    /// Xochitl start, which is the resource it is least safe to spend.
    fn wait_for_registry(systemctl: &Systemctl) -> RegistryCheck {
        let started = Instant::now();
        loop {
            let holder = crate::session::DisplayLockHolder::read_from(std::path::Path::new(
                EPFRAMEBUFFER_LOCK,
            ))
            .ok()
            .flatten();
            let check = RegistryCheck {
                holder_pid: holder.and_then(|holder| holder.pid),
                xochitl_pid: systemctl.main_pid(),
                waited: started.elapsed(),
            };
            if check.agrees() || started.elapsed() >= REGISTRY_RECLAIM_TIMEOUT {
                return check;
            }
            thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use paper_sdk::{Canvas, Rect, SCREEN, Size, palette};

    use super::{DisplayProbe, FrameDigest, HoldPlan, WATCHDOG_MARGIN, present_and_hold};
    use crate::error::DeviceError;
    use crate::panel::{MemoryPanel, Plane};
    use crate::waveform::Refresh;

    fn drawn() -> Canvas {
        let mut canvas = Canvas::new(SCREEN).expect("allocates");
        canvas.clear(palette::PAPER);
        canvas.fill_rect(Rect::new(100.0, 100.0, 400.0, 400.0), palette::INK);
        canvas
    }

    #[test]
    fn a_digest_is_stable_for_the_same_pixels_and_moves_for_different_ones() {
        let first = FrameDigest::of(&drawn()).expect("digests");
        let again = FrameDigest::of(&drawn()).expect("digests");
        assert_eq!(first, again);
        assert_eq!(first.to_hex(), again.to_hex());
        assert_eq!(first.size(), SCREEN);

        let mut other = drawn();
        other.fill_rect(Rect::new(600.0, 600.0, 10.0, 10.0), palette::INK);
        let moved = FrameDigest::of(&other).expect("digests");
        assert_ne!(first, moved);
    }

    #[test]
    fn a_blank_canvas_is_recognisably_not_a_screen() {
        let mut blank = Canvas::new(SCREEN).expect("allocates");
        blank.clear(palette::PAPER);
        let digest = FrameDigest::of(&blank).expect("digests");
        assert_eq!(digest.ink_per_mille(), 0);
        assert!(!digest.looks_drawn());
        assert!(FrameDigest::of(&drawn()).expect("digests").looks_drawn());
    }

    #[test]
    fn frames_with_minimal_ink_are_rejected_as_likely_wrong() {
        // A frame with ~0.1% ink (1 per mille) is below the threshold and
        // should be rejected to catch noise, glitches or uninitialized buffers.
        let mut almost_blank = Canvas::new(SCREEN).expect("allocates");
        almost_blank.clear(palette::PAPER);
        // A single pixel is ~0.0000037 per mille, so this is still much larger
        // than any single pixel but tiny compared to real screens (13%+).
        for y in 0..10 {
            for x in 0..10 {
                almost_blank.fill_rect(Rect::new(x as f32, y as f32, 1.0, 1.0), palette::INK);
            }
        }
        let digest = FrameDigest::of(&almost_blank).expect("digests");
        assert!(
            digest.ink_per_mille() < 10,
            "test setup should produce < 1% ink, got {}/1000",
            digest.ink_per_mille()
        );
        assert!(
            !digest.looks_drawn(),
            "frames with <1% ink should be rejected, got {}/1000",
            digest.ink_per_mille()
        );
    }

    #[test]
    fn presenting_an_empty_frame_is_refused_rather_than_swapped() {
        let mut blank = Canvas::new(SCREEN).expect("allocates");
        blank.clear(palette::PAPER);
        let mut panel = MemoryPanel::new(SCREEN);
        let plan = HoldPlan {
            hold: Duration::ZERO,
            ..HoldPlan::first_light()
        };
        let error = present_and_hold(
            &mut panel,
            &blank,
            &plan,
            &DisplayProbe::rooted(std::path::Path::new("/nonexistent")),
            false,
            |_| {},
        )
        .expect_err("refuses");
        assert!(error.to_string().contains("background"));
        assert!(panel.swaps().is_empty());
        assert_eq!(panel.clears(), 0);
    }

    #[test]
    fn the_first_paint_is_one_full_panel_swap_and_the_panel_is_cleared_after_it() {
        let mut panel = MemoryPanel::new(SCREEN);
        let plan = HoldPlan {
            hold: Duration::from_secs(60),
            sample_every: Duration::from_secs(20),
            ..HoldPlan::first_light()
        };
        let mut slept = Duration::ZERO;
        let work = present_and_hold(
            &mut panel,
            &drawn(),
            &plan,
            &DisplayProbe::rooted(std::path::Path::new("/nonexistent")),
            false,
            |duration| slept += duration,
        )
        .expect("presents");

        assert_eq!(panel.swaps().len(), 1);
        let swap = panel.swaps()[0];
        assert_eq!(swap.rect, crate::waveform::PixelRect::PANEL);
        assert_eq!(swap.refresh, Refresh::Full);
        assert_eq!(swap.waveform, crate::waveform::Waveform::UI);
        assert_eq!(panel.clears(), 1);
        // The hold is honoured in full, whatever the sampling schedule is.
        assert_eq!(slept, Duration::from_secs(60));
        assert_eq!(work.samples.len(), 3);
        assert!(!work.real_panel);
        assert_eq!(
            work.claim(),
            "not presented: the frame went to a memory panel, not to the glass"
        );
    }

    #[test]
    fn a_panel_record_survives_the_process_boundary_it_has_to_cross() {
        let mut panel = MemoryPanel::new(SCREEN);
        let plan = HoldPlan {
            hold: Duration::ZERO,
            ..HoldPlan::first_light()
        };
        let work = present_and_hold(
            &mut panel,
            &drawn(),
            &plan,
            &DisplayProbe::rooted(std::path::Path::new("/nonexistent")),
            false,
            |_| {},
        )
        .expect("presents");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("record");
        let written = super::PanelRecord::of(&work);
        written.write(&path).expect("writes");
        let read = super::PanelRecord::read(&path).expect("reads");

        assert_eq!(read, written);
        assert_eq!(read.digest, format!("sha256:{}", work.digest.to_hex()));
        assert!(!read.real_panel);
        assert_eq!(read.holds_sent, Some(!work.holding().is_empty()));
        // The report comes back verbatim, including the claim — the parent
        // reprints it rather than restating it, so it cannot drift.
        assert!(read.lines.iter().any(|line| line.contains(work.claim())));
    }

    #[test]
    fn a_presenter_that_wrote_nothing_is_an_error_rather_than_an_empty_report() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("record");
        // Killed before it finished: the file exists but names no digest.
        std::fs::write(&path, "real_panel=true\nline=opened the panel\n").expect("writes");
        let error = super::PanelRecord::read(&path).expect_err("refuses");
        assert!(error.to_string().contains("did not finish"), "{error}");

        let missing = dir.path().join("never-written");
        super::PanelRecord::read(&missing).expect_err("refuses a record that is not there");
    }

    /// A panel whose buffers do not come back holding what was written.
    ///
    /// The failure the readback exists to catch: every call returns success,
    /// and the pixels are not there.
    #[derive(Debug)]
    struct SilentFallback(MemoryPanel);

    impl crate::panel::Panel for SilentFallback {
        fn size(&self) -> Size {
            self.0.size()
        }
        fn buffer(&mut self) -> Result<crate::panel::PanelBuffer<'_>, DeviceError> {
            self.0.buffer()
        }
        fn swap(
            &mut self,
            rect: crate::waveform::PixelRect,
            waveform: crate::waveform::Waveform,
            refresh: Refresh,
        ) -> Result<(), DeviceError> {
            self.0.swap(rect, waveform, refresh)
        }
        fn readback(&self, _plane: Plane) -> Result<Vec<u32>, DeviceError> {
            let size = self.0.size();
            Ok(vec![
                0xFFFF_FFFF;
                size.width as usize * size.height as usize
            ])
        }
        fn ghost_control(
            &mut self,
            mode: crate::waveform::GhostControl,
        ) -> Result<(), DeviceError> {
            self.0.ghost_control(mode)
        }
        fn clear(&mut self) -> Result<(), DeviceError> {
            self.0.clear()
        }
    }

    fn present_once(panel: &mut dyn crate::panel::Panel) -> super::PanelWork {
        present_and_hold(
            panel,
            &drawn(),
            &HoldPlan {
                hold: Duration::ZERO,
                ..HoldPlan::first_light()
            },
            &DisplayProbe::rooted(std::path::Path::new("/nonexistent")),
            true,
            |_| {},
        )
        .expect("presents")
    }

    #[test]
    fn every_plane_is_read_back_and_compared_against_what_was_sent() {
        let mut panel = MemoryPanel::new(SCREEN);
        let work = present_once(&mut panel);

        assert_eq!(work.readback.len(), 3);
        for plane in &work.readback {
            assert!(plane.matches_sent, "{plane}");
            assert_eq!(
                plane.digest.as_ref().expect("readable").to_hex(),
                work.digest.to_hex()
            );
        }
        assert_eq!(work.holding(), Plane::ALL.to_vec());
    }

    #[test]
    fn a_panel_that_reports_success_over_the_wrong_pixels_is_caught() {
        let mut panel = SilentFallback(MemoryPanel::new(SCREEN));
        let work = present_once(&mut panel);

        assert!(work.holding().is_empty());
        assert!(
            work.readback.iter().all(|plane| !plane.matches_sent),
            "a blank readback must not match a drawn frame"
        );
        // The claim weakens by itself, so a caller printing it cannot overstate
        // what happened even if it never looks at the planes.
        assert!(work.claim().contains("unaccounted for"), "{}", work.claim());
        assert!(!work.claim().contains("engine holds"));
    }

    /// The verdict `holding()` computes has to survive being written to a file
    /// and read back by another process, because that is the only way it can
    /// reach the exit code: the presenting half cannot return a value, it can
    /// only leave a record and die (WWW-23's lock).
    ///
    /// Both directions, against the same fixture pair as
    /// `a_panel_that_reports_success_over_the_wrong_pixels_is_caught`: a
    /// truthful panel records `Some(true)`, and a panel that reports success
    /// over the wrong pixels records `Some(false)` rather than losing the
    /// finding somewhere in `lines`.
    #[test]
    fn the_readback_verdict_survives_the_process_boundary_in_both_directions() {
        let dir = tempfile::tempdir().expect("tempdir");

        let held = super::PanelRecord::of(&present_once(&mut MemoryPanel::new(SCREEN)));
        assert_eq!(held.holds_sent, Some(true));

        let lost =
            super::PanelRecord::of(&present_once(&mut SilentFallback(MemoryPanel::new(SCREEN))));
        assert_eq!(lost.holds_sent, Some(false));

        for record in [&held, &lost] {
            let path = dir.path().join("record");
            record.write(&path).expect("writes");
            assert_eq!(
                super::PanelRecord::read(&path).expect("reads").holds_sent,
                record.holds_sent
            );
        }
    }

    /// A record that names no verdict reads as unknown, never as a pass.
    ///
    /// The case that matters is an older or foreign presenter, or a record
    /// truncated after the digest: `paperctl open` exits non-zero on `None`
    /// exactly as it does on `Some(false)`, so the absent value must not
    /// default to the green one.
    #[test]
    fn a_record_with_no_verdict_is_unknown_rather_than_a_pass() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("record");
        std::fs::write(
            &path,
            "real_panel=true\ndigest=sha256:abc\nline=presented\n",
        )
        .expect("writes");
        assert_eq!(
            super::PanelRecord::read(&path).expect("reads").holds_sent,
            None
        );

        std::fs::write(&path, "digest=sha256:abc\nholds_sent=probably\n").expect("writes");
        assert_eq!(
            super::PanelRecord::read(&path).expect("reads").holds_sent,
            None,
            "anything but `true` or `false` is unknown"
        );
    }

    #[test]
    fn a_digest_ignores_stride_padding_so_a_readback_compares_with_a_canvas() {
        let size = Size::new(4, 3);
        let tight: Vec<u32> = (0..12).collect();
        // The same picture in a buffer padded to a stride of six.
        let mut padded = vec![0xDEAD_BEEFu32; 18];
        for row in 0..3 {
            padded[row * 6..row * 6 + 4].copy_from_slice(&tight[row * 4..row * 4 + 4]);
        }
        let a = FrameDigest::of_argb(&tight, size, 0.0).expect("digests");
        let b = FrameDigest::of_argb(&padded, size, 0.0).expect("digests");
        assert_eq!(a, b, "padding must not change the digest");

        FrameDigest::of_argb(&tight[..6], size, 0.0)
            .expect_err("a buffer too small for the frame is an error, not a short hash");
    }

    #[test]
    fn a_hold_never_outlives_its_watchdog() {
        let plan = HoldPlan::first_light();
        assert!(plan.watchdog_budget() > plan.hold);
        assert_eq!(plan.watchdog_budget(), plan.hold + WATCHDOG_MARGIN);
    }

    #[test]
    fn a_sample_reports_absence_rather_than_a_default() {
        let probe = DisplayProbe::rooted(std::path::Path::new("/nonexistent"));
        let sample = probe.sample(Duration::from_secs(1));
        assert!(probe.connector().is_none());
        assert!(sample.enabled.is_none());
        assert!(sample.status.is_none());
        assert!(sample.rails.is_empty());
        assert!(sample.to_string().contains("enabled=-"));
    }

    #[test]
    fn a_probe_reads_a_connector_and_a_wakelock_out_of_a_fake_sysfs() {
        let root = tempfile::tempdir().expect("tempdir");
        let drm = root.path().join("sys/class/drm/card0-LVDS-1");
        std::fs::create_dir_all(&drm).expect("mkdir");
        std::fs::write(drm.join("status"), "connected\n").expect("write");
        std::fs::write(drm.join("enabled"), "enabled\n").expect("write");
        std::fs::write(drm.join("dpms"), "On\n").expect("write");
        let power = root.path().join("sys/power");
        std::fs::create_dir_all(&power).expect("mkdir");
        std::fs::write(power.join("wake_lock"), "paperclip-takeover\n").expect("write");

        let probe = DisplayProbe::rooted(root.path());
        assert_eq!(probe.connector(), Some(drm.as_path()));
        let sample = probe.sample(Duration::ZERO);
        assert_eq!(sample.enabled.as_deref(), Some("enabled"));
        assert_eq!(sample.status.as_deref(), Some("connected"));
        assert_eq!(sample.dpms.as_deref(), Some("On"));
        assert_eq!(sample.wake_locks.as_deref(), Some("paperclip-takeover"));
    }

    #[test]
    fn rails_are_picked_by_consumer_and_reported_with_their_gaps() {
        let root = tempfile::tempdir().expect("tempdir");
        let class = root.path().join("sys/class/regulator");
        // The panel's, with no `state` — which is how most of them really are
        // on image 20260827113527.
        let vpos = class.join("regulator.7");
        std::fs::create_dir_all(&vpos).expect("mkdir");
        std::fs::write(vpos.join("name"), "VPOS1\n").expect("write");
        std::fs::write(vpos.join("state"), "\n").expect("write");
        std::fs::write(vpos.join("status"), "unknown\n").expect("write");
        std::fs::write(vpos.join("microvolts"), "6000000\n").expect("write");
        std::fs::write(vpos.join("consumer:platform:cumulus-panel"), "").expect("write");
        // Not the panel's, and named nothing like a rail-matching guess would
        // reject — this is why the filter reads consumers instead.
        let touch = class.join("regulator.9");
        std::fs::create_dir_all(&touch).expect("mkdir");
        std::fs::write(touch.join("name"), "VDD_TOUCH_3V3\n").expect("write");
        std::fs::write(touch.join("state"), "enabled\n").expect("write");
        std::fs::write(touch.join("consumer:platform:elan-touch"), "").expect("write");

        let rails = DisplayProbe::rooted(root.path())
            .sample(Duration::ZERO)
            .rails;
        assert_eq!(rails, vec!["VPOS1=unknown@6.000V".to_owned()]);
    }

    #[test]
    fn the_sample_schedule_stays_inside_the_hold() {
        let plan = HoldPlan {
            hold: Duration::from_secs(60),
            sample_every: Duration::from_secs(5),
            ..HoldPlan::first_light()
        };
        let schedule = plan.sample_schedule();
        assert_eq!(schedule.first(), Some(&Duration::ZERO));
        assert!(schedule.iter().all(|at| *at < plan.hold));
        assert_eq!(schedule.len(), 12);
    }

    #[test]
    fn a_digest_covers_the_whole_canvas_not_just_the_first_row() {
        // A hasher fed only the first row would not notice this.
        let mut late = Canvas::new(Size::new(64, 64)).expect("allocates");
        late.clear(palette::PAPER);
        let blank = FrameDigest::of(&late).expect("digests");
        late.fill_rect(Rect::new(0.0, 60.0, 4.0, 4.0), palette::INK);
        assert_ne!(blank, FrameDigest::of(&late).expect("digests"));
    }
}
