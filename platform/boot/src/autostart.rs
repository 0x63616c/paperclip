//! The disable marker: `paperctl autostart disable`'s escape hatch, and one
//! half of the boot-time skip (WWW-53, §10). The other half is
//! `ConditionPathExists=!` on `paperclip-launcher.service` itself
//! ([`crate::units`]), so a misbehaving launcher binary does not even need
//! to run for the skip to take effect.
//!
//! # Not `/data`
//!
//! ADR-0008 originally proposed a `/data` marker, following the vendor's own
//! `ConditionPathExists=/data/internal/rm_enable_ssh_wifi_marker` precedent.
//! `docs/recovery.md`'s hard rule is "never write to `/data`" — it is device
//! identity state, not Paperclip's. The two conflicted, and ADR-0008's
//! WWW-53 amendment resolves it: the marker lives under
//! `/home/root/paperclip`, which is already durable, already all
//! Paperclip's own, and is not device identity. The vendor precedent
//! motivated the *pattern* (a `ConditionPathExists=` kill switch checked
//! before a persistent unit does anything) without the location it happened
//! to use, and this module is where that distinction is enforced in code:
//! nothing here ever names a path under `/data`.

use std::path::{Path, PathBuf};

use paper_packages::store;

use crate::error::BootError;

/// Where the marker lives, under a Paperclip root.
pub fn path(root: &Path) -> PathBuf {
    root.join("autostart-disabled")
}

/// Whether autostart is disabled.
pub fn is_disabled(root: &Path) -> bool {
    path(root).is_file()
}

/// Disables autostart, durably. Idempotent — disabling twice is not an
/// error, and overwrites the recorded reason with the latest one.
///
/// # Errors
///
/// If the durable write failed.
pub fn disable(root: &Path, reason: &str) -> Result<(), BootError> {
    let file = path(root);
    store::atomic_write(&file, format!("{reason}\n").as_bytes())
        .map_err(|source| BootError::Store { path: file, source })
}

/// Enables autostart. Idempotent: enabling an already-enabled device is not
/// an error.
///
/// # Errors
///
/// If the marker exists and could not be removed.
pub fn enable(root: &Path) -> Result<(), BootError> {
    let file = path(root);
    match std::fs::remove_file(&file) {
        Ok(()) => {
            // Best effort: the marker is gone either way, and a caller that
            // wants to confirm durability can read `is_disabled` back.
            let _ = store::sync_directory(root);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BootError::Io { path: file, source }),
    }
}

/// The recorded reason, if autostart is disabled.
///
/// # Errors
///
/// If the marker exists but could not be read.
pub fn reason(root: &Path) -> Result<Option<String>, BootError> {
    let file = path(root);
    match std::fs::read_to_string(&file) {
        Ok(text) => Ok(Some(text.trim().to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(BootError::Io { path: file, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_root_is_not_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_disabled(dir.path()));
        assert_eq!(reason(dir.path()).expect("reason"), None);
    }

    #[test]
    fn disable_then_enable_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        disable(dir.path(), "operator request").expect("disable");
        assert!(is_disabled(dir.path()));
        assert_eq!(
            reason(dir.path()).expect("reason"),
            Some("operator request".to_owned())
        );

        enable(dir.path()).expect("enable");
        assert!(!is_disabled(dir.path()));
    }

    #[test]
    fn enabling_an_already_enabled_root_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        enable(dir.path()).expect("enable a device that was never disabled");
    }

    #[test]
    fn disabling_twice_overwrites_the_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        disable(dir.path(), "first reason").expect("first");
        disable(dir.path(), "second reason").expect("second");
        assert_eq!(
            reason(dir.path()).expect("reason"),
            Some("second reason".to_owned())
        );
    }
}
