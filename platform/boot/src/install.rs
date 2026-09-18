//! Putting the persistent units on the device's root filesystem, and taking
//! them off again (WWW-53, WWW-55).
//!
//! [`units`](crate::units) generates the text; this module is what writes it.
//! That split used to end at a human: `units`' own doc said installing was
//! "a human action at the tablet, not something this crate or any agent run
//! performs". WWW-55 established that a procedure a person retypes is a
//! procedure that is wrong on the second device — three separate hand fixes
//! were needed to make one tablet boot, and none of them would have existed
//! on the next one. So the action moved into code, where it can be one
//! command, idempotent, and reversible; the *decision* to run it is still a
//! person's, which is what ADR-0008's amendment actually requires.
//!
//! # Why `/usr/lib/systemd/system` and not `/etc/systemd/system`
//!
//! `/etc` is an overlay whose upper layer is `/var/volatile` — a tmpfs. A unit
//! written there, and an `systemctl enable` symlink made there, are both gone
//! at the next boot, which is the one boot they exist to affect. The only
//! persistent unit directory on this firmware is on the read-only root, which
//! is why this module remounts it and why ADR-0008 spent so long establishing
//! that there was no alternative.
//!
//! For the same reason the enable symlink is made directly, under
//! `/usr/lib/systemd/system/multi-user.target.wants/`, rather than by asking
//! `systemctl enable` — that command writes into `/etc` and its work would not
//! survive. `systemctl is-enabled` reads `/etc` too, so it reports `disabled`
//! for a unit installed this way; [`is_installed`] asks the question that
//! actually matters instead.

use std::path::{Path, PathBuf};

use paper_host::units::SessionPaths;

use crate::autostart;
use crate::units::{LAUNCHER_UNIT, SAFE_MODE_UNIT, launcher_service, safe_mode_service};

/// The persistent unit directory on this firmware.
pub const UNIT_DIR: &str = "/usr/lib/systemd/system";

/// The target whose `.wants` directory starts the launcher.
pub const WANTED_BY: &str = "multi-user.target";

/// One file the install writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedUnit {
    /// Absolute path the file is written to.
    pub path: PathBuf,
    /// The unit's generated text.
    pub contents: String,
    /// Whether the launcher is linked into [`WANTED_BY`]'s `.wants` directory.
    ///
    /// False for the safe-mode unit, and that is not an oversight.
    /// `safe_mode_service` carries an `[Install]` section because it is a
    /// well-formed unit, but its `ExecStart=` writes the disable marker: a
    /// device that enabled it would turn autostart off on every boot, for
    /// ever, and look like autostart simply did not work. It is reached
    /// through the launcher's `OnFailure=` and no other way.
    pub wanted: bool,
}

/// What an install would write, without writing any of it.
///
/// Separated from `apply` so the paths and the contents can be asserted on
/// a machine with no root filesystem to remount — the decision core is
/// testable, the two `mount` calls are not.
pub fn plan(paths: &SessionPaths, unit_dir: &Path) -> Vec<PlannedUnit> {
    let marker = autostart::path(&paths.root);
    vec![
        PlannedUnit {
            path: unit_dir.join(LAUNCHER_UNIT),
            contents: launcher_service(paths, &marker),
            wanted: true,
        },
        PlannedUnit {
            path: unit_dir.join(SAFE_MODE_UNIT),
            contents: safe_mode_service(&marker),
            wanted: false,
        },
    ]
}

/// Where the `.wants` symlink for `unit` goes, and what it points at.
///
/// Relative (`../<unit>`), the same form systemd's own presets use, so the
/// link does not break if the tree is ever mounted somewhere else.
pub fn wants_link(unit_dir: &Path, unit: &str) -> (PathBuf, PathBuf) {
    (
        unit_dir.join(format!("{WANTED_BY}.wants")).join(unit),
        PathBuf::from(format!("../{unit}")),
    )
}

/// Whether the launcher unit is installed *and* linked into [`WANTED_BY`].
///
/// Both halves, because either alone is a device that will not boot into
/// Paperclip while looking like it might. This is the question
/// `systemctl is-enabled` cannot answer here — it reads `/etc`, which is the
/// volatile overlay this module deliberately does not write to, so it reports
/// `disabled` for a correctly installed launcher.
pub fn is_installed(unit_dir: &Path) -> bool {
    let (link, _) = wants_link(unit_dir, LAUNCHER_UNIT);
    unit_dir.join(LAUNCHER_UNIT).is_file() && link.exists()
}

