//! The one sequence every device-touching command runs: resolve → banner →
//! run-log → run (WWW-48).
//!
//! Before this module, eight `Args` structs each carried their own inherent
//! `remote_argv()` method and their own copy of
//! `resolve_device`/`println!("device ...")`/`record_device`/`run_blocking`
//! or `run_open` — nine near-identical blocks (`upgrade.rs` alone had three),
//! agreeing with each other by nobody checking rather than by a shared type.
//! [`RemoteCommand`] replaces the inherent methods with a trait, and
//! [`dispatch`] replaces the repeated call sequence with one function.

use std::time::Duration;

use crate::error::CommandError;
use crate::transport::remote::{self, SshRunner};

/// How long a [`RemoteCommand`]'s own SSH call may run, and whether it must
/// survive the SSH session itself dying.
pub(crate) enum RemoteShape {
    /// Every device-touching command except `open` and `run`: one blocking
    /// `ssh` call, stdio inherited live.
    Blocking,
    /// `open` and `run`: WWW-23 found a live SSH session does not reliably
    /// survive a long hold, so these start detached and are polled for an
    /// exit marker instead of waited on directly.
    Detached {
        /// How long the caller is willing to hold the display (or the
        /// session) for — [`remote::run_open`] adds its own margin on top.
        hold: Duration,
    },
}

/// A subcommand that forwards itself, unchanged, to the tablet's own
/// `paperctl` over SSH (ADR-0020). Implementing this and calling [`dispatch`]
/// is the whole Mac-side half of a device-touching command; nothing else
/// needs to know `ssh` exists.
pub(crate) trait RemoteCommand {
    /// The argv the tablet's own `paperctl` should be given — exactly what
    /// this invocation was, minus any flag that only means something to the
    /// half of the command that already knows it is running on the tablet
    /// (`--present-only`, `--report-to`, `--from-unit`, and so on).
    fn remote_argv(&self) -> Vec<String>;
    /// Blocking, or detached with a hold.
    fn shape(&self) -> RemoteShape;
}

/// Resolves `device`, announces it, and runs `command` there.
pub(crate) fn dispatch(
    device: Option<&str>,
    command: &impl RemoteCommand,
) -> Result<(), CommandError> {
    let (host, _source) = remote::resolve_and_announce(device)?;
    run_resolved(remote::default_runner(), &host, command)
}

/// [`dispatch`]'s sequencing, with the host already resolved and the
/// [`SshRunner`] given explicitly — the seam this module's own tests use to
/// exercise the blocking/detached split without a network or a real `ssh`.
fn run_resolved(
    ssh: &dyn SshRunner,
    host: &str,
    command: &impl RemoteCommand,
) -> Result<(), CommandError> {
    match command.shape() {
        RemoteShape::Blocking => {
            remote::run_blocking_with(ssh, host, &command.remote_argv())?;
        }
        RemoteShape::Detached { hold } => {
            remote::run_open_with(ssh, host, &command.remote_argv(), hold)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::test_doubles::FakeSsh;

    /// A minimal [`RemoteCommand`], the same way `remote.rs`'s own tests
    /// build a bare argv rather than a real subcommand's `Args` — this
    /// module is testing the sequencing every implementer shares, not any
    /// one command's argv construction (that is each command's own test).
    struct Recipe {
        argv: Vec<String>,
        hold: Option<Duration>,
    }

    impl RemoteCommand for Recipe {
        fn remote_argv(&self) -> Vec<String> {
            self.argv.clone()
        }

        fn shape(&self) -> RemoteShape {
            match self.hold {
                Some(hold) => RemoteShape::Detached { hold },
                None => RemoteShape::Blocking,
            }
        }
    }

    #[test]
    fn a_blocking_command_runs_blocking_not_detached() {
        let fake = FakeSsh::default();
        let command = Recipe {
            argv: vec!["stock".to_owned(), "--force".to_owned()],
            hold: None,
        };

        run_resolved(&fake, "tablet.local", &command).expect("ok");

        assert_eq!(
            fake.blocking_calls.borrow().as_slice(),
            [("tablet.local".to_owned(), command.argv.clone())]
        );
        assert!(fake.detached_calls.borrow().is_empty());
    }

    #[test]
    fn a_detached_command_runs_detached_not_blocking() {
        let fake = FakeSsh::default();
        let command = Recipe {
            argv: vec!["open".to_owned(), "--hold".to_owned(), "45".to_owned()],
            hold: Some(Duration::from_secs(45)),
        };

        run_resolved(&fake, "tablet.local", &command).expect("ok");

        let detached = fake.detached_calls.borrow();
        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].0, "tablet.local");
        assert_eq!(detached[0].1, command.argv);
        assert!(fake.blocking_calls.borrow().is_empty());
    }

    #[test]
    fn a_nonzero_blocking_exit_is_reported() {
        let fake = FakeSsh {
            blocking_result: 7,
            ..FakeSsh::default()
        };
        let command = Recipe {
            argv: vec!["setup".to_owned()],
            hold: None,
        };

        let error = run_resolved(&fake, "tablet.local", &command).unwrap_err();
        assert!(matches!(
            error,
            CommandError::Transport(remote::TransportError::RemoteFailed { exit_code: 7, .. })
        ));
    }
}
