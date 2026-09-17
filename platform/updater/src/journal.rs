//! The update journal: `PREPARE -> ACTIVATE -> VERIFY -> COMMIT` (§13).
//!
//! # Why a journal and not a lock
//!
//! The failure this exists for is a power cut, and a power cut takes the lock
//! with it. What survives is a file, and what the file has to answer on the
//! way back up is not "was an update running" but **"what had already
//! happened when it stopped"** — because the answer decides whether the right
//! move is to finish or to revert.
//!
//! The rule is the ordinary write-ahead one: *the record of an intention is
//! made durable before the thing it describes.* [`Journal::record`] fsyncs
//! before it returns, so a phase in the file is a phase whose side effects may
//! or may not have happened, and a phase absent from the file is a phase whose
//! side effects certainly have not.
//!
//! # The phases, and what a reboot in each one means
//!
//! | Phase | What has happened | What reconcile does |
//! |---|---|---|
//! | `prepare` | A staging directory exists; `current` is untouched | Delete the staging directory and any uncommitted release |
//! | `activate` | Stock owns the display; `current` may or may not have moved | Point `current` back at `from`, restore any snapshot |
//! | `verify` | `current` is the candidate; it is being graded | The same: it never committed |
//! | `commit` | The candidate passed | Nothing. Terminal |
//! | `rolled-back` | The candidate failed and the fallback came back | Nothing. Terminal |
//! | `failed` | Neither came back; the device is at stock | Nothing. Terminal, and a person has to look |
//!
//! Reverting rather than resuming, at `activate` and `verify`, is the whole
//! decision. Resuming would mean starting a candidate that has never been
//! shown to be healthy, on a device that just came back from an interruption,
//! with nobody watching. §13's requirement is that a reboot mid-update leaves
//! stock startup available — and because units are runtime-only (WWW-11) it
//! does, by construction. Reverting keeps it that way on the next takeover
//! too.
//!
//! # Attempts
//!
//! [`Record::attempts`] is what stops "bounded health checks" from becoming an
//! unbounded loop across *crashes*. One verify of the candidate and one of the
//! fallback is the budget. A reconcile that finds the budget spent reverts
//! rather than trying again, so a release that reliably kills the machine
//! during verification cannot be retried forever by the thing that is supposed
//! to be recovering from it.

use std::fs;
use std::path::{Path, PathBuf};

use paper_packages::store;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::UpdateError;

/// How many times a candidate may be brought up and graded, across crashes.
pub const MAX_ATTEMPTS: u32 = 1;

/// How far a transaction got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Staging and verifying a candidate. Nothing selected has changed.
    Prepare,
    /// The session is down and the selection is being swapped.
    Activate,
    /// The candidate is up and being graded against the readiness ladder.
    Verify,
    /// The candidate passed. Terminal.
    Commit,
    /// The candidate failed and the fallback is back. Terminal.
    RolledBack,
    /// Neither came back. The device is at stock. Terminal.
    Failed,
}

impl Phase {
    /// The name used in messages and in the file.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Prepare => "prepare",
            Phase::Activate => "activate",
            Phase::Verify => "verify",
            Phase::Commit => "commit",
            Phase::RolledBack => "rolled-back",
            Phase::Failed => "failed",
        }
    }

    /// Whether nothing further will happen to this transaction on its own.
    pub fn is_terminal(self) -> bool {
        matches!(self, Phase::Commit | Phase::RolledBack | Phase::Failed)
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

/// One transaction, as it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// How far it got.
    pub phase: Phase,
    /// What was selected before it started. `None` on a first install.
    pub from: Option<Version>,
    /// What `previous` pointed at before it started.
    ///
    /// Recorded because rollback has to put it back. Without it, a rolled-back
    /// transaction leaves `previous` pointing at the release that is now
    /// `current` — so the next `paperctl upgrade rollback` is a no-op that
    /// stands the session down and back up for nothing.
    #[serde(default)]
    pub previous_before: Option<Version>,
    /// What it is moving to.
    pub to: Version,
    /// The staging directory, while there is one.
    pub staged: Option<PathBuf>,
    /// The platform-state snapshot taken for `from`, if rollback needs one.
    pub snapshot: Option<PathBuf>,
    /// When the transaction started, seconds since the epoch.
    pub started: u64,
    /// When this record was written.
    pub updated: u64,
    /// How many times the candidate has been brought up and graded.
    pub attempts: u32,
    /// The highest readiness rung observed on the last attempt.
    pub reached: Option<String>,
    /// Anything a person reading this at 2am would want.
    pub note: String,
}

impl Record {
    /// A fresh transaction at [`Phase::Prepare`].
    pub fn opening(from: Option<Version>, to: Version) -> Self {
        let now = store::now();
        Self {
            phase: Phase::Prepare,
            from,
            previous_before: None,
            to,
            staged: None,
            snapshot: None,
            started: now,
            updated: now,
            attempts: 0,
            reached: None,
            note: String::new(),
        }
    }

    /// Whether this transaction is still in flight.
    pub fn is_in_flight(&self) -> bool {
        !self.phase.is_terminal()
    }

    /// A one-line summary for a report or a terminal.
    pub fn summary(&self) -> String {
        let from = self
            .from
            .as_ref()
            .map_or_else(|| "nothing".to_owned(), Version::to_string);
        let mut line = format!("{} -> {}: {}", from, self.to, self.phase);
        if let Some(reached) = &self.reached {
            line.push_str(&format!(" (reached `{reached}`)"));
        }
        if !self.note.is_empty() {
            line.push_str(&format!(" — {}", self.note));
        }
        line
    }
}

/// The journal file.
#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    /// The journal at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the current record, if there is one.
    ///
    /// # Errors
    ///
    /// A file that exists but is not a record this build understands. Not
    /// treated as "no update in flight": a journal that cannot be read is
    /// exactly the situation in which guessing is worst.
    pub fn read(&self) -> Result<Option<Record>, UpdateError> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(UpdateError::io(&self.path, source)),
        };
        serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|source| UpdateError::CorruptJournal {
                path: self.path.clone(),
                reason: source.to_string(),
            })
    }

    /// Writes `record` durably, stamping [`Record::updated`].
    ///
    /// Returns once the bytes and the directory entry are both on the disk, so
    /// a power cut after this returns cannot lose the record.
    ///
    /// # Errors
    ///
    /// Any failure to write or fsync.
    pub fn record(&self, record: &mut Record) -> Result<(), UpdateError> {
        record.updated = store::now();
        let encoded =
            serde_json::to_vec_pretty(record).map_err(|source| UpdateError::CorruptJournal {
                path: self.path.clone(),
                reason: source.to_string(),
            })?;
        store::atomic_write(&self.path, &encoded)?;
        Ok(())
    }

    /// Moves `record` to `phase` and writes it.
    ///
    /// # Errors
    ///
    /// As [`Self::record`].
    pub fn advance(
        &self,
        record: &mut Record,
        phase: Phase,
        note: impl Into<String>,
    ) -> Result<(), UpdateError> {
        record.phase = phase;
        let note = note.into();
        if !note.is_empty() {
            record.note = note;
        }
        self.record(record)
    }
}
