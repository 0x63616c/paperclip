//! Emptying a session's process tree (§10, "Child survives parent").
//!
//! The Paper Pro has neither `cgroup.freeze` nor `cgroup.kill` (WWW-11), so
//! the atomic version of this is not available and the honest version is a
//! sweep: read the membership, signal it, read it again, repeat until it stays
//! empty or the budget runs out. The loop re-reads every pass precisely
//! because a child can appear between two reads; the alternative — snapshot
//! once and signal that list — is the bug this module exists to not have.
//!
//! Two protections are structural rather than careful:
//!
//! * pid 1 and our own pid are never signalled, and
//! * a caller-supplied protected set is never signalled, which is how stock
//!   Xochitl's pid is kept out of a sweep. Xochitl is only ever *stopped*, by
//!   systemd, cleanly — a signal from here would trip its `StartLimitBurst`
//!   and its missing `OnFailure=` unit, and put the tablet on a serial
//!   console.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::state::Escalation;

/// Where the membership of a session is read from.
#[derive(Debug, Clone)]
pub enum TreeSource {
    /// A cgroup v2 directory. Read recursively: a session that created its
    /// own sub-cgroups still belongs to us.
    Cgroup(PathBuf),
    /// No usable cgroup — walk `/proc` looking for descendants of a pid.
    /// Incomplete by construction, and reported as such.
    Descendants(u32),
}

/// A session's process tree.
#[derive(Debug, Clone)]
pub struct SessionTree {
    source: TreeSource,
    protected: BTreeSet<u32>,
    proc_root: PathBuf,
}

impl SessionTree {
    /// A tree read from a cgroup directory.
    pub fn in_cgroup(path: PathBuf) -> Self {
        Self {
            source: TreeSource::Cgroup(path),
            protected: BTreeSet::new(),
            proc_root: PathBuf::from("/proc"),
        }
    }

    /// A tree read by walking `/proc` from `pid`.
    pub fn below(pid: u32) -> Self {
        Self {
            source: TreeSource::Descendants(pid),
            protected: BTreeSet::new(),
            proc_root: PathBuf::from("/proc"),
        }
    }

    /// Adds pids that must never be signalled. Stock Xochitl's pid goes here.
    pub fn protecting(mut self, pids: impl IntoIterator<Item = u32>) -> Self {
        self.protected.extend(pids);
        self
    }

    /// Reads `/proc` from somewhere else, for tests.
    pub fn with_proc_root(mut self, root: PathBuf) -> Self {
        self.proc_root = root;
        self
    }

    /// Everything currently in the tree, excluding what must not be signalled.
    ///
    /// # Errors
    ///
    /// Propagates the read failure if the cgroup or `/proc` cannot be listed.
    pub fn members(&self) -> std::io::Result<BTreeSet<u32>> {
        let mut found = match &self.source {
            TreeSource::Cgroup(path) => cgroup_members(path)?,
            TreeSource::Descendants(pid) => descendants(&self.proc_root, *pid)?,
        };
        let own = std::process::id();
        found.retain(|pid| *pid > 1 && *pid != own && !self.protected.contains(pid));
        Ok(found)
    }

    /// Empties the tree, escalating from `SIGTERM` to `SIGKILL`.
    ///
    /// Returns what happened rather than a bare success: §10 wants "bounded
    /// force termination", and a caller that cannot tell an empty tree from a
    /// survivor cannot honour the bound.
    ///
    /// # Errors
    ///
    /// Propagates a failure to read the membership. A failure to *signal* a
    /// pid is not an error — the usual cause is that it exited between the
    /// read and the signal, which is the outcome we wanted.
    pub fn terminate(&self, escalation: Escalation) -> std::io::Result<Termination> {
        let started = Instant::now();
        let mut signalled = BTreeSet::new();
        let mut killed = BTreeSet::new();

        for pid in self.members()? {
            signal(pid, libc::SIGTERM);
            signalled.insert(pid);
        }
        if let Some(remaining) = self.settle(escalation.grace)? {
            for pid in &remaining {
                signal(*pid, libc::SIGKILL);
                killed.insert(*pid);
            }
        } else {
            return Ok(Termination {
                emptied: true,
                signalled,
                killed,
                survivors: BTreeSet::new(),
                took: started.elapsed(),
            });
        }
        // The SIGKILL pass repeats too: a process that forked while we were
        // signalling the previous list is only visible on the next read.
        let survivors = loop {
            match self.settle(escalation.force)? {
                None => break BTreeSet::new(),
                Some(remaining) => {
                    if started.elapsed() >= escalation.grace + escalation.force {
                        break remaining;
                    }
                    for pid in &remaining {
                        signal(*pid, libc::SIGKILL);
                        killed.insert(*pid);
                    }
                }
            }
        };
        Ok(Termination {
            emptied: survivors.is_empty(),
            signalled,
            killed,
            survivors,
            took: started.elapsed(),
        })
    }

