//! `paperctl logs` — the last run's report, as a real command over a real
//! path (WWW-34).
//!
//! A retired shell alias used to `cat /tmp/paperclip-open.log` by hand — an
//! invented path nothing else agreed on. This reads exactly what every
//! dispatch site's `paperctl_run` span writes through
//! [`paper_telemetry::run_log`] (WWW-46), at [`crate::transport::run_log_dir`],
//! the one directory convention both sides use.

use clap::Args;

use crate::error::CommandError;
use crate::transport::OutputFormat;
use paper_telemetry::run_log::{self, RunRecord};

/// `paperctl logs`.
#[derive(Debug, Args)]
pub(crate) struct LogsArgs {
    /// Show an older run: 1 (the default) is the newest, 2 the one before
    /// it, and so on.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    last: Option<u32>,
    /// List every retained run instead of printing one.
    #[arg(long)]
    list: bool,
    /// Table for a terminal, or one JSON array/object.
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
    /// Only runs against this tablet — `--device`, `PAPERCTL_DEVICE`, a pin
    /// or the cache, in that order.
    ///
    /// Never USB or mDNS: reading local history must not depend on live
    /// network probing, so the two *discovery* sources in the WWW-33 order
    /// are left out here — the two flags and the two on-disk sources are
    /// the ones that can name a device without touching the network.
    #[arg(long, value_name = "HOST")]
    device: Option<String>,
}

pub(crate) fn run(args: &LogsArgs) -> Result<(), CommandError> {
    let config =
        crate::transport::config::Config::load(&crate::transport::config::default_path()).ok();
    let wanted = wanted_device(
        args.device.as_deref(),
        crate::transport::device_env().as_deref(),
        config.as_ref().and_then(|config| config.pinned()),
        config.as_ref().and_then(|config| config.cached()),
    );
    run_in(&crate::transport::run_log_dir(), args, wanted.as_deref())
}

/// [`run`], with the run log directory and the resolved device filter given
/// explicitly — split out so a test can supply both instead of a temp
/// directory racing the real config-derived state and the process
/// environment.
fn run_in(
    dir: &std::path::Path,
    args: &LogsArgs,
    wanted_device: Option<&str>,
) -> Result<(), CommandError> {
    let mut records = run_log::list(dir).map_err(CommandError::RunLog)?;
    if records.is_empty() {
        return Err(CommandError::NoRuns {
            dir: dir.to_path_buf(),
        });
    }

    if let Some(wanted) = wanted_device {
        records.retain(|record| record.device.as_deref() == Some(wanted));
    }

    if args.list {
        return print_list(&records, args.output);
    }

    let position = args.last.unwrap_or(1) as usize;
    let retained = records.len();
    match records.into_iter().nth(position - 1) {
        Some(record) => print_one(&record, args.output),
        None => Err(CommandError::NoSuchRun { position, retained }),
    }
}

/// Which device a `--device`/`PAPERCTL_DEVICE` filter (or, failing those, a
/// pin or the cache) means. `None` when nothing names one, in which case
/// `logs` shows every retained run unfiltered.
///
/// Deliberately not the full six-source WWW-33 order: reading local history
/// must not depend on live network probing, so USB and mDNS — the two
/// *discovery* sources, which need a reachable tablet to mean anything — are
/// left out. The two flags and the two on-disk sources are the ones that can
/// name a device without touching the network, so they keep their relative
/// order (flag, then env, then pin, then cache).
fn wanted_device(
    flag: Option<&str>,
    env: Option<&str>,
    pinned: Option<&str>,
    cached: Option<&str>,
) -> Option<String> {
    flag.or(env).or(pinned).or(cached).map(str::to_owned)
}

fn print_list(records: &[RunRecord], output: OutputFormat) -> Result<(), CommandError> {
    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(records).map_err(CommandError::Json)?
            );
        }
        OutputFormat::Table => {
            println!(
                "{:<26} {:<12} {:<24} EXIT",
                "STARTED (unix)", "SUBCOMMAND", "DEVICE"
            );
            for record in records {
                println!(
                    "{:<26} {:<12} {:<24} {}",
                    record.started_at,
                    record.subcommand,
                    record.device.as_deref().unwrap_or("-"),
                    record.exit_code
                );
            }
        }
    }
    Ok(())
}

fn print_one(record: &RunRecord, output: OutputFormat) -> Result<(), CommandError> {
    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(record).map_err(CommandError::Json)?
            );
        }
        OutputFormat::Table => {
            println!("id          {}", record.id);
            println!("started_at  {} (unix)", record.started_at);
            println!("subcommand  {}", record.subcommand);
            println!("device      {}", record.device.as_deref().unwrap_or("-"));
            println!("exit_code   {}", record.exit_code);
            println!("path        {}", record.path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_beats_env_pin_and_cache() {
        assert_eq!(
            wanted_device(Some("flag"), Some("env"), Some("pin"), Some("cache")),
            Some("flag".to_owned())
        );
    }

    #[test]
    fn env_beats_pin_and_cache() {
        assert_eq!(
            wanted_device(None, Some("env"), Some("pin"), Some("cache")),
            Some("env".to_owned())
        );
    }

    #[test]
    fn pin_beats_cache() {
        assert_eq!(
            wanted_device(None, None, Some("pin"), Some("cache")),
            Some("pin".to_owned())
        );
    }

    #[test]
    fn cache_is_the_last_resort() {
        assert_eq!(
            wanted_device(None, None, None, Some("cache")),
            Some("cache".to_owned())
        );
    }

    #[test]
    fn nothing_at_all_means_no_filter() {
        assert_eq!(wanted_device(None, None, None, None), None);
    }

    fn args(last: Option<u32>, list: bool, device: Option<&str>) -> LogsArgs {
        LogsArgs {
            last,
            list,
            output: OutputFormat::Table,
            device: device.map(str::to_owned),
        }
    }

    #[test]
    fn no_retained_runs_names_the_path_it_looked_in() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs");

        let error = run_in(&path, &args(None, false, None), None).unwrap_err();
        assert!(matches!(error, CommandError::NoRuns { dir: reported } if reported == path));
    }

    #[test]
    fn with_no_last_given_the_newest_run_is_shown() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs");
        run_log::record(&path, "open", None, 1).expect("record");
        run_log::record(&path, "open", None, 0).expect("record");

        assert!(run_in(&path, &args(None, false, None), None).is_ok());
        assert!(run_in(&path, &args(Some(3), false, None), None).is_err());
    }

    #[test]
    fn a_device_filter_with_no_matching_run_is_a_missing_run_not_an_empty_list() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs");
        run_log::record(&path, "open", Some("other-host"), 0).expect("record");

        let error = run_in(
            &path,
            &args(None, false, Some("no-such-host")),
            Some("no-such-host"),
        )
        .unwrap_err();
        assert!(matches!(error, CommandError::NoSuchRun { retained: 0, .. }));
    }
}
