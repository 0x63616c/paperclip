//! The two things a takeover session must hold, and the one it must inspect.
//!
//! None of this presents a pixel. It is the part of §9 that decides whether a
//! session that *did* present pixels can be trusted to give the tablet back.
//!
//! Stopping and restarting `xochitl.service` is deliberately **not** here —
//! that is the host state machine's, WWW-4's. What is here are the primitives
//! that outlive any particular orchestration: a wakelock with a destructor, and
//! an honest reading of the vendor's advisory display locks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::DeviceError;

/// Where the kernel takes a wakelock.
pub const WAKE_LOCK: &str = "/sys/power/wake_lock";
/// Where the kernel releases one.
pub const WAKE_UNLOCK: &str = "/sys/power/wake_unlock";
/// The vendor's zero-length `flock` target for the EPD.
pub const EPD_LOCK: &str = "/tmp/epd.lock";
/// The vendor's advisory registry naming whoever holds `EPFramebuffer`.
pub const EPFRAMEBUFFER_LOCK: &str = "/tmp/epframebuffer.lock";

/// How long after a resume the panel cannot be presented to.
///
/// The FPGA display bridge loses its configuration in suspend and the kernel
/// reprograms it on every resume — measured at 73–80 ms across every
/// `PM: suspend exit` in the ring buffer (WWW-20). Presenting inside that
/// window is presenting to a bridge that is not there yet.
pub const RESUME_BRIDGE_DELAY: Duration = Duration::from_millis(80);

/// A held kernel wakelock, released when dropped.
///
/// **Correctness, not optimisation.** The tablet autosleeps on its own
/// schedule and `/sys/power/state` returns `EBUSY` under autosleep, so a
/// session cannot refuse a suspend any other way. A suspend in the middle of a
/// takeover resumes into a *second* Xochitl contending for the panel — which
/// is a tablet needing a power cycle, not a glitch.
///
/// The lock node is mode `0660`, group `xochitl`, so this needs no root.
///
/// `Drop` releases on every exit path including an unwind, which is the
/// property that matters: a panicking host must not leave the tablet unable to
/// sleep until someone notices the battery.
#[derive(Debug)]
pub struct WakeLock {
    tag: String,
    unlock: PathBuf,
    released: bool,
}

impl WakeLock {
    /// Takes the wakelock named `tag` on the real kernel nodes.
    ///
    /// `tag` must be a single token: the kernel parses the write as a name,
    /// and whitespace in it would register a lock nobody can release by name.
    pub fn acquire(tag: &str) -> Result<Self, DeviceError> {
        Self::acquire_at(tag, Path::new(WAKE_LOCK), Path::new(WAKE_UNLOCK))
    }

    /// Takes the wakelock against explicit paths.
    ///
    /// The paths are arguments so that the release-on-drop behaviour — the
    /// only part of this that is hard to get right — is testable without a
    /// tablet.
    pub fn acquire_at(tag: &str, lock: &Path, unlock: &Path) -> Result<Self, DeviceError> {
        let tag = validate_tag(tag)?;
        fs::write(lock, &tag).map_err(|source| DeviceError::io("write to", lock, source))?;
        Ok(Self {
            tag,
            unlock: unlock.to_path_buf(),
            released: false,
        })
    }

    /// The tag this lock is registered under.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Releases the lock, reporting failure.
    ///
    /// Call this on the normal exit path so a failure is visible. [`Drop`]
    /// calls it too, and there swallows the error, because there is nowhere
    /// for it to go and unwinding out of a destructor is worse.
    pub fn release(mut self) -> Result<(), DeviceError> {
        self.release_inner()
    }

    fn release_inner(&mut self) -> Result<(), DeviceError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        fs::write(&self.unlock, &self.tag)
            .map_err(|source| DeviceError::io("write to", &self.unlock, source))
    }
}

impl Drop for WakeLock {
    fn drop(&mut self) {
        // The tablet not sleeping is a worse outcome than a lost error, so the
        // destructor always runs the release and discards what it says.
        let _ = self.release_inner();
    }
}

fn validate_tag(tag: &str) -> Result<String, DeviceError> {
    if tag.is_empty() || tag.chars().any(char::is_whitespace) {
        return Err(DeviceError::unexpected(format!(
            "wakelock tag {tag:?} must be a single non-empty token"
        )));
    }
    Ok(tag.to_owned())
}

/// Who the vendor's advisory registry says holds the display.
///
/// Format established on the device (WWW-20): five newline-separated fields,
/// `pid`, `process`, `hostname`, `machine-id`, `boot-id`. Observed content, with
/// the identifiers redacted:
///
/// ```text
/// 15310
/// xochitl
/// imx8mm-ferrari
/// <machine-id>
/// <boot-id>
/// ```
///
/// Parsing is deliberately tolerant. This file belongs to closed vendor code
/// and may gain fields in a firmware update; a session must not fail to start
/// because a lock file grew a line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DisplayLockHolder {
    /// The holder's process id, if the first line parsed as one.
    pub pid: Option<i32>,
    /// The holder's process name.
    pub process: Option<String>,
    /// The hostname recorded.
    pub hostname: Option<String>,
    /// The machine id recorded. **Never post this to an issue.**
    pub machine_id: Option<String>,
    /// The boot id recorded — the field that makes staleness detectable.
    pub boot_id: Option<String>,
}

