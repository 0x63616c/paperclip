//! Reaching the tablet's own `paperctl` over SSH.
//!
//! The whole strategy is: build the same argv a person would type on the
//! tablet, and run it there through the system `ssh` binary — the thing
//! Calum's `~/.aliases` wrapper already did by hand. Nothing here
//! reinterprets a path or stages a file onto the device; `--catalog`,
//! `--trust`, a bundle path and so on are exactly what they are when typed
//! directly on the tablet, because that is exactly what is about to run.
//!
//! `open` is the one exception, and only in *how* it runs, not what it runs:
//! WWW-23 found that a live SSH session does not reliably survive a long
//! hold, so `open` runs detached (`setsid`, logged, backgrounded) and this
//! side polls the log for the `EXIT=` marker instead of waiting on the SSH
//! process itself.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::transport::config::{Config, ConfigError};
use crate::transport::discover::{self, DeviceSource, NoDeviceFound, PROBE_TIMEOUT, SystemProber};

/// Where the on-device build lives. Fixed, not configurable: every doc in
/// this repository that says where `paperctl` is installed says this path.
pub(crate) const REMOTE_PAPERCTL: &str = "/home/root/paperclip/bin/paperctl";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// A safety margin added to `--hold` before an unfinished detached session is
/// treated as stuck rather than merely slow — present, clear and stock
/// restart all take real time on top of the hold itself.
const DETACH_MARGIN: Duration = Duration::from_secs(120);

/// Why the Mac-side transport could not reach or complete on the tablet.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TransportError {
    /// No source in the resolution order named a device.
    #[error(transparent)]
    NoDevice(#[from] NoDeviceFound),
    /// The config file backing the pin and the cache could not be used.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// `ssh` (or `scp`-equivalent local machinery) could not even start.
    #[error("cannot run `{command}`")]
    Spawn {
        /// What was being run.
        command: &'static str,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The remote `paperctl` ran and exited non-zero.
    #[error("`{host}` exited with status {exit_code}")]
    RemoteFailed {
        /// Which tablet.
        host: String,
        /// Its exit code.
        exit_code: i32,
    },
    /// A detached `open` never left an `EXIT=` marker within its deadline.
    #[error(
        "no exit marker from `{host}` after {}s; it may still be running — check {log_path} \
         over ssh",
        .waited.as_secs()
    )]
    DetachedTimedOut {
        /// Which tablet.
        host: String,
        /// Where its log was.
        log_path: String,
        /// How long this side waited.
        waited: Duration,
    },
    /// A local file `deploy` needed to send could not be read.
    #[error("cannot read {path} to send it to the tablet")]
    LocalFile {
        /// Which file.
        path: std::path::PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// What a captured remote command returned.
///
/// Distinct from [`SshRunner::run_blocking`], which inherits stdio because
/// forwarding it live *is* the point of every other command here: `doctor`
/// has to parse the answer, and `deploy`'s install step has to know whether
/// it worked before it can say so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CapturedOutput {
    /// What the remote command printed to stdout.
    pub(crate) stdout: String,
    /// What it exited with.
    pub(crate) exit_code: i32,
}

/// The primitive operations a device transport needs from SSH. A trait so
/// the exact remote command line can be asserted in tests without spawning a
/// real `ssh` process (WWW-33 acceptance criterion 14).
pub(crate) trait SshRunner {
    /// Runs `remote_argv` on `host` as one blocking call, with stdout and
    /// stderr inherited so the tablet's own output reaches the terminal live.
    /// Returns the remote exit code.
    fn run_blocking(&self, host: &str, remote_argv: &[String]) -> Result<i32, TransportError>;

    /// Starts `remote_argv` on `host` detached — it outlives this SSH
    /// session — logging combined stdout/stderr to `log_path` on the device
    /// and appending `EXIT=<code>` once it finishes.
    fn start_detached(
        &self,
        host: &str,
        remote_argv: &[String],
        log_path: &str,
    ) -> Result<(), TransportError>;

    /// Reads `log_path` on `host`. The exit code is `Some` once the `EXIT=`
    /// marker has been written.
    fn poll_detached(
        &self,
        host: &str,
        log_path: &str,
    ) -> Result<(String, Option<i32>), TransportError>;

    /// Runs a literal `remote_command` on `host` — not necessarily the
    /// on-device `paperctl` — as one blocking call, capturing its stdout
    /// rather than inheriting it. For a read-only diagnostic query
    /// (`doctor`) that has to parse the answer.
    fn run_capture(
        &self,
        host: &str,
        remote_command: &str,
    ) -> Result<CapturedOutput, TransportError>;

