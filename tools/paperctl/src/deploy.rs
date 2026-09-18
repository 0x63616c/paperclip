//! `paperctl deploy` — the dev loop: cross-compile `paperctl` for the device
//! and install it (WWW-34). Replaces the retired shell alias that ran
//! `tools/cross/build-device.sh` and then a hand-typed `scp`.
//!
//! This is the dev-loop deploy of the tool itself, not `upgrade` (§13,
//! releases) and not WWW-24's lifecycle ergonomics: it does not sign
//! anything, does not go through a catalog, and does not touch the install
//! transaction. It replaces a copy of `paperctl` in place, which is exactly
//! what the shell alias it retires did.

use std::path::Path;

use clap::Args;

use crate::error::CommandError;
use crate::transport::discover::QUICK_PROBE_TIMEOUT;
use crate::transport::remote::{self, REMOTE_PAPERCTL, SshRunner, TransportError};

/// Where `tools/cross/build-device.sh` is, relative to the repository root —
/// `deploy`, like the script itself, is meant to be run from there.
const BUILD_SCRIPT: &str = "tools/cross/build-device.sh";

/// Where the script leaves the binary it built.
const BUILT_BINARY: &str = "target/device-container/release/paperctl";

/// `paperctl deploy`.
#[derive(Debug, Args)]
pub(crate) struct DeployArgs {
    /// Print the build and install commands without running either.
    #[arg(long)]
    dry_run: bool,
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

pub(crate) fn run(args: &DeployArgs) -> Result<(), CommandError> {
    let argv = build_argv();
    let install = install_command();

    if args.dry_run {
        println!("build    {}", argv.join(" "));
        println!("install  {install}");
        println!("dry run: nothing was built and no device was reached");
        return Ok(());
    }

    let (host, _source) =
        remote::resolve_and_announce_with_timeout(args.device.as_deref(), QUICK_PROBE_TIMEOUT)?;

    run_build(&argv)?;
    install_to(
        remote::default_runner(),
        &host,
        Path::new(BUILT_BINARY),
        &install,
    )?;
    println!("installed {BUILT_BINARY} -> {host}:{REMOTE_PAPERCTL}");
    Ok(())
}

/// `tools/cross/build-device.sh --bin paperctl`'s argv — what the retired
/// shell alias ran by hand.
fn build_argv() -> Vec<String> {
    vec![
        BUILD_SCRIPT.to_owned(),
        "--bin".to_owned(),
        "paperctl".to_owned(),
    ]
}

/// The one remote command line that gets a freshly built binary into place:
/// staged, made executable, then moved over the live path in a single
/// remote call, so there is never a half-written `paperctl` at
/// [`REMOTE_PAPERCTL`] for something else to try to run.
fn install_command() -> String {
    let staged = format!("{REMOTE_PAPERCTL}.new");
    format!("cat > {staged} && chmod +x {staged} && mv {staged} {REMOTE_PAPERCTL}")
}

/// Runs the local cross-compile. Not exercised by a test in this crate: it
/// shells out to Docker and a Rust toolchain install, the same as
/// `tools/cross/build-device.sh` itself is nowhere unit-tested.
fn run_build(argv: &[String]) -> Result<(), CommandError> {
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|source| CommandError::Deploy {
            detail: format!("cannot run `{}`: {source}", argv.join(" ")),
        })?;
    if !status.success() {
        return Err(CommandError::Deploy {
            detail: format!("`{}` exited with {status}", argv.join(" ")),
        });
    }
    Ok(())
}

/// Sends `built_binary` to `host` and installs it — the part of `deploy`
/// that reaches the tablet. Split from [`run`] so a test can inject an
/// [`SshRunner`] fake without also invoking a real cross-compile.
///
/// One blocking call, never detached: a binary copy takes seconds, not the
/// long hold `open` needs to survive an SSH session dying (WWW-23) — that
/// rule is `open`'s alone, and this must not blur into it.
fn install_to(
    ssh: &dyn SshRunner,
    host: &str,
    built_binary: &Path,
    install_command: &str,
) -> Result<(), CommandError> {
    let output = ssh
        .run_with_stdin(host, install_command, built_binary)
        .map_err(CommandError::Transport)?;
    print!("{}", output.stdout);
    if output.exit_code != 0 {
        return Err(CommandError::Transport(TransportError::RemoteFailed {
            host: host.to_owned(),
            exit_code: output.exit_code,
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::remote::CapturedOutput;
    use crate::transport::test_doubles::FakeSsh;

    #[test]
    fn the_install_command_stages_chmods_and_moves_atomically() {
        assert_eq!(
            install_command(),
            format!(
                "cat > {REMOTE_PAPERCTL}.new && chmod +x {REMOTE_PAPERCTL}.new && \
                 mv {REMOTE_PAPERCTL}.new {REMOTE_PAPERCTL}"
            )
        );
    }

    #[test]
    fn the_build_argv_is_the_bin_paperctl_form_the_retired_alias_used() {
        assert_eq!(build_argv(), vec![BUILD_SCRIPT, "--bin", "paperctl"]);
    }

    #[test]
    fn install_sends_the_exact_remote_command_line_and_never_detaches() {
        let ssh = FakeSsh {
            stdin_result: CapturedOutput {
                stdout: String::new(),
                exit_code: 0,
            },
            ..FakeSsh::default()
        };
        let binary = Path::new("/tmp/paperctl-built-for-a-test");

        install_to(&ssh, "tablet.local", binary, &install_command()).expect("ok");

        let calls = ssh.stdin_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "tablet.local");
        assert_eq!(calls[0].1, install_command());
        assert_eq!(calls[0].2, binary);

        assert!(
            ssh.detached_calls.borrow().is_empty(),
            "deploy must never detach — only a long `open` hold does (WWW-23)"
        );
        assert!(ssh.poll_calls.borrow().is_empty());
    }

    #[test]
    fn a_nonzero_install_exit_is_reported() {
        let ssh = FakeSsh {
            stdin_result: CapturedOutput {
                stdout: "mv: cannot stat".to_owned(),
                exit_code: 1,
            },
            ..FakeSsh::default()
        };

        let error = install_to(
            &ssh,
            "tablet.local",
            Path::new("/tmp/x"),
            &install_command(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CommandError::Transport(TransportError::RemoteFailed { exit_code: 1, .. })
        ));
    }
}
