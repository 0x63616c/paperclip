//! Whether a release has ever actually worked (§12).
//!
//! Extraction succeeding does not make a release known-good. A package can
//! verify, unpack, commit and select perfectly and still fail on its first
//! instruction, and the install transaction has no way to know — it never runs
//! anything.
//!
//! So "did it start?" is recorded separately, by whoever tried. The ledger
//! exists to make one specific failure impossible: a release that crashes at
//! startup being relaunched forever because each attempt looks like the first.
//! [`Ledger::should_launch`] stops saying yes after
//! [`Ledger::MAX_ATTEMPTS`] attempts that never reported a start, and
//! [`Ledger::failing`] is what a host or the App Store reads to offer a
//! rollback.
//!
//! Nothing here rolls back on its own. §12 wants a failed release to be
//! recoverable from, not quietly replaced: a platform that silently reverts
//! hides the failure, and the next install walks into it again.

use std::path::PathBuf;

use semver::Version;
use serde::Deserialize;

use crate::id::AppId;
use crate::store::{self, Layout, StoreError};

/// What is known about one app's attempts to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// The version the record is about.
    pub version: Version,
    /// Launches attempted since the last reported start.
    pub attempts: u32,
    /// Whether this version has ever reported a successful start.
    pub started: bool,
    /// What the last failure said, if there was one.
    pub last_failure: Option<String>,
}

impl Health {
    /// Longest failure note kept, in bytes. A stack trace is not a status.
    pub const MAX_NOTE_BYTES: usize = 512;

    fn fresh(version: Version) -> Self {
        Self {
            version,
            attempts: 0,
            started: false,
            last_failure: None,
        }
    }

    fn to_document(&self) -> String {
        let mut out = String::from("# Written by the host as it launches apps.\n");
        out.push_str(&format!("version = \"{}\"\n", self.version));
        out.push_str(&format!("attempts = {}\n", self.attempts));
        out.push_str(&format!("started = {}\n", self.started));
        if let Some(note) = &self.last_failure {
            out.push_str(&format!("last_failure = \"{}\"\n", escape(note)));
        }
        out
    }
}

/// The per-app record of launch outcomes.
#[derive(Debug, Clone)]
pub struct Ledger {
    layout: Layout,
}

impl Ledger {
    /// How many attempts without a reported start before a version is treated
    /// as failing.
    ///
    /// Three, not one: a first launch can lose to a transient — the display
    /// still being handed over, a filesystem still settling — and declaring a
    /// release dead on one bad morning is its own failure mode. Three
    /// consecutive attempts that never reported a start is a release that does
    /// not work.
    pub const MAX_ATTEMPTS: u32 = 3;

    /// A ledger over `layout`.
    pub fn new(layout: Layout) -> Self {
        Self { layout }
    }

    /// What is recorded for `app`, if anything.
    pub fn health(&self, app: &AppId) -> Result<Option<Health>, StoreError> {
        let path = self.path(app);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io { path, source });
            }
        };
        let raw: RawHealth =
            toml::from_str(&text).map_err(|_| StoreError::Corrupt { path: path.clone() })?;
        let version = Version::parse(&raw.version).map_err(|_| StoreError::Corrupt { path })?;
        Ok(Some(Health {
            version,
            attempts: raw.attempts,
            started: raw.started,
            last_failure: raw.last_failure,
        }))
    }

    /// Whether the host should launch `version` of `app`.
    ///
    /// `false` once the version has been tried [`Self::MAX_ATTEMPTS`] times
    /// without ever reporting a start. The answer is about this version only:
    /// a new version starts with a clean record, because the point is to stop
    /// relaunching *the release that is broken*, not to stop trying at all.
    pub fn should_launch(&self, app: &AppId, version: &Version) -> Result<bool, StoreError> {
        Ok(match self.health(app)? {
            Some(health) if &health.version == version => {
                health.started || health.attempts < Self::MAX_ATTEMPTS
            }
            _ => true,
        })
    }

    /// Versions that have exhausted their attempts without starting.
    pub fn failing(&self, app: &AppId) -> Result<Option<Health>, StoreError> {
        Ok(self
            .health(app)?
            .filter(|health| !health.started && health.attempts >= Self::MAX_ATTEMPTS))
    }

    /// Records that the host is about to launch `version`.
    ///
    /// Written *before* the launch, and fsynced, because a release that hangs
    /// the device is exactly the case where the attempt must still be on disk
    /// after the power cycle. An attempt counted only on the way back is an
    /// attempt that never gets counted.
    pub fn record_attempt(&self, app: &AppId, version: &Version) -> Result<(), StoreError> {
        let mut health = match self.health(app)? {
            Some(health) if &health.version == version => health,
            _ => Health::fresh(version.clone()),
        };
        health.attempts = health.attempts.saturating_add(1);
        self.write(app, &health)
    }

    /// Records that `version` started successfully.
    pub fn record_started(&self, app: &AppId, version: &Version) -> Result<(), StoreError> {
        self.write(
            app,
            &Health {
                version: version.clone(),
                attempts: 0,
                started: true,
                last_failure: None,
            },
        )
    }

    /// Records that `version` failed, and why.
    pub fn record_failure(
        &self,
        app: &AppId,
        version: &Version,
        reason: &str,
    ) -> Result<(), StoreError> {
        let mut health = match self.health(app)? {
            Some(health) if &health.version == version => health,
            _ => Health::fresh(version.clone()),
        };
        health.started = false;
        let mut note = reason.to_owned();
        note.truncate(
            (0..=Health::MAX_NOTE_BYTES)
                .rev()
                .find(|&n| note.is_char_boundary(n))
                .unwrap_or(0),
        );
        health.last_failure = Some(note);
        self.write(app, &health)
    }

    /// Forgets everything recorded for `app`.
    ///
    /// Called when a new version is selected: the previous version's failures
    /// say nothing about the one replacing it.
    pub fn clear(&self, app: &AppId) -> Result<(), StoreError> {
        match std::fs::remove_file(self.path(app)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io {
                path: self.path(app),
                source,
            }),
        }
    }

    fn write(&self, app: &AppId, health: &Health) -> Result<(), StoreError> {
        let dir = self.layout.state_dir().join("health");
        store::create_dir_if_missing(&dir)?;
        store::atomic_write(&self.path(app), health.to_document().as_bytes())
    }

    fn path(&self, app: &AppId) -> PathBuf {
        self.layout
            .state_dir()
            .join("health")
            .join(format!("{app}.toml"))
    }
}