    /// Runs `remote_command` on `host` with `local_file` piped in as stdin,
    /// capturing stdout. The one primitive `deploy` needs to get a binary
    /// onto the tablet without a second protocol: `ssh host 'cat > path' <
    /// file` does what `scp` would, through the exact transport everything
    /// else here already uses.
    fn run_with_stdin(
        &self,
        host: &str,
        remote_command: &str,
        local_file: &Path,
    ) -> Result<CapturedOutput, TransportError>;
}

/// Shells out to the system `ssh` binary — preferred over an in-process SSH
/// implementation because it already carries the user's own config, keys and
/// agent, and a second implementation of the protocol would be a second
/// thing in this repository that can get authentication wrong.
///
/// Not named outside this module. Every production caller reaches it through
/// [`default_runner`] instead, so `SystemSsh` stays the one place that names
/// — and could be swapped for — the real `ssh` binary.
pub(crate) struct SystemSsh;

/// The [`SshRunner`] every production dispatch goes through.
///
/// A function rather than a public `SystemSsh` so no subcommand module needs
/// to name the concrete type: `deploy` and `transport::dispatch` both take an
/// `&dyn SshRunner` and get the real one from here, the same seam a test
/// swaps in [`crate::transport::test_doubles::FakeSsh`] for.
pub(crate) fn default_runner() -> &'static dyn SshRunner {
    const RUNNER: SystemSsh = SystemSsh;
    &RUNNER
}

impl SystemSsh {
    fn base(host: &str) -> Command {
        let mut command = Command::new("ssh");
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", CONNECT_TIMEOUT.as_secs()))
            .arg(ssh_target(host));
        command
    }

    fn spawn_err(source: std::io::Error) -> TransportError {
        TransportError::Spawn {
            command: "ssh",
            source,
        }
    }
}

/// The target ssh actually gets, from whatever `resolve_device` returned.
///
/// Discovery (USB, mDNS, the cache) returns a bare host or IP with no notion
/// of who to log in as. With `BatchMode=yes` set, an unqualified target that
/// matches no `Host` block in `~/.ssh/config` falls back to the *local*
/// username and default keys — `calum@10.11.99.1: Permission denied
/// (publickey,password)` — while `paperctl devices` reports the same host
/// reachable, because the TCP probe behind that report never asks ssh to
/// authenticate at all (WWW-35).
///
/// Every documented way onto this tablet logs in as `root`, so a bare host
/// gets `root@` prepended. An already-qualified target — `--device`,
/// `PAPERCLIP_DEVICE`, a pin, or a `~/.ssh/config` alias someone wrote
/// `user@` into — is left exactly as given.
fn ssh_target(host: &str) -> String {
    if host.contains('@') {
        host.to_owned()
    } else {
        format!("root@{host}")
    }
}

impl SshRunner for SystemSsh {
    fn run_blocking(&self, host: &str, remote_argv: &[String]) -> Result<i32, TransportError> {
        // Quoted as one command line, exactly as `start_detached` below does
        // it, and for the same reason: `ssh` does not forward argv. It joins
        // whatever it is given with spaces and hands the result to a shell on
        // the far side, so a token containing a space arrives as two tokens.
        // Passing `args(remote_argv)` looks like it preserves argument
        // boundaries and does not — `autostart disable`'s default
        // `--reason "operator request"` reached the device as `--reason
        // operator request` and was rejected as an unexpected argument, which
        // made the one escape hatch WWW-53 requires work over SSH fail
        // whenever its reason held a space, i.e. by default.
        let mut full = Vec::with_capacity(remote_argv.len() + 1);
        full.push(REMOTE_PAPERCTL.to_owned());
        full.extend(remote_argv.iter().cloned());
        let mut command = Self::base(host);
        command.arg(shell_join(full.iter()));
        let status = command.status().map_err(Self::spawn_err)?;
        Ok(status.code().unwrap_or(-1))
    }

    fn start_detached(
        &self,
        host: &str,
        remote_argv: &[String],
        log_path: &str,
    ) -> Result<(), TransportError> {
        let mut full = Vec::with_capacity(remote_argv.len() + 1);
        full.push(REMOTE_PAPERCTL.to_owned());
        full.extend(remote_argv.iter().cloned());
        let invocation = shell_join(full.iter());
        let log = shell_quote(log_path);
        // Everything up to here is already individually quoted for the
        // *inner* `sh -c`. Escaping it as one block for the *outer* `sh -c`
        // is what lets the redirects and `;` stay live shell syntax for the
        // inner one instead of being swallowed by the outer quoting.
        let inner = format!("{invocation} > {log} 2>&1 < /dev/null; echo EXIT=$? >> {log}");
        let remote_shell = format!("setsid sh -c '{}' &", inner.replace('\'', "'\\''"));
        let mut command = Self::base(host);
        command.arg(remote_shell);
        let status = command.status().map_err(Self::spawn_err)?;
        if !status.success() {
            return Err(TransportError::RemoteFailed {
                host: host.to_owned(),
                exit_code: status.code().unwrap_or(-1),
            });
        }
        Ok(())
    }

