//! The Mac-side run history behind `paperctl logs` (WWW-34).
//!
//! A retired shell alias used to `cat` an invented `/tmp/paperclip-open.log`
//! by hand. [`default_dir`] is the one constant that replaces it: [`record`]
//! is called by every long device operation when it finishes (`open`
//! today), and `paperctl logs` is the only reader — a writer and a reader
//! that both take the same path as an argument can never quietly drift onto
//! two different ones the way a `cat` typed from memory could.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::CommandError;

/// How many runs to keep. Older ones are pruned on the next write.
pub(crate) const RETAIN: usize = 20;

/// One retained run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RunRecord {
    /// The start time and this process's pid, so two runs started in the
    /// same second still sort and file uniquely.
    pub(crate) id: String,
    /// Seconds since the Unix epoch, UTC. Nothing else in this workspace
    /// depends on a date/time crate for formatting, and a plain integer
    /// sorts and diffs exactly as well as a calendar string.
    pub(crate) started_at: u64,
    /// The subcommand this run was, e.g. `open`.
    pub(crate) subcommand: String,
    /// Which tablet the run targeted, when the run got far enough to know —
    /// `None` for a run that failed before a device resolved.
    pub(crate) device: Option<String>,
    /// What the run exited with.
    pub(crate) exit_code: i32,
    /// Where this record itself lives — a real file `paperctl logs` reads,
    /// replacing the ad-hoc `/tmp/paperclip-open.log` a retired shell alias
    /// used to cat.
    pub(crate) path: PathBuf,
}

