//! The durable boot-attempt counter (WWW-53, §10).
//!
//! Three boot attempts, matching greenboot, RAUC and barebox — Android uses
//! seven. The counter cannot live under `/etc`, `/var/lib` or `/var/cache`:
//! ADR-0008 established those are overlays with tmpfs upper layers on this
//! firmware, so a counter written there evaporates at the next boot —
//! silently, which is worse than not having one at all. `/home/root/paperclip`
//! is the durable location everything else Paperclip owns already uses.
//!
//! # Why this is not the update journal
//!
//! `paper_updater::journal::Journal` already survives a power cut, and it
//! would be tempting to reuse it. It answers a different question: "did
//! *this transaction* commit". This counter answers "how many *boots in a
//! row* has the currently-selected release failed to reach Home" — a
//! transaction can commit (the candidate was healthy when graded) and the
//! release can still wedge three boots later for a reason grading never
//! exercised. Two questions, two records; conflating them would mean a
//! rollback nobody asked for on a boot the transaction has no opinion about.

use std::path::{Path, PathBuf};

use paper_packages::store;

use crate::error::BootError;

/// How many boots in a row may fail to reach Home before the launcher stops
/// trying. greenboot, RAUC and barebox all use three; Android uses seven —
/// three is the conservative end of established practice, and this is one
/// personal device, not a fleet where a slower failover is affordable.
pub const MAX_BOOT_ATTEMPTS: u32 = 3;

/// Where the counter lives, under a Paperclip root.
pub fn path(root: &Path) -> PathBuf {
    root.join("boot").join("attempts")
}

/// The counter, read from disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BootCounter {
    /// How many consecutive boots have been attempted without reaching Home.
    pub attempts: u32,
}

impl BootCounter {
    /// Reads the counter. A missing file reads as zero — the first boot ever,
    /// or one just reset by [`mark_good`].
    ///
    /// # Errors
    ///
    /// [`BootError::CorruptCounter`] if the file exists but is not a small
    /// non-negative integer; [`BootError::Io`] for any other read failure.
    pub fn read(root: &Path) -> Result<Self, BootError> {
        let file = path(root);
        match std::fs::read_to_string(&file) {
            Ok(text) => text
                .trim()
                .parse()
                .map(|attempts| Self { attempts })
                .map_err(|_| BootError::CorruptCounter { path: file, text }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(BootError::Io { path: file, source }),
        }
    }

    /// Whether this many consecutive failures means "stop trying".
    pub fn exhausted(self) -> bool {
        self.attempts >= MAX_BOOT_ATTEMPTS
    }
}

/// Records that a boot is being attempted, **before** anything that could
/// wedge the machine happens.
///
/// A crash between this write and [`mark_good`] is exactly the case the
/// counter exists to catch, so it has to be durable before the attempt is
/// made, not after — recording it afterward would mean a boot that hangs
/// hard enough to need a power cycle is never counted at all.
///
/// # Errors
///
/// If the durable write failed, or the prior counter could not be read.
pub fn record_attempt(root: &Path) -> Result<BootCounter, BootError> {
    let current = BootCounter::read(root)?;
    let next = BootCounter {
        attempts: current.attempts + 1,
    };
    write(root, next)?;
    Ok(next)
}

/// Resets the counter to zero: this boot reached Home.
///
/// # Errors
///
/// If the durable write failed.
pub fn mark_good(root: &Path) -> Result<(), BootError> {
    write(root, BootCounter::default())
}

fn write(root: &Path, counter: BootCounter) -> Result<(), BootError> {
    let file = path(root);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|source| BootError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    store::atomic_write(&file, format!("{}\n", counter.attempts).as_bytes())
        .map_err(|source| BootError::Store { path: file, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_counter_reads_as_zero_attempts() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            BootCounter::read(dir.path()).expect("read"),
            BootCounter::default()
        );
    }

    #[test]
    fn record_attempt_increments_and_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = record_attempt(dir.path()).expect("first");
        assert_eq!(first.attempts, 1);
        let second = record_attempt(dir.path()).expect("second");
        assert_eq!(second.attempts, 2);
        assert_eq!(BootCounter::read(dir.path()).expect("read"), second);
    }

    #[test]
    fn the_counter_survives_being_read_again_after_a_simulated_power_cut() {
        // "Power cut" here means: a fresh process, reading nothing but the
        // file the previous process left on disk — no in-memory state
        // crosses the boundary, which is the property that matters. The VM
        // harness is what proves the write itself survives a real power
        // loss (fsync before rename, in `paper_packages::store::atomic_write`);
        // this proves the value round-trips through exactly that file.
        let dir = tempfile::tempdir().expect("tempdir");
        record_attempt(dir.path()).expect("attempt 1");
        record_attempt(dir.path()).expect("attempt 2");
        record_attempt(dir.path()).expect("attempt 3");

        let after_power_cut = BootCounter::read(dir.path()).expect("read back");
        assert_eq!(after_power_cut.attempts, 3);
        assert!(after_power_cut.exhausted());
    }

    #[test]
    fn mark_good_resets_the_counter() {
        let dir = tempfile::tempdir().expect("tempdir");
        record_attempt(dir.path()).expect("attempt 1");
        record_attempt(dir.path()).expect("attempt 2");
        mark_good(dir.path()).expect("mark good");
        assert_eq!(
            BootCounter::read(dir.path()).expect("read"),
            BootCounter::default()
        );
    }

    #[test]
    fn three_attempts_is_exhausted_but_two_is_not() {
        assert!(!BootCounter { attempts: 2 }.exhausted());
        assert!(BootCounter { attempts: 3 }.exhausted());
        assert!(BootCounter { attempts: 4 }.exhausted());
    }

    #[test]
    fn a_corrupt_counter_is_an_error_not_a_silent_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("boot")).expect("boot dir");
        std::fs::write(path(dir.path()), b"not-a-number\n").expect("corrupt it");
        assert!(matches!(
            BootCounter::read(dir.path()),
            Err(BootError::CorruptCounter { .. })
        ));
    }
}
