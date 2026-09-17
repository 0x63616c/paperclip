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
//! 3. Wakelock, then [`Takeover`], then the vendor panel.
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
use crate::panel::{Panel, present};
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
    /// [`Waveform::MONO_QUALITY`] because the shelf is greyscale line art and
    /// mode 3 is the quality mono table; [`Refresh::Full`] because this is the
    /// **first** paint over whatever stock had on the panel, and a partial
    /// update leaves the previous image ghosting under it. Every *subsequent*
    /// present in a session should drop back to [`Refresh::Partial`] — the
    /// vendor backend escalates a full update to the whole panel however small
    /// the rectangle, so `full=1` is a cost paid deliberately, once.
    pub fn first_light() -> Self {
        Self {
            hold: DEFAULT_HOLD,
            waveform: Waveform::MONO_QUALITY,
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
        let mut hasher = Sha256::new();
        // Row at a time: 3.5 million single-word updates is measurably slower
        // than 2160 row-sized ones, and the tablet is not a fast machine.
        let mut row = Vec::with_capacity(size.width as usize * 4);
        for line in buffer.chunks(size.width as usize) {
            row.clear();
            for pixel in line {
                row.extend_from_slice(&pixel.to_le_bytes());
            }
            hasher.update(&row);
        }
        Ok(Self {
            digest: hasher.finalize().into(),
            size,
            ink_per_mille: (canvas.ink_coverage() * 1000.0).round().max(0.0) as u32,
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
    /// session in every log this process can write.
    pub fn looks_drawn(&self) -> bool {
        self.ink_per_mille > 0
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
    /// -24.0" (WWW-3), so the rails exist; whether *this* kernel exports them
    /// under `/sys/class/regulator` is not something a Mac can answer. An empty
    /// list means the class held nothing matching, and is reported as such
    /// rather than as rails being down.
    fn rails(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.regulators) else {
            return Vec::new();
        };
        let mut rails: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let dir = entry.path();
                let name = trimmed(&dir.join("name"))?;
                let lower = name.to_lowercase();
                if ![
                    "epd", "eink", "e-ink", "vcom", "vgh", "vgl", "vpos", "vneg", "disp",
                ]
                .iter()
                .any(|needle| lower.contains(needle))
                {
                    return None;
                }
                let state = trimmed(&dir.join("state")).unwrap_or_else(|| "?".to_owned());
                Some(format!("{name}={state}"))
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
}

impl PanelWork {
    /// The honest one-line claim, and the only one this code is entitled to.
    pub fn claim(&self) -> &'static str {
        if self.real_panel {
            "presented without error, appearance unverified"
        } else {
            "not presented: this build has no vendor engine, the frame went to memory"
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
        write!(f, "  {}", self.claim())
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

    let panel_size = panel.size();
    let started = std::time::Instant::now();
    let rect = PixelRect::whole(panel_size);
    present(panel, canvas, rect, plan.waveform, plan.refresh)?;
    let present_took = started.elapsed();

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
    })
}

#[cfg(target_os = "linux")]
pub use linux::{HoldReport, RegistryCheck, open_and_hold};

#[cfg(target_os = "linux")]
mod linux {
    use std::fmt;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime};

    use paper_sdk::Canvas;

    use super::{DisplayProbe, HoldPlan, PanelWork, REGISTRY_RECLAIM_TIMEOUT, present_and_hold};
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
        /// What the panel half did.
        pub panel: PanelWork,
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
            writeln!(f, "{}", self.panel)?;
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

    /// Runs the whole thing on the tablet.
    ///
    /// Every early return leaves stock untouched; every late one goes through
    /// [`Takeover`]'s release, which is also what `Drop` and the detached
    /// watchdog run. There is no path out of this function that leaves the
    /// display taken.
    pub fn open_and_hold(canvas: &Canvas, plan: &HoldPlan) -> Result<HoldReport, DeviceError> {
        let systemctl = Systemctl;
        let before = systemctl.health();
        if !before.is_healthy() {
            return Err(DeviceError::unexpected(format!(
                "stock is not healthy before the session ({before:?}); refusing to take the display"
            )));
        }
        before.allows_takeover()?;

        // Digested before anything is stopped. A frame that is not worth
        // presenting must cost the tablet nothing at all.
        let digest = super::FrameDigest::of(canvas)?;
        if !digest.looks_drawn() {
            return Err(DeviceError::unexpected(
                "the frame is entirely background; refusing to take the display for an empty panel",
            ));
        }

        let locks = VendorLockState::capture()?;
        let probe = DisplayProbe::device();

        // Wakelock before the display, always: the window between the stop and
        // the wakelock is the window a suspend resumes into a second Xochitl.
        let wake = WakeLock::acquire(WAKELOCK_TAG)?;
        let stock = Stock::new(Systemctl, StartBudget::shared());
        let session = match Takeover::acquire(
            stock,
            wake,
            DetachedWatchdog::new(),
            locks,
            plan.watchdog_budget(),
            SystemTime::now(),
        ) {
            Ok(session) => session,
            Err((error, _wake)) => return Err(error),
        };

        // The panel is opened, used and dropped inside this block, so it is
        // always closed before the release runs — including when the present
        // fails and `work` is an error.
        let work = {
            let opened = Instant::now();
            match open_panel() {
                Err(error) => Err(error),
                Ok(mut panel) => {
                    let open_took = opened.elapsed();
                    present_and_hold(
                        panel.as_mut(),
                        canvas,
                        plan,
                        &probe,
                        is_real_device(),
                        thread::sleep,
                    )
                    .map(|mut work| {
                        work.open = open_took;
                        work
                    })
                }
            }
        };

        // Release regardless of how the panel went. Reported second, because a
        // failed restore outranks a failed present.
        let released = Takeover::release(session, SystemTime::now());
        let work = match (work, released) {
            (_, Err(error)) => return Err(error),
            (Err(error), Ok(())) => return Err(error),
            (Ok(work), Ok(())) => work,
        };

        let registry = wait_for_registry(&systemctl);
        let after = systemctl.health();
        let regressions = after.regressions_from(&before);
        Ok(HoldReport {
            before,
            after,
            panel: work,
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
    use crate::panel::MemoryPanel;
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
        assert_eq!(swap.waveform, crate::waveform::Waveform::MONO_QUALITY);
        assert_eq!(panel.clears(), 1);
        // The hold is honoured in full, whatever the sampling schedule is.
        assert_eq!(slept, Duration::from_secs(60));
        assert_eq!(work.samples.len(), 3);
        assert!(!work.real_panel);
        assert_eq!(
            work.claim(),
            "not presented: this build has no vendor engine, the frame went to memory"
        );
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