impl DisplayLockHolder {
    /// Parses the contents of `/tmp/epframebuffer.lock`.
    pub fn parse(contents: &str) -> Self {
        let mut lines = contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let pid = lines.next().and_then(|line| line.parse().ok());
        let process = lines.next().map(str::to_owned);
        let hostname = lines.next().map(str::to_owned);
        let machine_id = lines.next().map(str::to_owned);
        let boot_id = lines.next().map(str::to_owned);
        Self {
            pid,
            process,
            hostname,
            machine_id,
            boot_id,
        }
    }

    /// Reads the registry, or `None` when there is no lock file at all.
    pub fn read_from(path: &Path) -> Result<Option<Self>, DeviceError> {
        match fs::read_to_string(path) {
            Ok(contents) => Ok(Some(Self::parse(&contents))),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(DeviceError::io("read", path, source)),
        }
    }

    /// Whether this record was written by a boot that is no longer running.
    ///
    /// A lock naming a different boot id cannot be held by anything, because
    /// the process that wrote it does not exist. This is the *only* safe
    /// staleness test: a matching boot id with a dead pid could still be a
    /// pid that has been recycled.
    pub fn is_from_another_boot(&self, current_boot_id: &str) -> bool {
        self.boot_id
            .as_deref()
            .is_some_and(|recorded| !recorded.eq_ignore_ascii_case(current_boot_id.trim()))
    }

    /// A one-line description safe to log — no machine id, no boot id.
    pub fn describe(&self) -> String {
        match (&self.process, self.pid) {
            (Some(process), Some(pid)) => format!("{process} (pid {pid})"),
            (Some(process), None) => process.clone(),
            (None, Some(pid)) => format!("pid {pid}"),
            (None, None) => "an unnamed holder".to_owned(),
        }
    }
}

/// What the advisory locks say right now.
///
/// Holding DRM master is not the whole of display ownership: Xochitl also
/// participates in this registry, and `EPFramebuffer::checkLockFile()` is the
/// vendor's own side of it. A session that ignored it would be the second
/// writer on the panel with nothing to say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLocks {
    /// Whether the zero-length `flock` target exists.
    pub epd_lock_present: bool,
    /// The registry's contents, if any.
    pub holder: Option<DisplayLockHolder>,
}

impl DisplayLocks {
    /// Inspects the two vendor lock paths. Read-only; changes nothing.
    pub fn inspect() -> Result<Self, DeviceError> {
        Self::inspect_at(Path::new(EPD_LOCK), Path::new(EPFRAMEBUFFER_LOCK))
    }

    /// Inspects explicit paths, so this is testable without a tablet.
    pub fn inspect_at(epd_lock: &Path, registry: &Path) -> Result<Self, DeviceError> {
        Ok(Self {
            epd_lock_present: epd_lock.exists(),
            holder: DisplayLockHolder::read_from(registry)?,
        })
    }

    /// Fails with [`DeviceError::DisplayBusy`] when someone from this boot
    /// still claims the panel.
    ///
    /// `current_boot_id` is `/proc/sys/kernel/random/boot_id`. A holder from a
    /// previous boot is reported as stale and does not block, because it
    /// cannot be holding anything.
    pub fn ensure_free(&self, current_boot_id: &str) -> Result<(), DeviceError> {
        let Some(holder) = &self.holder else {
            return Ok(());
        };
        if holder.is_from_another_boot(current_boot_id) {
            return Ok(());
        }
        Err(DeviceError::DisplayBusy {
            holder: holder.describe(),
        })
    }
}

/// Reads this boot's id from `/proc`.
pub fn current_boot_id() -> Result<String, DeviceError> {
    let path = Path::new("/proc/sys/kernel/random/boot_id");
    fs::read_to_string(path)
        .map(|contents| contents.trim().to_owned())
        .map_err(|source| DeviceError::io("read", path, source))
}

#[cfg(test)]
mod tests {
    use super::{DisplayLockHolder, DisplayLocks, WakeLock};
    use std::fs;
    use std::path::Path;

    /// The observed contents of `/tmp/epframebuffer.lock` on the tablet, with
    /// the two identifiers replaced. Field order is from the WWW-20 capture.
    const OBSERVED: &str = "15310\nxochitl\nimx8mm-ferrari\nmachine-id-here\nboot-id-here\n";

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("paper-device-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("creates a scratch directory");
        dir
    }