    fn poll_detached(
        &self,
        host: &str,
        log_path: &str,
    ) -> Result<(String, Option<i32>), TransportError> {
        let mut command = Self::base(host);
        command.arg(format!("cat {} 2>/dev/null || true", shell_quote(log_path)));
        let output = command.output().map_err(Self::spawn_err)?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let exit = text.lines().rev().find_map(|line| {
            line.strip_prefix("EXIT=")
                .and_then(|code| code.trim().parse::<i32>().ok())
        });
        Ok((text, exit))
    }

    fn run_capture(
        &self,
        host: &str,
        remote_command: &str,
    ) -> Result<CapturedOutput, TransportError> {
        let mut command = Self::base(host);
        command.arg(remote_command);
        let output = command.output().map_err(Self::spawn_err)?;
        Ok(CapturedOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn run_with_stdin(
        &self,
        host: &str,
        remote_command: &str,
        local_file: &Path,
    ) -> Result<CapturedOutput, TransportError> {
        let file = std::fs::File::open(local_file).map_err(|source| TransportError::LocalFile {
            path: local_file.to_path_buf(),
            source,
        })?;
        let mut command = Self::base(host);
        command
            .arg(remote_command)
            .stdin(std::process::Stdio::from(file));
        let output = command.output().map_err(Self::spawn_err)?;
        Ok(CapturedOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

/// Single-quotes `value` for use inside a remote shell command.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Joins argv into one shell-safe command line, each token single-quoted.
fn shell_join<'a>(argv: impl Iterator<Item = &'a String>) -> String {
    argv.map(|token| shell_quote(token))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolves `flag` and prints the one `device   <host> (<source>)` banner
/// every device-touching command shows, recording it in the run log at the
/// same time.
///
/// The single place this line is written (WWW-48): nine call sites used to
/// each `println!` and `record_device` it by hand, which is what let them
/// drift apart in the first place.
pub(crate) fn resolve_and_announce(
    flag: Option<&str>,
) -> Result<(String, DeviceSource), TransportError> {
    resolve_and_announce_with_timeout(flag, PROBE_TIMEOUT)
}

/// [`resolve_and_announce`], with the reachability budget given explicitly —
/// see [`resolve_device_with_timeout`] for why `doctor` and `deploy` need
/// their own.
pub(crate) fn resolve_and_announce_with_timeout(
    flag: Option<&str>,
    timeout: Duration,
) -> Result<(String, DeviceSource), TransportError> {
    let (host, source) = resolve_device_with_timeout(flag, timeout)?;
    println!("device   {host} ({source})");
    paper_telemetry::run_log::record_device(&host);
    Ok((host, source))
}

/// Resolves which tablet to talk to, given whatever `--device` was passed
/// (`None` if the flag was absent), with the reachability budget given
/// explicitly — `doctor` and `deploy` pass [`discover::QUICK_PROBE_TIMEOUT`]
/// instead of [`PROBE_TIMEOUT`]; see that constant for why.
pub(crate) fn resolve_device_with_timeout(
    flag: Option<&str>,
    timeout: Duration,
) -> Result<(String, DeviceSource), TransportError> {
    let mut config = Config::load(&crate::transport::config::default_path())?;
    let inputs = discover::Inputs {
        flag: flag.map(str::to_owned),
        env: crate::transport::device_env(),
        pinned: config.pinned().map(str::to_owned),
        usb: Some(discover::USB_HOST.to_owned()),
        cache: config.cached().map(str::to_owned),
    };
    let (host, source) = discover::resolve(&inputs, &SystemProber, timeout)?;

    // Remember it for next time, unless it already is the cache — sparing a
    // write on the overwhelmingly common case of resolving the same tablet
    // repeatedly during a session.
    if source != DeviceSource::Cache {
        config.remember(&host)?;
    }
    Ok((host, source))
}

/// Runs `remote_argv` on `host`, blocking, for every device-touching command
/// except `open` and `run` — see the module doc for why they are different.
/// The [`SshRunner`] is given explicitly — every production caller reaches
/// [`default_runner`] through it, and this module's own tests inject
/// [`crate::transport::test_doubles::FakeSsh`] instead.
pub(crate) fn run_blocking_with(
    runner: &dyn SshRunner,
    host: &str,
    remote_argv: &[String],
) -> Result<(), TransportError> {
    let code = runner.run_blocking(host, remote_argv)?;
    if code != 0 {
        return Err(TransportError::RemoteFailed {
            host: host.to_owned(),
            exit_code: code,
        });
    }
    Ok(())
}

/// Runs `remote_argv` on `host`, detached, and waits for it to finish by
/// polling its log rather than the (possibly dead) SSH session — `open` and
/// `run`, see the module doc. The [`SshRunner`] is given explicitly, same as
/// [`run_blocking_with`].
pub(crate) fn run_open_with(
    runner: &dyn SshRunner,
    host: &str,
    remote_argv: &[String],
    hold: Duration,
) -> Result<(), TransportError> {
    let log_path = format!("/tmp/paperclip-open-{}.log", std::process::id());
    runner.start_detached(host, remote_argv, &log_path)?;

    let deadline = Instant::now() + hold + DETACH_MARGIN;
    let mut backoff = Duration::from_millis(500);
    loop {
        let (text, exit) = runner.poll_detached(host, &log_path)?;
        if let Some(code) = exit {
            print!("{text}");
            if code != 0 {
                return Err(TransportError::RemoteFailed {
                    host: host.to_owned(),
                    exit_code: code,
                });
            }
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TransportError::DetachedTimedOut {
                host: host.to_owned(),
                log_path,
                waited: hold + DETACH_MARGIN,
            });
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::transport::test_doubles::FakeSsh;

    #[test]
    fn a_short_command_runs_blocking_not_detached() {
        let fake = FakeSsh::default();
        let argv = vec!["stock".to_owned(), "--force".to_owned()];

        run_blocking_with(&fake, "tablet.local", &argv).expect("ok");

        assert_eq!(
            fake.blocking_calls.borrow().as_slice(),
            [("tablet.local".to_owned(), argv)]
        );
        assert!(fake.detached_calls.borrow().is_empty());
    }

    #[test]
    fn open_with_a_hold_runs_detached_not_blocking() {
        let fake = FakeSsh::default();
        let argv = vec!["open".to_owned(), "--hold".to_owned(), "45".to_owned()];

        run_open_with(&fake, "tablet.local", &argv, Duration::from_secs(45)).expect("ok");

        let detached = fake.detached_calls.borrow();
        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].0, "tablet.local");
        assert_eq!(detached[0].1, argv);
        assert!(fake.blocking_calls.borrow().is_empty());
    }

    #[test]
    fn open_polls_until_the_exit_marker_appears() {
        let fake = FakeSsh {
            // Popped in reverse: the first poll sees `None` (still running),
            // the second sees the exit code.
            poll_results: RefCell::new(vec![Some(0), None]),
            ..FakeSsh::default()
        };
        let argv = vec!["open".to_owned()];

        run_open_with(&fake, "tablet.local", &argv, Duration::from_millis(1)).expect("ok");

        assert_eq!(fake.poll_calls.borrow().len(), 2);
    }

    #[test]
    fn a_nonzero_remote_exit_is_reported() {
        let fake = FakeSsh {
            blocking_result: 7,
            ..FakeSsh::default()
        };
        let error = run_blocking_with(&fake, "tablet.local", &["setup".to_owned()]).unwrap_err();
        assert!(matches!(
            error,
            TransportError::RemoteFailed { exit_code: 7, .. }
        ));
    }

    #[test]
    fn shell_quoting_survives_a_single_quote() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn a_blocking_argument_containing_a_space_stays_one_argument() {
        // The regression this exists for: `ssh` does not forward argv, so a
        // blocking command built with `args(remote_argv)` let `--reason
        // "operator request"` split on the far side and `autostart disable`
        // — WWW-53's escape hatch, and the only one that works without the
        // panel — was rejected as `unexpected argument 'request'` by its own
        // default. Asserted on the joined command line rather than through
        // `FakeSsh`, because the fake captures argv above the layer that
        // hands it to `ssh`, which is exactly where the boundary was lost.
        let argv = [
            REMOTE_PAPERCTL.to_owned(),
            "autostart".to_owned(),
            "disable".to_owned(),
            "--reason".to_owned(),
            "operator request".to_owned(),
        ];

        assert_eq!(
            shell_join(argv.iter()),
            "'/home/root/paperclip/bin/paperctl' 'autostart' 'disable' \
             '--reason' 'operator request'"
        );
    }

    #[test]
    fn a_bare_discovered_host_gets_the_root_user_ssh_actually_needs() {
        // WWW-35: `10.11.99.1` (USB), an mDNS hostname, and the cache all
        // come back bare. Without this, `BatchMode=yes` sends the *local*
        // username and default keys, which authenticates against nothing on
        // the tablet.
        assert_eq!(ssh_target("10.11.99.1"), "root@10.11.99.1");
        assert_eq!(ssh_target("tablet.local"), "root@tablet.local");
    }

    #[test]
    fn an_already_qualified_target_is_left_exactly_as_given() {
        // `--device`, `PAPERCLIP_DEVICE`, a pin, or a `~/.ssh/config` alias
        // someone already wrote a user into: not this function's business.
        assert_eq!(ssh_target("calum@tablet.local"), "calum@tablet.local");
        assert_eq!(ssh_target("root@10.11.99.1"), "root@10.11.99.1");
    }
}