/// Write the units to the root filesystem and link the launcher into
/// [`WANTED_BY`].
///
/// Remounts `/` read-write for the writes and read-only again afterwards,
/// including on the way out of a failure: a tablet left with a writable root
/// is a tablet one bad write away from an unbootable one, and this is the
/// only code in the project that ever makes it writable.
///
/// Idempotent. Running it twice writes the same bytes and replaces the same
/// symlink, which is what makes it safe to run after every OS update — the
/// one event ADR-0008 established will erase it.
#[cfg(target_os = "linux")]
pub fn apply(
    paths: &SessionPaths,
    unit_dir: &Path,
) -> Result<Vec<PathBuf>, crate::error::BootError> {
    use crate::error::BootError;

    if !paths.launcher().is_file() {
        return Err(BootError::Install(format!(
            "{} is not on the device; install it before the units that exec it",
            paths.launcher().display()
        )));
    }

    let planned = plan(paths, unit_dir);
    remount("rw")?;
    // Every path after this point must run `remount("ro")`, so the work is
    // done in a closure whose result is inspected after the remount rather
    // than propagated with `?` through it.
    let outcome = (|| -> Result<Vec<PathBuf>, BootError> {
        let mut written = Vec::new();
        for unit in &planned {
            std::fs::write(&unit.path, &unit.contents).map_err(|error| {
                BootError::Install(format!("writing {}: {error}", unit.path.display()))
            })?;
            written.push(unit.path.clone());

            if !unit.wanted {
                continue;
            }
            let name = unit
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let (link, target) = wants_link(unit_dir, name);
            if let Some(parent) = link.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    BootError::Install(format!("creating {}: {error}", parent.display()))
                })?;
            }
            // Replaced rather than skipped when present: an install over a
            // link pointing somewhere stale must end with it pointing here.
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&target, &link).map_err(|error| {
                BootError::Install(format!("linking {}: {error}", link.display()))
            })?;
            written.push(link);
        }
        Ok(written)
    })();

    remount("ro")?;
    let written = outcome?;
    daemon_reload()?;
    Ok(written)
}

/// Take the units back off the root filesystem.
#[cfg(target_os = "linux")]
pub fn remove(unit_dir: &Path) -> Result<Vec<PathBuf>, crate::error::BootError> {
    let (link, _) = wants_link(unit_dir, LAUNCHER_UNIT);
    let targets = [
        link,
        unit_dir.join(LAUNCHER_UNIT),
        unit_dir.join(SAFE_MODE_UNIT),
    ];

    remount("rw")?;
    let mut removed = Vec::new();
    for target in targets {
        // A missing file is the desired end state, not a failure: `remove`
        // has to be runnable on a device that was only half installed.
        if std::fs::remove_file(&target).is_ok() {
            removed.push(target);
        }
    }
    remount("ro")?;
    daemon_reload()?;
    Ok(removed)
}

#[cfg(target_os = "linux")]
fn remount(mode: &str) -> Result<(), crate::error::BootError> {
    run("mount", &["-o", &format!("remount,{mode}"), "/"])
}

#[cfg(target_os = "linux")]
fn daemon_reload() -> Result<(), crate::error::BootError> {
    run("systemctl", &["daemon-reload"])
}

#[cfg(target_os = "linux")]
fn run(program: &str, args: &[&str]) -> Result<(), crate::error::BootError> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|error| crate::error::BootError::Install(format!("running {program}: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(crate::error::BootError::Install(format!(
        "{program} {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> SessionPaths {
        SessionPaths::device()
    }

    #[test]
    fn the_plan_writes_both_units_and_wants_only_the_launcher() {
        let planned = plan(&paths(), Path::new(UNIT_DIR));

        let launcher = &planned[0];
        assert_eq!(
            launcher.path,
            PathBuf::from("/usr/lib/systemd/system/paperclip-launcher.service")
        );
        assert!(launcher.wanted);

        let safe_mode = &planned[1];
        assert_eq!(
            safe_mode.path,
            PathBuf::from("/usr/lib/systemd/system/paperclip-safe-mode.service")
        );
        // The whole point: enabling this one disables autostart permanently.
        assert!(!safe_mode.wanted);
    }

    #[test]
    fn the_plan_never_writes_into_the_volatile_overlay() {
        // `/etc` is tmpfs-overlaid on this firmware, so a unit written there
        // is gone at the boot it exists to affect. Asserted structurally
        // because the path is the kind of thing a later edit reaches for by
        // habit, having seen `/etc/systemd/system` on every other Linux.
        for unit in plan(&paths(), Path::new(UNIT_DIR)) {
            assert!(
                !unit.path.starts_with("/etc"),
                "{} is on the volatile overlay",
                unit.path.display()
            );
        }
    }

    #[test]
    fn the_wants_link_is_relative_to_its_own_directory() {
        let (link, target) = wants_link(Path::new(UNIT_DIR), LAUNCHER_UNIT);
        assert_eq!(
            link,
            PathBuf::from(
                "/usr/lib/systemd/system/multi-user.target.wants/paperclip-launcher.service"
            )
        );
        assert_eq!(target, PathBuf::from("../paperclip-launcher.service"));
    }
}
