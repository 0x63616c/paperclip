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
}

/// Shells out to the system `ssh` binary — preferred over an in-process SSH
/// implementation because it already carries the user's own config, keys and
/// agent, and a second implementation of the protocol would be a second
/// thing in this repository that can get authentication wrong.
pub(crate) struct SystemSsh;

impl SystemSsh {
    fn base(host: &str) -> Command {
        let mut command = Command::new("ssh");
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={}", CONNECT_TIMEOUT.as_secs()))
            .arg(host);
        command
    }

    fn spawn_err(source: std::io::Error) -> TransportError {
        TransportError::Spawn {
            command: "ssh",
            source,
        }
    }
}

impl SshRunner for SystemSsh {
    fn run_blocking(&self, host: &str, remote_argv: &[String]) -> Result<i32, TransportError> {
        let mut command = Self::base(host);
        command.arg("--").arg(REMOTE_PAPERCTL).args(remote_argv);
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

/// Resolves which tablet to talk to, given whatever `--device` was passed
/// (`None` if the flag was absent).
pub(crate) fn resolve_device(flag: Option<&str>) -> Result<(String, DeviceSource), TransportError> {
    let mut config = Config::load(&crate::transport::config::default_path())?;
    let inputs = discover::Inputs {
        flag: flag.map(str::to_owned),
        env: crate::transport::device_env(),
        pinned: config.pinned().map(str::to_owned),
        usb: Some(discover::USB_HOST.to_owned()),
        cache: config.cached().map(str::to_owned),
    };
    let (host, source) = discover::resolve(&inputs, &SystemProber, PROBE_TIMEOUT)?;

    // Remember it for next time, unless it already is the cache — sparing a
    // write on the overwhelmingly common case of resolving the same tablet
    // repeatedly during a session.
    if source != DeviceSource::Cache {
        config.remember(&host)?;
    }
    Ok((host, source))
}

/// Runs `remote_argv` on `host`, blocking, for every device-touching command
/// except `open` — see the module doc for why `open` is different.
pub(crate) fn run_blocking(host: &str, remote_argv: &[String]) -> Result<(), TransportError> {
    run_blocking_with(&SystemSsh, host, remote_argv)
}

fn run_blocking_with(
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

/// Runs `open`'s `remote_argv` on `host`, detached, and waits for it to
/// finish by polling its log rather than the (possibly dead) SSH session.
pub(crate) fn run_open(
    host: &str,
    remote_argv: &[String],
    hold: Duration,
) -> Result<(), TransportError> {
    run_open_with(&SystemSsh, host, remote_argv, hold)
}

fn run_open_with(
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

    #[derive(Default)]
    struct FakeSsh {
        blocking_calls: RefCell<Vec<(String, Vec<String>)>>,
        detached_calls: RefCell<Vec<(String, Vec<String>, String)>>,
        poll_calls: RefCell<Vec<(String, String)>>,
        blocking_result: i32,
        /// Exit codes to hand back on successive polls — lets a test make the
        /// fake "still running" once before it finishes.
        poll_results: RefCell<Vec<Option<i32>>>,
    }

    impl SshRunner for FakeSsh {
        fn run_blocking(&self, host: &str, remote_argv: &[String]) -> Result<i32, TransportError> {
            self.blocking_calls
                .borrow_mut()
                .push((host.to_owned(), remote_argv.to_vec()));
            Ok(self.blocking_result)
        }

        fn start_detached(
            &self,
            host: &str,
            remote_argv: &[String],
            log_path: &str,
        ) -> Result<(), TransportError> {
            self.detached_calls.borrow_mut().push((
                host.to_owned(),
                remote_argv.to_vec(),
                log_path.to_owned(),
            ));
            Ok(())
        }

        fn poll_detached(
            &self,
            host: &str,
            log_path: &str,
        ) -> Result<(String, Option<i32>), TransportError> {
            self.poll_calls
                .borrow_mut()
                .push((host.to_owned(), log_path.to_owned()));
            let exit = self.poll_results.borrow_mut().pop().unwrap_or(Some(0));
            Ok((String::new(), exit))
        }
    }

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
}