    /// Waits up to `budget` for the tree to empty. `None` means it did.
    fn settle(&self, budget: Duration) -> std::io::Result<Option<BTreeSet<u32>>> {
        let deadline = Instant::now() + budget;
        loop {
            let remaining = self.members()?;
            if remaining.is_empty() {
                return Ok(None);
            }
            if Instant::now() >= deadline {
                return Ok(Some(remaining));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
}

/// What a termination actually achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Termination {
    /// Whether the tree is empty now. The only field a caller may treat as
    /// success.
    pub emptied: bool,
    /// Pids sent `SIGTERM`.
    pub signalled: BTreeSet<u32>,
    /// Pids sent `SIGKILL`.
    pub killed: BTreeSet<u32>,
    /// Pids still present when the budget ran out.
    pub survivors: BTreeSet<u32>,
    /// How long it took.
    pub took: Duration,
}

/// Sends `signal` to `pid`, ignoring the result.
///
/// A failure here is almost always `ESRCH` — the process exited between the
/// read and the signal — which is the outcome the caller wanted. Anything
/// else shows up as a survivor in [`Termination`], which is a better place to
/// notice it than a log line nobody reads.
fn signal(pid: u32, signal: libc::c_int) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: `kill` takes two integers, touches no memory we own, and has no
    // preconditions beyond a valid signal number. `pid` is a positive value
    // read from the cgroup or /proc, so this can never become a process-group
    // or broadcast signal (which is what pid <= 0 would mean).
    #[allow(unsafe_code)]
    unsafe {
        libc::kill(pid, signal);
    }
}

/// Reads `cgroup.procs` from `path` and every directory under it.
fn cgroup_members(path: &Path) -> std::io::Result<BTreeSet<u32>> {
    let mut found = BTreeSet::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let procs = dir.join("cgroup.procs");
        if let Ok(raw) = fs::read_to_string(&procs) {
            found.extend(
                raw.lines()
                    .filter_map(|line| line.trim().parse::<u32>().ok()),
            );
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
    Ok(found)
}

/// Every descendant of `pid`, by repeatedly closing over parent pids.
///
/// The incompleteness is worth naming: a child that has already reparented to
/// pid 1 is indistinguishable from any other orphan, so this walk cannot find
/// it. That is why [`TreeSource::Cgroup`] is preferred whenever a cgroup
/// exists, and why [`crate::facilities::TreeTermination::is_complete`] reports
/// `false` for this mode.
fn descendants(proc_root: &Path, pid: u32) -> std::io::Result<BTreeSet<u32>> {
    let mut parents: Vec<(u32, u32)> = Vec::new();
    for entry in fs::read_dir(proc_root)? {
        let entry = entry?;
        let Some(name) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(parent) = parent_of(&entry.path()) {
            parents.push((name, parent));
        }
    }
    let mut found = BTreeSet::from([pid]);
    loop {
        let before = found.len();
        for (child, parent) in &parents {
            if found.contains(parent) {
                found.insert(*child);
            }
        }
        if found.len() == before {
            break;
        }
    }
    Ok(found)
}

/// Reads `PPid:` out of `/proc/<pid>/status`.
///
/// `status` rather than `stat`, because a process whose command name contains
/// a space and a bracket makes `stat` ambiguous to parse and this does not.
fn parent_of(dir: &Path) -> Option<u32> {
    let status = fs::read_to_string(dir.join("status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))?
        .trim()
        .parse()
        .ok()
}
