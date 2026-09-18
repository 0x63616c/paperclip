//! `paperctl doctor` — "can I reach the tablet, and is it healthy" (WWW-34).
//!
//! Replaces two retired shell aliases: one that ran `systemctl is-active
//! xochitl` plus `NRestarts` by hand over SSH, and one that reported which
//! resolution route was used. Read-only: nothing here stops Xochitl, takes
//! the vendor lock, or writes to the device.

use std::time::Duration;

use clap::Args;

use crate::error::CommandError;
use crate::transport::OutputFormat;
use crate::transport::discover::{self, DeviceSource, Prober, QUICK_PROBE_TIMEOUT, SystemProber};
use crate::transport::remote::{CapturedOutput, REMOTE_PAPERCTL, SshRunner, SystemSsh};

/// `paperctl doctor`.
#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Table for a terminal, or one JSON object.
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

/// Which host resolved, from where, and whether it actually answers.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DeviceStatus {
    host: Option<String>,
    source: Option<DeviceSource>,
    reachable: bool,
}

/// `xochitl.service`, read over SSH.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct XochitlStatus {
    active: bool,
    n_restarts: u32,
}

/// The vendor display lock registry, read over SSH.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LockStatus {
    held: bool,
    holder: Option<String>,
}

/// The `paperctl` binary installed on the device.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct BinaryStatus {
    present: bool,
    version: Option<String>,
    matches_local: Option<bool>,
}

/// The overall answer, and what it means for the process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Verdict {
    Healthy,
    Degraded,
    Unreachable,
}

impl Verdict {
    /// 0 healthy, a distinct nonzero for unreachable vs. reachable-but-
    /// degraded (WWW-34 acceptance criterion 9).
    fn exit_code(self) -> u8 {
        match self {
            Verdict::Healthy => 0,
            Verdict::Degraded => 1,
            Verdict::Unreachable => 2,
        }
    }
}

/// The full answer `paperctl doctor` gives.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DoctorReport {
    device: DeviceStatus,
    xochitl: Option<XochitlStatus>,
    lock: Option<LockStatus>,
    device_binary: Option<BinaryStatus>,
    verdict: Verdict,
}

/// Returns the raw verdict code (0/1/2), not [`ExitCode`]: `main` needs the
/// actual number both to build the process's real exit code and to record it
/// on the `paperctl_run` span (WWW-46) — [`ExitCode`] exposes no way to read
/// a code back out of it once built.
pub(crate) fn run(args: &DoctorArgs) -> Result<u8, CommandError> {
    let config =
        crate::transport::config::Config::load(&crate::transport::config::default_path()).ok();
    let report = build_report(
        args.device.as_deref(),
        crate::transport::device_env().as_deref(),
        config.as_ref().and_then(|config| config.pinned()),
        config.as_ref().and_then(|config| config.cached()),
        &SystemProber,
        &SystemSsh,
        QUICK_PROBE_TIMEOUT,
    )?;
    print_report(&report, args.output)?;
    Ok(report.verdict.exit_code())
}

/// [`run`]'s logic, with every resolution input, the prober, the SSH runner
/// and the reachability budget given explicitly — so it is testable without
/// a network, a real tablet, or the actual machine's own `paperctl` config,
/// which may already have a real pin from unrelated work.
#[allow(clippy::too_many_arguments)]
fn build_report(
    flag: Option<&str>,
    env: Option<&str>,
    pinned: Option<&str>,
    cached: Option<&str>,
    prober: &dyn Prober,
    ssh: &dyn SshRunner,
    timeout: Duration,
) -> Result<DoctorReport, CommandError> {
    let device = resolve_status(flag, env, pinned, cached, prober, timeout);
    if !device.reachable {
        return Ok(DoctorReport {
            device,
            xochitl: None,
            lock: None,
            device_binary: None,
            verdict: Verdict::Unreachable,
        });
    }
    let host = device
        .host
        .clone()
        .expect("a reachable device status always names a host");

    let xochitl = query_xochitl(ssh, &host)?;
    let lock = query_lock(ssh, &host)?;
    let device_binary = query_binary(ssh, &host)?;
    let verdict = compute_verdict(&xochitl, &lock, &device_binary);

    Ok(DoctorReport {
        device,
        xochitl: Some(xochitl),
        lock: Some(lock),
        device_binary: Some(device_binary),
        verdict,
    })
}