/// Why a run record could not be written or read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RunLogError {
    /// The run log directory could not be created.
    #[error("cannot create the run log directory {path}")]
    CreateDir {
        /// Which directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A run record could not be written.
    #[error("cannot write the run record {path}")]
    Write {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A run record could not be encoded as JSON.
    #[error("cannot encode the run record")]
    Encode(#[from] serde_json::Error),
    /// The run log directory could not be listed.
    #[error("cannot read the run log directory {path}")]
    ReadDir {
        /// Which directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A retained run record could not be read.
    #[error("cannot read the run record {path}")]
    Read {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A retained run record is not valid JSON, or not the shape expected.
    #[error("{path} is not a valid paperctl run record")]
    Decode {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

/// Where run records live, absent an override.
///
/// Alongside the device config, under the same directory
/// `PAPERCTL_CONFIG_DIR`/`XDG_CONFIG_HOME` already name — this never needed
/// a convention of its own to drift from that one.
pub(crate) fn default_dir() -> PathBuf {
    crate::transport::config::default_path()
        .parent()
        .expect("the config path always has a parent directory")
        .join("runs")
}

/// Records one finished run in `dir`, pruning down to [`RETAIN`].
pub(crate) fn record(
    dir: &Path,
    subcommand: &str,
    device: Option<&str>,
    exit_code: i32,
) -> Result<RunRecord, RunLogError> {
    std::fs::create_dir_all(dir).map_err(|source| RunLogError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let started_at = now.as_secs();
    // The pid alone collides for two runs in the same wall-clock second
    // (a `deploy` right after an `open`, or two calls inside one test); the
    // sub-second component is what actually makes the id — and the file
    // it names — unique.
    let id = format!("{started_at}-{}-{}", std::process::id(), now.subsec_nanos());
    let path = dir.join(format!("{id}.json"));

    let record = RunRecord {
        id,
        started_at,
        subcommand: subcommand.to_owned(),
        device: device.map(str::to_owned),
        exit_code,
        path: path.clone(),
    };
    let document = serde_json::to_vec_pretty(&record)?;
    std::fs::write(&path, document).map_err(|source| RunLogError::Write {
        path: path.clone(),
        source,
    })?;

    prune(dir)?;
    Ok(record)
}

/// Deletes every retained record past [`RETAIN`], oldest first.
fn prune(dir: &Path) -> Result<(), RunLogError> {
    let mut records = list(dir)?;
    if records.len() <= RETAIN {
        return Ok(());
    }
    records.sort_by_key(|record| record.started_at);
    for stale in &records[..records.len() - RETAIN] {
        let _ = std::fs::remove_file(&stale.path);
    }
    Ok(())
}

/// Every retained record, newest first.
///
/// A directory that has never been created is an empty list, not an error —
/// "no runs yet" and "never run" read the same.
pub(crate) fn list(dir: &Path) -> Result<Vec<RunRecord>, RunLogError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|source| RunLogError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| RunLogError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|source| RunLogError::Read {
            path: path.clone(),
            source,
        })?;
        let record: RunRecord =
            serde_json::from_str(&text).map_err(|source| RunLogError::Decode {
                path: path.clone(),
                source,
            })?;
        records.push(record);
    }
    records.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(records)
}

/// Times `f`, then records one run — exit code 0 on success, 1 on any
/// error, matching the process exit code `main` itself computes for
/// everything that is not `doctor`.
///
/// Best-effort: a run log that could not be written must not turn a
/// successful `open` into a failure, so a recording error is reported and
/// swallowed rather than propagated.
pub(crate) fn wrap<T>(
    dir: &Path,
    subcommand: &str,
    device: Option<&str>,
    f: impl FnOnce() -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    let result = f();
    let exit_code = i32::from(result.is_err());
    if let Err(error) = record(dir, subcommand, device, exit_code) {
        eprintln!("paperctl: could not record this run in the log: {error}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs");
        (dir, path)
    }

    #[test]
    fn logs_reads_what_a_writer_wrote_including_the_exit_code() {
        let (_dir, path) = temp_dir();

        let written = record(&path, "open", Some("remarkable-wifi"), 0).expect("record");
        let read_back = list(&path).expect("list");

        assert_eq!(read_back, vec![written]);
        assert_eq!(read_back[0].exit_code, 0);
        assert_eq!(read_back[0].device.as_deref(), Some("remarkable-wifi"));
    }

    #[test]
    fn a_failed_run_round_trips_its_nonzero_exit_code() {
        let (_dir, path) = temp_dir();

        record(&path, "open", None, 1).expect("record");
        let read_back = list(&path).expect("list");

        assert_eq!(read_back[0].exit_code, 1);
        assert_eq!(read_back[0].device, None);
    }

    #[test]
    fn an_empty_or_missing_directory_lists_as_no_runs() {
        let (_dir, path) = temp_dir();
        assert!(list(&path).expect("list").is_empty());
    }

    #[test]
    fn list_orders_newest_first() {
        let (_dir, path) = temp_dir();
        std::fs::create_dir_all(&path).expect("create dir");
        for (i, started_at) in [10u64, 30, 20].into_iter().enumerate() {
            let record = RunRecord {
                id: format!("synthetic-{i}"),
                started_at,
                subcommand: "open".to_owned(),
                device: None,
                exit_code: started_at as i32,
                path: path.join(format!("synthetic-{i}.json")),
            };
            std::fs::write(&record.path, serde_json::to_vec(&record).unwrap()).expect("write");
        }

        let read_back = list(&path).expect("list");
        let started_ats: Vec<u64> = read_back.iter().map(|record| record.started_at).collect();
        assert_eq!(started_ats, vec![30, 20, 10], "newest (highest) first");
    }

    #[test]
    fn pruning_keeps_only_the_most_recent_retain_runs() {
        let (_dir, path) = temp_dir();
        for i in 0..(RETAIN + 5) {
            // Distinguish records written in the same second by pid alone
            // being insufficient — write directly so retention is testable
            // without sleeping between every one of 25 writes.
            let record = RunRecord {
                id: format!("synthetic-{i}"),
                started_at: i as u64,
                subcommand: "open".to_owned(),
                device: None,
                exit_code: 0,
                path: path.join(format!("synthetic-{i}.json")),
            };
            std::fs::create_dir_all(&path).expect("create dir");
            std::fs::write(&record.path, serde_json::to_vec(&record).unwrap()).expect("write");
        }

        prune(&path).expect("prune");

        let remaining = list(&path).expect("list");
        assert_eq!(remaining.len(), RETAIN);
        assert_eq!(remaining[0].started_at, (RETAIN + 4) as u64);
    }

    #[test]
    fn wrap_records_zero_on_success_and_one_on_failure() {
        let (_dir, path) = temp_dir();

        wrap(&path, "open", Some("remarkable-wifi"), || Ok(())).expect("ok");
        let failing: Result<(), CommandError> = wrap(&path, "open", None, || {
            Err(CommandError::NoRuns { dir: path.clone() })
        });
        assert!(failing.is_err());

        let records = list(&path).expect("list");
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|record| record.exit_code == 0));
        assert!(records.iter().any(|record| record.exit_code == 1));
    }
}
