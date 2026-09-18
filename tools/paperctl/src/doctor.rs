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
use crate::transport::remote::{self, CapturedOutput, REMOTE_PAPERCTL, SshRunner};

/// `ssh` reserves 255 for *its own* failures — it could not connect, or could
/// not authenticate — as distinct from any code the remote command returned.
///
/// Without this, an ssh that never ran anything is indistinguishable from a
/// tablet that answered "no": empty output parses as `active=false`,
/// `held=false`, `present=false`, and `doctor` reports a healthy tablet as
/// `Degraded` with every field wrong. Observed 2026-09-18 against a tablet
/// whose `xochitl.service` was `active` with `NRestarts=0` at the time.
///
/// "I could not ask" and "the answer is no" are different answers, and a
/// false `xochitl active=false` is the one that invites a recovery action
/// against a device that never needed one.
const SSH_FAILURE: i32 = 255;

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
    /// The unit's `MainPID`, which is what says whether the process holding
    /// the display is stock Xochitl or something left behind. `None` when the
    /// unit is not running (`systemctl` writes `MainPID=0`).
    main_pid: Option<i32>,
}

/// The vendor display lock registry, read over SSH.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LockStatus {
    held: bool,
    holder: Option<String>,
    /// Whether the holder is stock Xochitl itself, by `MainPID`.
    ///
    /// Stock holds the display whenever it is running, which is the normal
    /// resting state of a tablet nobody has taken over — so a held lock is
    /// only a finding when someone *else* holds it. `tools/device-acceptance/
    /// run.sh`'s `display-free` precondition makes exactly this distinction,
    /// and its comment records that the naive "is anything holding it" check
    /// refused every session on a healthy device the first time it ran on
    /// hardware. `doctor` had the same bug: it could never report `Healthy`.
    stock: bool,
}

/// The `paperctl` binary installed on the device.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct BinaryStatus {
    present: bool,
    version: Option<String>,
    comparison: Option<VersionComparison>,
}

/// How the device's `paperctl` version compares to the one running this
/// command (WWW-76): a plain string-equality check cannot say which side is
/// stale, and "the device's binary is older" is what a recovery bootstrap
/// actually needs surfaced — a newer device talking to an older Mac is not
/// the same problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VersionComparison {
    Same,
    DeviceOlder,
    DeviceNewer,
    /// One side (almost always the device's, which reports whatever WWW-34's
    /// era of `paperctl` printed) did not parse as semver.
    Unparseable,
}

impl VersionComparison {
    fn of(local: &str, device: &str) -> Self {
        match (
            local.parse::<semver::Version>(),
            device.parse::<semver::Version>(),
        ) {
            (Ok(local), Ok(device)) => match device.cmp(&local) {
                std::cmp::Ordering::Less => VersionComparison::DeviceOlder,
                std::cmp::Ordering::Equal => VersionComparison::Same,
                std::cmp::Ordering::Greater => VersionComparison::DeviceNewer,
            },
            _ => VersionComparison::Unparseable,
        }
    }
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
        remote::default_runner(),
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
    let lock = query_lock(ssh, &host, xochitl.as_ref().and_then(|x| x.main_pid))?;
    let device_binary = query_binary(ssh, &host)?;
    let verdict = compute_verdict(xochitl.as_ref(), lock.as_ref(), device_binary.as_ref());