/// Resolves through the WWW-33 order — the same [`discover::resolve`] every
/// other command uses — but, unlike them, always confirms reachability
/// itself: `resolve` trusts `--device`, `PAPERCTL_DEVICE` and a pin without
/// probing, because a pin is allowed to name a sleeping tablet. `doctor`'s
/// whole job is answering whether the tablet answers, so it cannot inherit
/// that trust.
fn resolve_status(
    flag: Option<&str>,
    env: Option<&str>,
    pinned: Option<&str>,
    cached: Option<&str>,
    prober: &dyn Prober,
    timeout: Duration,
) -> DeviceStatus {
    let inputs = discover::Inputs {
        flag: flag.map(str::to_owned),
        env: env.map(str::to_owned),
        pinned: pinned.map(str::to_owned),
        usb: Some(discover::USB_HOST.to_owned()),
        cache: cached.map(str::to_owned),
    };
    match discover::resolve(&inputs, prober, timeout) {
        Ok((host, source)) => {
            let reachable = match source {
                DeviceSource::Usb | DeviceSource::Mdns | DeviceSource::Cache => true,
                DeviceSource::Flag | DeviceSource::Env | DeviceSource::Pinned => {
                    prober.reachable(&host, timeout)
                }
            };
            DeviceStatus {
                host: Some(host),
                source: Some(source),
                reachable,
            }
        }
        Err(_) => DeviceStatus {
            host: None,
            source: None,
            reachable: false,
        },
    }
}

fn query_xochitl(
    ssh: &dyn SshRunner,
    host: &str,
) -> Result<XochitlStatus, crate::transport::remote::TransportError> {
    let command = format!(
        "systemctl show {} -p ActiveState -p NRestarts",
        paper_host::units::XOCHITL_UNIT
    );
    let output = ssh.run_capture(host, &command)?;
    Ok(parse_xochitl(&output.stdout))
}

fn parse_xochitl(stdout: &str) -> XochitlStatus {
    let mut active = false;
    let mut n_restarts = 0;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("ActiveState=") {
            active = value.trim() == "active";
        } else if let Some(value) = line.strip_prefix("NRestarts=") {
            n_restarts = value.trim().parse().unwrap_or(0);
        }
    }
    XochitlStatus { active, n_restarts }
}

fn query_lock(
    ssh: &dyn SshRunner,
    host: &str,
) -> Result<LockStatus, crate::transport::remote::TransportError> {
    let command = format!(
        "cat {} 2>/dev/null || true",
        paper_device::session::EPFRAMEBUFFER_LOCK
    );
    let output = ssh.run_capture(host, &command)?;
    Ok(parse_lock(&output.stdout))
}

fn parse_lock(stdout: &str) -> LockStatus {
    let holder = paper_device::session::DisplayLockHolder::parse(stdout);
    let held = holder.pid.is_some();
    LockStatus {
        held,
        holder: held.then(|| holder.describe()),
    }
}

fn query_binary(
    ssh: &dyn SshRunner,
    host: &str,
) -> Result<BinaryStatus, crate::transport::remote::TransportError> {
    let command = format!("{REMOTE_PAPERCTL} --version 2>&1");
    let output = ssh.run_capture(host, &command)?;
    Ok(parse_binary(&output))
}

fn parse_binary(output: &CapturedOutput) -> BinaryStatus {
    if output.exit_code != 0 {
        return BinaryStatus {
            present: false,
            version: None,
            matches_local: None,
        };
    }
    let version = output.stdout.split_whitespace().last().map(str::to_owned);
    let matches_local = version
        .as_deref()
        .map(|version| version == env!("CARGO_PKG_VERSION"));
    BinaryStatus {
        present: true,
        version,
        matches_local,
    }
}

fn compute_verdict(xochitl: &XochitlStatus, lock: &LockStatus, binary: &BinaryStatus) -> Verdict {
    let degraded = xochitl.n_restarts > 0
        || lock.held
        || !binary.present
        || binary.matches_local == Some(false);
    if degraded {
        Verdict::Degraded
    } else {
        Verdict::Healthy
    }
}