fn escape(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' => "'".to_owned(),
            '\\' => "/".to_owned(),
            c if c.is_control() => " ".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHealth {
    version: String,
    attempts: u32,
    started: bool,
    #[serde(default)]
    last_failure: Option<String>,
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::Ledger;
    use crate::id::AppId;
    use crate::store::Layout;

    fn app() -> AppId {
        "dev.calum.chess".parse().unwrap()
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn an_unknown_app_may_be_launched() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(Layout::new(dir.path()));
        assert!(ledger.should_launch(&app(), &version("0.1.0")).unwrap());
        assert!(ledger.failing(&app()).unwrap().is_none());
    }

    #[test]
    fn a_release_that_never_starts_stops_being_relaunched() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(Layout::new(dir.path()));
        let v = version("0.2.0");

        for _ in 0..Ledger::MAX_ATTEMPTS {
            assert!(ledger.should_launch(&app(), &v).unwrap());
            ledger.record_attempt(&app(), &v).unwrap();
            ledger
                .record_failure(&app(), &v, "exited immediately")
                .unwrap();
        }

        assert!(!ledger.should_launch(&app(), &v).unwrap());
        let failing = ledger.failing(&app()).unwrap().unwrap();
        assert_eq!(failing.version, v);
        assert_eq!(failing.last_failure.as_deref(), Some("exited immediately"));
    }

    #[test]
    fn a_successful_start_clears_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(Layout::new(dir.path()));
        let v = version("0.2.0");

        for _ in 0..Ledger::MAX_ATTEMPTS {
            ledger.record_attempt(&app(), &v).unwrap();
        }
        assert!(!ledger.should_launch(&app(), &v).unwrap());

        ledger.record_started(&app(), &v).unwrap();
        assert!(ledger.should_launch(&app(), &v).unwrap());
        assert!(ledger.failing(&app()).unwrap().is_none());
    }

    #[test]
    fn a_new_version_is_judged_on_its_own_record() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(Layout::new(dir.path()));
        let broken = version("0.2.0");
        for _ in 0..Ledger::MAX_ATTEMPTS {
            ledger.record_attempt(&app(), &broken).unwrap();
        }
        assert!(!ledger.should_launch(&app(), &broken).unwrap());
        // The rollback target, which is a different release entirely.
        assert!(ledger.should_launch(&app(), &version("0.1.0")).unwrap());
    }

    #[test]
    fn a_failure_note_is_bounded_and_cannot_break_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(Layout::new(dir.path()));
        let v = version("0.1.0");
        let hostile = format!("\"\n[evil]\nattempts = 0\n{}", "x".repeat(4096));
        ledger.record_failure(&app(), &v, &hostile).unwrap();

        let health = ledger.health(&app()).unwrap().unwrap();
        assert!(health.last_failure.unwrap().len() <= super::Health::MAX_NOTE_BYTES);
        assert_eq!(health.version, v);
    }

    #[test]
    fn clearing_forgets_a_previous_versions_failures() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(Layout::new(dir.path()));
        let v = version("0.1.0");
        ledger.record_attempt(&app(), &v).unwrap();
        ledger.clear(&app()).unwrap();
        assert!(ledger.health(&app()).unwrap().is_none());
    }
}
