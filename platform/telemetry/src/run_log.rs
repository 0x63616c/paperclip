//! The Mac-side run history behind `paperctl logs` (ADR-0023), now a
//! `tracing` [`Layer`] instead of a manually-called wrapper (WWW-46).
//!
//! Before this change, recording a run meant every command's own code called
//! `runlog::wrap` — and two call sites out of nine did (`deploy`, `open`);
//! the other seven (`stock`, `setup`, `install`, `upgrade run`, `upgrade
//! rollback`, `remove`, `run`) were invisible to `paperctl logs` because
//! nobody had copied the call into them yet. [`RunLogLayer`] is installed
//! once, in `main`, around a span every dispatch enters — a layer watching
//! for that span cannot be forgotten at a call site the way a function call
//! can, because there is no longer a call site to forget it at.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// The name every dispatch span is created with. [`RunLogLayer`] ignores
/// every other span; this is the one name it watches for.
pub const RUN_SPAN: &str = "paperctl_run";

/// How many runs to keep. Older ones are pruned on the next write.
pub const RETAIN: usize = 20;

/// One retained run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunRecord {
    /// The start time and this process's pid, so two runs started in the
    /// same second still sort and file uniquely.
    pub id: String,
    /// Seconds since the Unix epoch, UTC.
    pub started_at: u64,
    /// The subcommand this run was, e.g. `open`.
    pub subcommand: String,
    /// Which tablet the run targeted, when the run got far enough to know —
    /// `None` for a run that failed before a device resolved.
    pub device: Option<String>,
    /// What the run exited with.
    pub exit_code: i32,
    /// Where this record itself lives.
    pub path: PathBuf,
}

/// Why a run record could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum RunLogError {
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

/// Builds the span every dispatch site enters for the duration of one run.
///
/// `subcommand` is recorded immediately; `device` and `exit_code` start
/// empty and are filled in as they become known — `device` by
/// [`record_device`] once resolution succeeds, `exit_code` by the caller
/// right before the span is dropped.
pub fn run_span(subcommand: &str) -> tracing::Span {
    tracing::info_span!(
        RUN_SPAN,
        subcommand = subcommand,
        device = tracing::field::Empty,
        exit_code = tracing::field::Empty,
    )
}

/// Records which device a run resolved to, on the current
/// [`run_span`]. A no-op if called outside one.
pub fn record_device(host: &str) {
    tracing::Span::current().record("device", host);
}

/// Writes one retained run record in `dir`, pruning down to [`RETAIN`].
///
/// # Errors
///
/// If the directory could not be created or the record could not be written.
pub fn record(
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
///
/// # Errors
///
/// If the directory exists but could not be listed, or a record in it could
/// not be read or decoded.
pub fn list(dir: &Path) -> Result<Vec<RunRecord>, RunLogError> {
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

/// What [`RunLogLayer`] has read off a `paperctl_run` span so far.
#[derive(Debug, Default)]
struct RunFields {
    subcommand: String,
    device: Option<String>,
    exit_code: Option<i64>,
}

impl Visit for RunFields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "exit_code" {
            self.exit_code = Some(value);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "exit_code" {
            self.exit_code = Some(value as i64);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "subcommand" => self.subcommand = value.to_owned(),
            "device" => self.device = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "subcommand" if self.subcommand.is_empty() => self.subcommand = format!("{value:?}"),
            "device" if self.device.is_none() => self.device = Some(format!("{value:?}")),
            _ => {}
        }
    }
}

/// A [`tracing_subscriber::Layer`] that writes one [`RunRecord`] every time a
/// [`RUN_SPAN`] closes.
///
/// Watching span *close*, not creation: `exit_code` is not known until the
/// dispatched command has returned, and a span only closes once every handle
/// to it — including the one `main` holds for the whole dispatch — has been
/// dropped.
#[derive(Debug)]
pub struct RunLogLayer {
    dir: PathBuf,
}

impl RunLogLayer {
    /// Writes records into `dir`.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

impl<S> tracing_subscriber::Layer<S> for RunLogLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        if attrs.metadata().name() != RUN_SPAN {
            return;
        }
        let Some(span) = ctx.span(id) else { return };
        let mut fields = RunFields::default();
        attrs.record(&mut fields);
        span.extensions_mut().insert(fields);
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else { return };
        if span.metadata().name() != RUN_SPAN {
            return;
        }
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<RunFields>() {
            values.record(fields);
        }
    }

    fn on_close(&self, id: tracing::span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        if span.metadata().name() != RUN_SPAN {
            return;
        }
        let extensions = span.extensions();
        let Some(fields) = extensions.get::<RunFields>() else {
            return;
        };
        let exit_code = fields.exit_code.unwrap_or(1);
        let exit_code = i32::try_from(exit_code).unwrap_or(1);
        if let Err(error) = record(
            &self.dir,
            &fields.subcommand,
            fields.device.as_deref(),
            exit_code,
        ) {
            eprintln!("paperctl: could not record this run in the log: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt as _;

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
    fn pruning_keeps_only_the_most_recent_retain_runs() {
        let (_dir, path) = temp_dir();
        for i in 0..(RETAIN + 5) {
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

    /// The behaviour that replaces `runlog::wrap`: entering the span, doing
    /// work, recording the device and the exit code, then dropping the span
    /// is enough on its own to produce a record — no explicit write call at
    /// the dispatch site.
    #[test]
    fn closing_a_run_span_writes_a_record_with_no_explicit_wrap_call() {
        let (_dir, path) = temp_dir();
        let subscriber = tracing_subscriber::registry().with(RunLogLayer::new(path.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);

        let span = run_span("open");
        let result: Result<(), &str> = span.in_scope(|| {
            record_device("remarkable-wifi");
            Ok(())
        });
        let exit_code = i64::from(result.is_err());
        span.record("exit_code", exit_code);
        drop(span);

        let records = list(&path).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].subcommand, "open");
        assert_eq!(records[0].device.as_deref(), Some("remarkable-wifi"));
        assert_eq!(records[0].exit_code, 0);
    }

    #[test]
    fn a_run_that_never_resolves_a_device_is_still_recorded() {
        let (_dir, path) = temp_dir();
        let subscriber = tracing_subscriber::registry().with(RunLogLayer::new(path.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);

        let span = run_span("doctor");
        let result: Result<(), &str> = span.in_scope(|| Err("no device reachable"));
        let exit_code = i64::from(result.is_err());
        span.record("exit_code", exit_code);
        drop(span);

        let records = list(&path).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].device, None);
        assert_eq!(records[0].exit_code, 1);
    }
}