    #[test]
    fn the_registry_parses_the_shape_the_tablet_actually_wrote() {
        let holder = DisplayLockHolder::parse(OBSERVED);
        assert_eq!(holder.pid, Some(15310));
        assert_eq!(holder.process.as_deref(), Some("xochitl"));
        assert_eq!(holder.hostname.as_deref(), Some("imx8mm-ferrari"));
        assert_eq!(holder.boot_id.as_deref(), Some("boot-id-here"));
        assert_eq!(holder.describe(), "xochitl (pid 15310)");
    }

    #[test]
    fn describe_leaks_neither_the_machine_id_nor_the_boot_id() {
        let described = DisplayLockHolder::parse(OBSERVED).describe();
        assert!(!described.contains("machine-id-here"), "{described}");
        assert!(!described.contains("boot-id-here"), "{described}");
    }

    #[test]
    fn a_shorter_or_longer_lock_file_parses_rather_than_failing() {
        let short = DisplayLockHolder::parse("42\nxochitl\n");
        assert_eq!(short.pid, Some(42));
        assert_eq!(short.boot_id, None);

        let long = DisplayLockHolder::parse(&format!("{OBSERVED}something-new\n"));
        assert_eq!(long.boot_id.as_deref(), Some("boot-id-here"));

        let empty = DisplayLockHolder::parse("");
        assert_eq!(empty, DisplayLockHolder::default());
        assert_eq!(empty.describe(), "an unnamed holder");
    }

    #[test]
    fn a_lock_from_another_boot_is_stale_and_one_from_this_boot_is_not() {
        let holder = DisplayLockHolder::parse(OBSERVED);
        assert!(holder.is_from_another_boot("a-different-boot"));
        assert!(!holder.is_from_another_boot("boot-id-here"));
        // Trailing newline from /proc must not read as a different boot.
        assert!(!holder.is_from_another_boot("boot-id-here\n"));
    }

    #[test]
    fn a_holder_with_no_boot_id_is_never_assumed_stale() {
        let holder = DisplayLockHolder::parse("42\nxochitl\n");
        assert!(!holder.is_from_another_boot("anything"));
    }

    #[test]
    fn ensure_free_blocks_on_a_live_holder_and_allows_a_stale_one() {
        let locks = DisplayLocks {
            epd_lock_present: true,
            holder: Some(DisplayLockHolder::parse(OBSERVED)),
        };
        let error = locks.ensure_free("boot-id-here").expect_err("blocks");
        assert!(error.to_string().contains("xochitl"), "{error}");
        locks
            .ensure_free("a-different-boot")
            .expect("stale, allowed");

        let free = DisplayLocks {
            epd_lock_present: false,
            holder: None,
        };
        free.ensure_free("boot-id-here").expect("nobody holds it");
    }

    #[test]
    fn inspecting_absent_lock_paths_is_not_an_error() {
        let dir = scratch("absent");
        let locks = DisplayLocks::inspect_at(&dir.join("epd.lock"), &dir.join("fb.lock"))
            .expect("inspects");
        assert!(!locks.epd_lock_present);
        assert_eq!(locks.holder, None);
    }

    #[test]
    fn a_wakelock_writes_its_tag_and_releases_it_on_drop() {
        let dir = scratch("wakelock");
        let lock = dir.join("wake_lock");
        let unlock = dir.join("wake_unlock");
        fs::write(&unlock, "").expect("creates the unlock node");

        {
            let held =
                WakeLock::acquire_at("paperclip-test", &lock, &unlock).expect("takes the lock");
            assert_eq!(held.tag(), "paperclip-test");
            assert_eq!(
                fs::read_to_string(&lock).expect("written"),
                "paperclip-test"
            );
            assert_eq!(fs::read_to_string(&unlock).expect("exists"), "");
        }

        assert_eq!(
            fs::read_to_string(&unlock).expect("written on drop"),
            "paperclip-test"
        );
    }

    #[test]
    fn releasing_twice_writes_once() {
        let dir = scratch("double-release");
        let lock = dir.join("wake_lock");
        let unlock = dir.join("wake_unlock");

        let held = WakeLock::acquire_at("paperclip-once", &lock, &unlock).expect("takes");
        held.release().expect("releases");
        fs::write(&unlock, "sentinel").expect("overwrites");
        // The value dropped inside `release` must not write again.
        assert_eq!(fs::read_to_string(&unlock).expect("exists"), "sentinel");
    }

    #[test]
    fn a_tag_the_kernel_could_not_release_by_name_is_refused() {
        let dir = scratch("bad-tag");
        let lock = dir.join("wake_lock");
        let unlock = dir.join("wake_unlock");

        for tag in ["", "two words", "trailing "] {
            WakeLock::acquire_at(tag, &lock, &unlock).expect_err("refuses");
        }
        assert!(!Path::new(&lock).exists(), "nothing was written");
    }
}
