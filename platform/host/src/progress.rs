//! Main-loop progress, which is not the same thing as being alive (§10).
//!
//! The requirement is explicit: *a heartbeat on an unrelated thread does not
//! prove the UI works*. So the witness a session publishes is a counter it can
//! only advance from the loop that draws, and the supervisor reads two
//! independent facts about a session:
//!
//! * **liveness** — does the process exist. Cheap, and nearly worthless on its
//!   own: a deadlocked app is alive.
//! * **progress** — has the counter moved. This is the one that decides the
//!   §10 "App hang" row.
//!
//! The file format is one line, `<count> <unix_millis>`, written with a
//! create-and-rename so a torn read is impossible. Being a file rather than a
//! socket matters: the supervisor can read it after the session has died, and
//! the last recorded tick goes into the diagnostics bundle.
//!
//! None of this is a safety mechanism. A session that wants to lie can write
//! the file from a timer. The force-stop path is deliberately independent of
//! the witness — §10 asks for that in so many words — so a lying session
//! delays its own termination and prevents nothing.

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Why a progress witness could not be read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProgressError {
    /// The file could not be written.
    #[error("cannot publish main-loop progress to {path}")]
    Write {
        /// Which file.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The file existed but did not parse.
    #[error("{path} is not a progress witness: {detail}")]
    Malformed {
        /// Which file.
        path: PathBuf,
        /// What was wrong.
        detail: String,
    },
}

/// One observation of a session's main loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    /// Monotonically increasing; the only field that proves anything.
    pub count: u64,
    /// When the session says it wrote this.
    pub at: SystemTime,
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tick {} at {}", self.count, millis(self.at))
    }
}

fn millis(at: SystemTime) -> u128 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
}

/// The session side: publishes progress from the loop that draws.
///
/// Takes `&mut self` on [`MainLoopProgress::tick`] so a shared reference
/// cannot be handed to a background thread and ticked from there. That is a
/// nudge, not an enforcement — see the module note about why the supervisor
/// does not depend on it.
#[derive(Debug)]
pub struct MainLoopProgress {
    path: PathBuf,
    count: u64,
}

impl MainLoopProgress {
    /// Publishes to `path`, which the supervisor is watching.
    pub fn new(path: PathBuf) -> Self {
        Self { path, count: 0 }
    }

    /// Records one pass of the main loop.
    ///
    /// # Errors
    ///
    /// Returns [`ProgressError::Write`] if the witness cannot be published,
    /// which the caller should treat as a reason to exit rather than continue
    /// invisibly.
    pub fn tick(&mut self) -> Result<Tick, ProgressError> {
        self.count += 1;
        let tick = Tick {
            count: self.count,
            at: SystemTime::now(),
        };
        write_atomically(&self.path, &format!("{} {}\n", tick.count, millis(tick.at)))?;
        Ok(tick)
    }
}

fn write_atomically(path: &Path, contents: &str) -> Result<(), ProgressError> {
    let fail = |source| ProgressError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(fail)?;
    }
    let temporary = path.with_extension("next");
    let mut file = fs::File::create(&temporary).map_err(fail)?;
    file.write_all(contents.as_bytes()).map_err(fail)?;
    file.sync_data().map_err(fail)?;
    drop(file);
    fs::rename(&temporary, path).map_err(fail)
}

/// The supervisor side: watches a session's witness and decides whether the
/// loop is moving.
#[derive(Debug)]
pub struct ProgressWatch {
    path: PathBuf,
    last: Option<Tick>,
    last_change: Option<SystemTime>,
}

impl ProgressWatch {
    /// Watches `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            last: None,
            last_change: None,
        }
    }

    /// The most recent tick seen.
    pub fn last(&self) -> Option<Tick> {
        self.last
    }

    /// Re-reads the witness and reports what the loop is doing.
    ///
    /// `now` is supplied so the decision is testable without sleeping.
    ///
    /// # Errors
    ///
    /// Returns [`ProgressError::Malformed`] if the file exists but cannot be
    /// read as a witness. A missing file is not an error: a session that has
    /// not ticked yet is simply not making progress yet.
    pub fn poll(&mut self, now: SystemTime, stall: Duration) -> Result<Progress, ProgressError> {
        let observed = self.read()?;
        match (observed, self.last) {
            (Some(new), Some(old)) if new.count > old.count => {
                self.last = Some(new);
                self.last_change = Some(now);
                Ok(Progress::Advancing { tick: new })
            }
            (Some(new), None) => {
                self.last = Some(new);
                self.last_change = Some(now);
                Ok(Progress::Advancing { tick: new })
            }
            (observed, _) => {
                if observed.is_some() {
                    self.last = observed;
                }
                let since = self.last_change.unwrap_or(now);
                let stalled_for = now.duration_since(since).unwrap_or(Duration::ZERO);
                if self.last_change.is_none() {
                    self.last_change = Some(now);
                }
                if stalled_for >= stall {
                    Ok(Progress::Stalled { stalled_for })
                } else {
                    Ok(Progress::Quiet { stalled_for })
                }
            }
        }
    }

    fn read(&self) -> Result<Option<Tick>, ProgressError> {
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return Ok(None);
        };
        let malformed = |detail: &str| ProgressError::Malformed {
            path: self.path.clone(),
            detail: detail.to_owned(),
        };
        let mut fields = raw.split_whitespace();
        let count: u64 = fields
            .next()
            .ok_or_else(|| malformed("empty"))?
            .parse()
            .map_err(|_| malformed("first field is not a counter"))?;
        let at: u64 = fields
            .next()
            .ok_or_else(|| malformed("no timestamp"))?
            .parse()
            .map_err(|_| malformed("second field is not a timestamp"))?;
        Ok(Some(Tick {
            count,
            at: UNIX_EPOCH + Duration::from_millis(at),
        }))
    }
}

/// What the main loop is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// The counter moved.
    Advancing {
        /// The tick that was read.
        tick: Tick,
    },
    /// The counter has not moved, but not for long enough to matter.
    Quiet {
        /// How long it has been still.
        stalled_for: Duration,
    },
    /// The counter has not moved for longer than the budget. This is a hang,
    /// whatever the process table says.
    Stalled {
        /// How long it has been still.
        stalled_for: Duration,
    },
}
