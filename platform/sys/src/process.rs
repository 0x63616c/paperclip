//! Running a command, behind a seam.
//!
//! The primitive underneath `platform/device`'s `Systemctl`, `platform/host`'s
//! `Systemd`, and `tools/paperctl`'s SSH and docker invocations (WWW-46's
//! effects table): all four spawn a program and read back what it did. One
//! trait here, one real adapter, means [`UnitControl`](crate::UnitControl)'s
//! real implementation can be exercised in a test against a fake `Process`
//! instead of a real `systemctl` — the same win `platform/updater` already
//! banked for `SessionControl`.

use std::fmt;

/// A program and its arguments, built once and handed to a [`Process`].
///
/// Not `std::process::Command`: that type is not `Clone`, is not comparable,
/// and carries no way for a fake to inspect what was asked for. This is a
/// plain, comparable value so a test can assert on exactly what a caller
/// built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCommand {
    /// The program to run, e.g. `"systemctl"`.
    pub program: String,
    /// Its arguments, in order.
    pub args: Vec<String>,
}

impl ProcessCommand {
    /// A command with no arguments yet.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends every argument in `args`.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
}

/// What a finished process left behind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessOutput {
    /// Whether it exited zero.
    pub success: bool,
    /// The exit code, when the process was not killed by a signal.
    pub code: Option<i32>,
    /// Captured stdout, lossily decoded.
    pub stdout: String,
    /// Captured stderr, lossily decoded.
    pub stderr: String,
}

/// Why a process could not be run at all.
///
/// Distinct from a nonzero exit: [`ProcessOutput::success`] is how a caller
/// hears about a command that ran and refused, this is how it hears that the
/// program could not be started or waited on.
#[derive(Debug, thiserror::Error)]
#[error("could not run `{program}`: {reason}")]
pub struct ProcessError {
    /// The program that could not be run.
    pub program: String,
    /// Why.
    pub reason: String,
}

/// Spawning a program and waiting for it, behind a seam.
pub trait Process: fmt::Debug {
    /// Runs `command` to completion and returns what it left behind.
    ///
    /// # Errors
    ///
    /// If the program could not be started or its output could not be read.
    /// A nonzero exit is not an error here — see [`ProcessOutput::success`].
    fn run(&self, command: &ProcessCommand) -> Result<ProcessOutput, ProcessError>;
}

/// The real one: `std::process::Command`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcess;

impl Process for SystemProcess {
    fn run(&self, command: &ProcessCommand) -> Result<ProcessOutput, ProcessError> {
        let output = std::process::Command::new(&command.program)
            .args(&command.args)
            .output()
            .map_err(|source| ProcessError {
                program: command.program.clone(),
                reason: source.to_string(),
            })?;
        Ok(ProcessOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_process_captures_stdout_and_a_zero_exit() {
        let command = ProcessCommand::new("printf").arg("hi");
        let output = SystemProcess.run(&command).expect("runs");
        assert!(output.success);
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, "hi");
    }

    #[test]
    fn a_nonzero_exit_is_reported_not_returned_as_an_error() {
        let command = ProcessCommand::new("sh").args(["-c", "exit 3"]);
        let output = SystemProcess.run(&command).expect("runs");
        assert!(!output.success);
        assert_eq!(output.code, Some(3));
    }

    #[test]
    fn a_program_that_does_not_exist_is_an_error() {
        let command = ProcessCommand::new("paperclip-does-not-exist-anywhere");
        let error = SystemProcess.run(&command).expect_err("refuses");
        assert!(error.to_string().contains("paperclip-does-not-exist"));
    }

    #[test]
    fn building_a_command_records_program_and_args_in_order() {
        let command = ProcessCommand::new("systemctl")
            .arg("start")
            .args(["a.service"]);
        assert_eq!(command.program, "systemctl");
        assert_eq!(command.args, vec!["start", "a.service"]);
    }
}