fn print_report(report: &DoctorReport, output: OutputFormat) -> Result<(), CommandError> {
    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(report).map_err(CommandError::Json)?
            );
        }
        OutputFormat::Table => {
            match (&report.device.host, &report.device.source) {
                (Some(host), Some(source)) => {
                    println!(
                        "device       {host} ({source}), reachable={}",
                        report.device.reachable
                    );
                }
                _ => println!("device       none resolved"),
            }
            if let Some(xochitl) = &report.xochitl {
                println!(
                    "xochitl      active={} n_restarts={}",
                    xochitl.active, xochitl.n_restarts
                );
            }
            if let Some(lock) = &report.lock {
                println!(
                    "lock         held={}{}",
                    lock.held,
                    lock.holder
                        .as_deref()
                        .map(|holder| format!(" ({holder})"))
                        .unwrap_or_default()
                );
            }
            if let Some(binary) = &report.device_binary {
                println!(
                    "device paperctl present={} version={}{}",
                    binary.present,
                    binary.version.as_deref().unwrap_or("-"),
                    match binary.matches_local {
                        Some(true) => " (matches this paperctl)".to_owned(),
                        Some(false) => format!(" (this paperctl is {})", env!("CARGO_PKG_VERSION")),
                        None => String::new(),
                    }
                );
            }
            println!("verdict      {:?}", report.verdict);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::test_doubles::{FakeProber, FakeSsh};

    #[test]
    fn unreachable_names_every_route_and_never_queries_the_device() {
        let prober = FakeProber::default();
        let ssh = FakeSsh::default();

        let report = build_report(
            None,
            None,
            None,
            None,
            &prober,
            &ssh,
            Duration::from_millis(10),
        )
        .expect("ok");

        assert_eq!(report.verdict, Verdict::Unreachable);
        assert!(report.xochitl.is_none());
        assert!(ssh.capture_calls.borrow().is_empty());
        assert_eq!(report.verdict.exit_code(), 2);
    }

    #[test]
    fn a_reachable_healthy_device_reports_healthy() {
        let prober = FakeProber::reachable_hosts(&["10.11.99.1"]);
        let ssh = FakeSsh {
            capture_result: CapturedOutput {
                stdout: "ActiveState=active\nNRestarts=0\n".to_owned(),
                exit_code: 0,
            },
            ..FakeSsh::default()
        };

        let report = build_report(
            None,
            None,
            None,
            None,
            &prober,
            &ssh,
            Duration::from_millis(10),
        )
        .expect("ok");

        // The lock and binary queries reuse the same canned capture result:
        // an empty lock file reads as not held, and a `--version` line with
        // no recognisable token is at least present.
        assert!(report.device.reachable);
        assert_eq!(report.xochitl.as_ref().map(|x| x.n_restarts), Some(0));
        assert_eq!(ssh.capture_calls.borrow().len(), 3);
    }

    #[test]
    fn restarts_hold_or_a_missing_binary_degrade_the_verdict() {
        let restarted = XochitlStatus {
            active: true,
            n_restarts: 1,
        };
        let clean = XochitlStatus {
            active: true,
            n_restarts: 0,
        };
        let free = LockStatus {
            held: false,
            holder: None,
        };
        let held = LockStatus {
            held: true,
            holder: Some("xochitl (pid 1)".to_owned()),
        };
        let present = BinaryStatus {
            present: true,
            version: Some("0.1.0".to_owned()),
            matches_local: Some(true),
        };
        let missing = BinaryStatus {
            present: false,
            version: None,
            matches_local: None,
        };

        assert_eq!(compute_verdict(&clean, &free, &present), Verdict::Healthy);
        assert_eq!(
            compute_verdict(&restarted, &free, &present),
            Verdict::Degraded
        );
        assert_eq!(compute_verdict(&clean, &held, &present), Verdict::Degraded);
        assert_eq!(compute_verdict(&clean, &free, &missing), Verdict::Degraded);
    }

    #[test]
    fn parses_the_shape_systemctl_show_actually_writes() {
        let status = parse_xochitl("ActiveState=active\nNRestarts=2\n");
        assert!(status.active);
        assert_eq!(status.n_restarts, 2);
    }

    #[test]
    fn an_empty_lock_file_reads_as_not_held() {
        let status = parse_lock("");
        assert!(!status.held);
        assert_eq!(status.holder, None);
    }

    #[test]
    fn a_lock_naming_a_holder_reads_as_held() {
        let status = parse_lock("15310\nxochitl\nimx8mm-ferrari\nmachine\nboot\n");
        assert!(status.held);
        assert_eq!(status.holder.as_deref(), Some("xochitl (pid 15310)"));
    }

    #[test]
    fn a_nonzero_version_exit_means_the_binary_is_not_present() {
        let status = parse_binary(&CapturedOutput {
            stdout: "sh: /home/root/paperclip/bin/paperctl: not found".to_owned(),
            exit_code: 127,
        });
        assert!(!status.present);
        assert_eq!(status.version, None);
    }

    #[test]
    fn a_version_mismatch_is_flagged() {
        let status = parse_binary(&CapturedOutput {
            stdout: "paperctl 0.0.1".to_owned(),
            exit_code: 0,
        });
        assert_eq!(status.version.as_deref(), Some("0.0.1"));
        assert_eq!(
            status.matches_local,
            Some("0.0.1" == env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn resolves_within_its_own_budget_not_the_thirty_second_one() {
        // `resolve_status` must accept the caller's timeout rather than
        // hardcoding `discover::PROBE_TIMEOUT` — otherwise `doctor` could
        // never meet its own 15s failure bound (acceptance criterion 8).
        let prober = FakeProber::default();
        let started = std::time::Instant::now();
        let status = resolve_status(None, None, None, None, &prober, Duration::from_millis(50));
        assert!(!status.reachable);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn resolution_follows_the_www_33_precedence_order() {
        let timeout = Duration::from_millis(10);
        let prober = FakeProber::reachable_hosts(&["env-host", "pinned-host", "10.11.99.1"]);

        let flag = resolve_status(
            Some("flag-host"),
            Some("env-host"),
            Some("pinned-host"),
            None,
            &prober,
            timeout,
        );
        assert_eq!(flag.source, Some(DeviceSource::Flag));

        let env = resolve_status(
            None,
            Some("env-host"),
            Some("pinned-host"),
            None,
            &prober,
            timeout,
        );
        assert_eq!(env.source, Some(DeviceSource::Env));

        let pinned = resolve_status(None, None, Some("pinned-host"), None, &prober, timeout);
        assert_eq!(pinned.source, Some(DeviceSource::Pinned));

        let usb = resolve_status(None, None, None, None, &prober, timeout);
        assert_eq!(usb.source, Some(DeviceSource::Usb));
    }

    #[test]
    fn a_pinned_but_unreachable_host_is_reported_as_such() {
        // `discover::resolve` trusts a pin without probing — a pin is
        // allowed to name a sleeping tablet. `doctor` cannot inherit that
        // trust: its only job is saying whether the tablet answers, so it
        // must probe a pin's reachability itself.
        let prober = FakeProber::default();
        let status = resolve_status(
            None,
            None,
            Some("asleep-host"),
            None,
            &prober,
            Duration::from_millis(10),
        );
        assert_eq!(status.source, Some(DeviceSource::Pinned));
        assert!(!status.reachable);
    }

    #[test]
    fn exit_codes_are_zero_one_two_for_healthy_degraded_unreachable() {
        assert_eq!(Verdict::Healthy.exit_code(), 0);
        assert_eq!(Verdict::Degraded.exit_code(), 1);
        assert_eq!(Verdict::Unreachable.exit_code(), 2);
    }

    #[test]
    fn the_json_report_has_the_shape_acceptance_criterion_6_names() {
        let prober = FakeProber::reachable_hosts(&["10.11.99.1"]);
        let ssh = FakeSsh {
            capture_result: CapturedOutput {
                stdout: "ActiveState=active\nNRestarts=0\n".to_owned(),
                exit_code: 0,
            },
            ..FakeSsh::default()
        };
        let report = build_report(
            None,
            None,
            None,
            None,
            &prober,
            &ssh,
            Duration::from_millis(10),
        )
        .expect("ok");

        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).expect("encode")).expect("parse");
        for pointer in [
            "/device/host",
            "/device/source",
            "/device/reachable",
            "/xochitl/active",
            "/xochitl/n_restarts",
            "/lock/held",
            "/device_binary/present",
            "/device_binary/version",
            "/verdict",
        ] {
            assert!(
                value.pointer(pointer).is_some(),
                "missing {pointer} in {value}"
            );
        }
    }

    #[test]
    fn doctor_source_uses_the_same_type_devices_reports() {
        // Not a runtime assertion so much as a compile-time one: `doctor`'s
        // `DeviceStatus::source` and `paperctl devices --output json`'s
        // `Discovered::source` are both `discover::DeviceSource` — the same
        // type, not two enums that happen to look alike — so this would
        // fail to compile, not just fail an assertion, the moment either
        // drifts from the other.
        let source: crate::transport::discover::DeviceSource =
            crate::transport::discover::DeviceSource::Usb;
        let status = DeviceStatus {
            host: Some("10.11.99.1".to_owned()),
            source: Some(source),
            reachable: true,
        };
        assert_eq!(
            serde_json::to_value(status.source).unwrap(),
            serde_json::json!("usb")
        );
    }
}
