//! Real observability (WWW-46).
//!
//! `tracing` is the API everywhere in this workspace from here on. This
//! crate is the sinks, not the calls: a crate that wants to log calls
//! `tracing::info!`/`warn!`/`error!` (or opens a span with `tracing::info_span!`)
//! directly and depends on nothing from here; only the process that owns
//! `main` depends on `paper_telemetry` itself, to choose where those calls
//! end up.
//!
//! Two entry points, matched to where the process runs:
//!
//! - [`init_pretty`] — human-readable output to stderr, plus the JSON
//!   run-log [`run_log::RunLogLayer`] that backs `paperctl logs`. What a
//!   developer's Mac gets.
//! - [`init_journald`] — the systemd journal, Linux only. What the device
//!   gets: the same journal a crash is read out of, rather than text that
//!   scrolled off a terminal nobody was watching.
//!
//! Both install an [`tracing_subscriber::EnvFilter`] read from `RUST_LOG`,
//! defaulting to `info`.
//!
//! # Spans over the sequences that matter
//!
//! `takeover`, `install`, `upgrade`, `session` and `present` are each
//! expected to open one `tracing::info_span!` at their entry point and hold
//! it for the whole operation, so everything that happens underneath —
//! including a `paper_telemetry::diagnostic::record` from an app, or an
//! `paper_telemetry::counters::EINK` entry from a present — nests under it in
//! `journalctl`'s output. This crate does not enforce that (a span is a
//! caller's choice, not a type it hands back), but [`run_log::run_span`] is
//! the one place a span *is* mandatory, because [`run_log::RunLogLayer`]
//! only ever sees the one name it creates.

pub mod counters;
pub mod diagnostic;
pub mod run_log;

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Why telemetry could not be initialised.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// `init_pretty`/`init_journald` was called more than once in this
    /// process, or something else already installed a global subscriber.
    #[error("telemetry was already initialised")]
    AlreadyInitialised,
    /// The systemd journal could not be reached.
    #[cfg(target_os = "linux")]
    #[error("could not connect to the systemd journal: {0}")]
    Journald(#[source] std::io::Error),
}

fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Human-readable output to stderr, plus the JSON run-log at `run_log_dir`.
/// What a developer's Mac gets.
///
/// # Errors
///
/// If a global subscriber was already installed.
pub fn init_pretty(run_log_dir: PathBuf) -> Result<(), InitError> {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr)
        // Prints a span's own duration when it closes. Mac-only convenience:
        // `tracing-journald`'s layer implements no `on_close`, so this never
        // reaches the device journal — an operation that must show its
        // duration there needs an explicit field on the event that reports
        // it (docs/logging.md), which this does not replace.
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE);
    tracing_subscriber::registry()
        .with(env_filter())
        .with(fmt_layer)
        .with(run_log::RunLogLayer::new(run_log_dir))
        .try_init()
        .map_err(|_| InitError::AlreadyInitialised)
}

/// The systemd journal — the same journal a crash is read out of. Linux only;
/// a Mac build of a caller must not reach for this.
///
/// Installs no run-log layer: nothing on the device reads `paperctl logs`'
/// history (that command is itself `#[cfg(not(target_os = "linux"))]`).
///
/// # Errors
///
/// If the journal could not be reached, or a global subscriber was already
/// installed.
#[cfg(target_os = "linux")]
pub fn init_journald() -> Result<(), InitError> {
    let journald = tracing_journald::layer().map_err(InitError::Journald)?;
    tracing_subscriber::registry()
        .with(env_filter())
        .with(journald)
        .try_init()
        .map_err(|_| InitError::AlreadyInitialised)
}