    Ok(DoctorReport {
        device,
        xochitl,
        lock,
        device_binary,
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

/// `None` when the device could not be asked, never a fabricated "no".
fn query_xochitl(
    ssh: &dyn SshRunner,
    host: &str,
) -> Result<Option<XochitlStatus>, crate::transport::remote::TransportError> {
    let command = format!(
        "systemctl show {} -p ActiveState -p NRestarts -p MainPID",
        paper_host::units::XOCHITL_UNIT
    );
    let output = ssh.run_capture(host, &command)?;
    if output.exit_code == SSH_FAILURE {
        return Ok(None);
    }
    Ok(parse_xochitl(&output.stdout))
}

/// `None` when the output carries no `ActiveState=` at all — `systemctl show`
/// always writes the properties it was asked for, so its absence means the
/// command did not run, not that the unit is inactive.
fn parse_xochitl(stdout: &str) -> Option<XochitlStatus> {
    let mut active = None;
    let mut n_restarts = 0;
    let mut main_pid = None;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("ActiveState=") {
            active = Some(value.trim() == "active");
        } else if let Some(value) = line.strip_prefix("NRestarts=") {
            n_restarts = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("MainPID=") {
            // systemctl writes MainPID=0 for a unit with no running process.
            main_pid = value.trim().parse::<i32>().ok().filter(|pid| *pid != 0);
        }
    }
    Some(XochitlStatus {
        active: active?,
        n_restarts,
        main_pid,
    })
}

/// `xochitl_main_pid` is what makes a held lock readable: see
/// [`LockStatus::stock`].
fn query_lock(
    ssh: &dyn SshRunner,
    host: &str,
    xochitl_main_pid: Option<i32>,
) -> Result<Option<LockStatus>, crate::transport::remote::TransportError> {
    let command = format!(
        "cat {} 2>/dev/null || true",
        paper_device::session::EPFRAMEBUFFER_LOCK
    );
    let output = ssh.run_capture(host, &command)?;
    if output.exit_code == SSH_FAILURE {
        return Ok(None);
    }
    Ok(Some(parse_lock(&output.stdout, xochitl_main_pid)))
}

fn parse_lock(stdout: &str, xochitl_main_pid: Option<i32>) -> LockStatus {
    let holder = paper_device::session::DisplayLockHolder::parse(stdout);
    let held = holder.pid.is_some();
    let stock = match (holder.pid, xochitl_main_pid) {
        (Some(holder), Some(xochitl)) => holder == xochitl,
        // Without Xochitl's MainPID there is nothing to compare against, so
        // the holder is not *known* to be stock. Erring toward "report it"
        // keeps a real stray session visible.
        _ => false,
    };
    LockStatus {
        held,
        holder: held.then(|| holder.describe()),
        stock,
    }
}

/// `None` when the device could not be asked. Distinct from a binary that is
/// genuinely absent, which exits non-zero with output of its own.
fn query_binary(
    ssh: &dyn SshRunner,
    host: &str,
) -> Result<Option<BinaryStatus>, crate::transport::remote::TransportError> {
    let command = format!("{REMOTE_PAPERCTL} --version 2>&1");
    let output = ssh.run_capture(host, &command)?;
    if output.exit_code == SSH_FAILURE {
        return Ok(None);
    }
    Ok(Some(parse_binary(&output)))
}

fn parse_binary(output: &CapturedOutput) -> BinaryStatus {
    if output.exit_code != 0 {
        return BinaryStatus {
            present: false,
            version: None,
            comparison: None,
        };
    }
    let version = output.stdout.split_whitespace().last().map(str::to_owned);
    let comparison = version
        .as_deref()
        .map(|version| VersionComparison::of(env!("CARGO_PKG_VERSION"), version));
    BinaryStatus {
        present: true,
        version,
        comparison,
    }
}

/// A missing component means the tablet answered the TCP probe but could not
/// be *asked* — `ssh` failed. That is not `Degraded`, which asserts something
/// about the device: it is a device this command cannot speak for, so it gets
/// [`Verdict::Unreachable`] and its exit code. Reporting `Healthy` or
/// `Degraded` from three unanswered questions is the failure this replaces.
fn compute_verdict(
    xochitl: Option<&XochitlStatus>,
    lock: Option<&LockStatus>,
    binary: Option<&BinaryStatus>,
) -> Verdict {
    let (Some(xochitl), Some(lock), Some(binary)) = (xochitl, lock, binary) else {
        return Verdict::Unreachable;
    };
    let degraded = xochitl.n_restarts > 0
        || (lock.held && !lock.stock)
        || !binary.present
        || matches!(
            binary.comparison,
            Some(VersionComparison::DeviceOlder) | Some(VersionComparison::Unparseable)
        );
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
            match &report.xochitl {
                Some(xochitl) => println!(
                    "xochitl      active={} n_restarts={}",
                    xochitl.active, xochitl.n_restarts
                ),
                None if report.device.reachable => {
                    println!("xochitl      unknown (could not run commands over ssh)");
                }
                None => {}
            }
            match &report.lock {
                Some(lock) => println!(
                    "lock         held={}{}{}",
                    lock.held,
                    lock.holder
                        .as_deref()
                        .map(|holder| format!(" ({holder})"))
                        .unwrap_or_default(),
                    if lock.held && lock.stock {
                        " — stock xochitl, which is the normal resting state"
                    } else {
                        ""
                    }
                ),
                None if report.device.reachable => {
                    println!("lock         unknown (could not run commands over ssh)");
                }
                None => {}
            }
            if report.device_binary.is_none() && report.device.reachable {
                println!("device paperctl unknown (could not run commands over ssh)");
            }
            if let Some(binary) = &report.device_binary {
                println!(
                    "device paperctl present={} version={}{}",
                    binary.present,
                    binary.version.as_deref().unwrap_or("-"),
                    match binary.comparison {
                        Some(VersionComparison::Same) => " (matches this paperctl)".to_owned(),
                        Some(VersionComparison::DeviceOlder) => format!(
                            " (older than this paperctl, {} — run `paperctl upgrade bootstrap` \
                             or `paperctl deploy` to refresh it)",
                            env!("CARGO_PKG_VERSION")
                        ),
                        Some(VersionComparison::DeviceNewer) =>
                            format!(" (newer than this paperctl, {})", env!("CARGO_PKG_VERSION")),
                        Some(VersionComparison::Unparseable) => {
                            " (could not compare to this paperctl)".to_owned()
                        }
                        None => String::new(),
                    }
                );
            }
            println!("verdict      {:?}", report.verdict);
            // A tablet that answers on :22 but cannot be asked anything is
            // the case that used to be reported as three confident falsehoods.
            // Say what actually happened, and what usually causes it.
            if report.device.reachable && report.xochitl.is_none() {
                println!(
                    "             the tablet answered on port 22 but `ssh` could not run a \
                     command.\n             Check that `ssh {} true` works — a host with no \
                     matching\n             `~/.ssh/config` entry logs in as the local user \
                     with no key.",
                    report.device.host.as_deref().unwrap_or("<host>")
                );
            }
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
    fn restarts_a_stray_hold_or_a_missing_binary_degrade_the_verdict() {
        let restarted = XochitlStatus {
            active: true,
            n_restarts: 1,
            main_pid: Some(423),
        };
        let clean = XochitlStatus {
            active: true,
            n_restarts: 0,
            main_pid: Some(423),
        };
        let free = LockStatus {
            held: false,
            holder: None,
            stock: false,
        };
        let held_by_stranger = LockStatus {
            held: true,
            holder: Some("paperctl (pid 900)".to_owned()),
            stock: false,
        };
        let present = BinaryStatus {
            present: true,
            version: Some("0.1.0".to_owned()),
            comparison: Some(VersionComparison::Same),
        };
        let missing = BinaryStatus {
            present: false,
            version: None,
            comparison: None,
        };

        assert_eq!(
            compute_verdict(Some(&clean), Some(&free), Some(&present)),
            Verdict::Healthy
        );
        assert_eq!(
            compute_verdict(Some(&restarted), Some(&free), Some(&present)),
            Verdict::Degraded
        );
        assert_eq!(
            compute_verdict(Some(&clean), Some(&held_by_stranger), Some(&present)),
            Verdict::Degraded
        );
        assert_eq!(
            compute_verdict(Some(&clean), Some(&free), Some(&missing)),
            Verdict::Degraded
        );
    }

    #[test]
    fn a_display_lock_held_by_stock_xochitl_is_not_a_finding() {
        // The normal resting state of every tablet nobody has taken over.
        // Treating it as a finding meant `doctor` could never say `Healthy`
        // and exited 1 forever — which in a CI gate reads as a failure.
        let clean = XochitlStatus {
            active: true,
            n_restarts: 0,
            main_pid: Some(423),
        };
        let present = BinaryStatus {
            present: true,
            version: Some("0.1.0".to_owned()),
            comparison: Some(VersionComparison::Same),
        };
        let held_by_stock = parse_lock("423\nxochitl\nimx8mm-ferrari\nmachine\nboot\n", Some(423));

        assert!(held_by_stock.held, "stock really is holding it");
        assert!(held_by_stock.stock);
        assert_eq!(
            compute_verdict(Some(&clean), Some(&held_by_stock), Some(&present)),
            Verdict::Healthy
        );

        // The same lock, held by anything that is not Xochitl's MainPID, is
        // exactly the stray session the check exists to catch.
        let stray = parse_lock("900\npaperctl\nimx8mm-ferrari\nmachine\nboot\n", Some(423));
        assert!(!stray.stock);
        assert_eq!(
            compute_verdict(Some(&clean), Some(&stray), Some(&present)),
            Verdict::Degraded
        );
    }

    #[test]
    fn a_question_that_could_not_be_asked_is_never_answered_no() {
        // ssh exits 255 when it could not connect or authenticate, having run
        // nothing. The empty output that follows used to parse as three
        // confident falsehoods and a `Degraded` verdict.
        let prober = FakeProber::reachable_hosts(&["10.11.99.1"]);
        let ssh = FakeSsh {
            capture_result: CapturedOutput {
                stdout: String::new(),
                exit_code: 255,
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

        assert!(report.device.reachable, "the TCP probe did answer");
        assert!(report.xochitl.is_none(), "not `active=false`");
        assert!(report.lock.is_none(), "not `held=false`");
        assert!(report.device_binary.is_none(), "not `present=false`");
        assert_eq!(
            report.verdict,
            Verdict::Unreachable,
            "a device that cannot be asked is not one to call Degraded"
        );
    }

    #[test]
    fn output_with_no_active_state_is_not_an_inactive_unit() {
        // `systemctl show` always writes the properties it was asked for.
        assert!(parse_xochitl("").is_none());
        assert!(parse_xochitl("some unrelated noise\n").is_none());
    }

    #[test]
    fn version_comparison_reads_semver_order_not_string_equality() {
        assert_eq!(
            VersionComparison::of("0.4.0", "0.4.0"),
            VersionComparison::Same
        );
        assert_eq!(
            VersionComparison::of("0.4.0", "0.3.9"),
            VersionComparison::DeviceOlder
        );
        assert_eq!(
            VersionComparison::of("0.4.0", "0.5.0"),
            VersionComparison::DeviceNewer
        );
        assert_eq!(
            VersionComparison::of("0.4.0", "garbage"),
            VersionComparison::Unparseable
        );
    }

    #[test]
    fn parses_the_shape_systemctl_show_actually_writes() {
        let status = parse_xochitl("ActiveState=active\nNRestarts=2\nMainPID=423\n")
            .expect("ActiveState= is present, so this is a real answer");
        assert!(status.active);
        assert_eq!(status.n_restarts, 2);
        assert_eq!(status.main_pid, Some(423));

        // A stopped unit reports MainPID=0, which is not a pid.
        let stopped = parse_xochitl("ActiveState=inactive\nNRestarts=0\nMainPID=0\n")
            .expect("still a real answer");
        assert_eq!(stopped.main_pid, None);
    }

    #[test]
    fn an_empty_lock_file_reads_as_not_held() {
        let status = parse_lock("", Some(423));
        assert!(!status.held);
        assert_eq!(status.holder, None);
    }

    #[test]
    fn a_lock_naming_a_holder_reads_as_held() {
        let status = parse_lock("15310\nxochitl\nimx8mm-ferrari\nmachine\nboot\n", Some(423));
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
            status.comparison,
            Some(VersionComparison::of(env!("CARGO_PKG_VERSION"), "0.0.1"))
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
