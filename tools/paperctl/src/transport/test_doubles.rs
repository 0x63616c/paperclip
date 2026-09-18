//! Shared test doubles for the Mac-side device transport.
//!
//! `remote`, `doctor` and `deploy` each need to assert the exact command
//! line they send over SSH, and `doctor` also needs to fake reachability —
//! all without touching a real network or spawning a real `ssh`. One fake
//! per role here is what keeps those assertions from drifting into slightly
//! different doubles in each module.

#![cfg(test)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::discover::Prober;
use super::remote::{CapturedOutput, SshRunner, TransportError};

#[derive(Default)]
pub(crate) struct FakeSsh {
    pub(crate) blocking_calls: RefCell<Vec<(String, Vec<String>)>>,
    pub(crate) detached_calls: RefCell<Vec<(String, Vec<String>, String)>>,
    pub(crate) poll_calls: RefCell<Vec<(String, String)>>,
    pub(crate) capture_calls: RefCell<Vec<(String, String)>>,
    pub(crate) stdin_calls: RefCell<Vec<(String, String, PathBuf)>>,
    pub(crate) blocking_result: i32,
    /// Popped in reverse order — the *last* item here is the *first* poll
    /// result — matching how `remote.rs`'s own tests already build one.
    pub(crate) poll_results: RefCell<Vec<Option<i32>>>,
    pub(crate) capture_result: CapturedOutput,
    pub(crate) stdin_result: CapturedOutput,
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

    fn run_capture(
        &self,
        host: &str,
        remote_command: &str,
    ) -> Result<CapturedOutput, TransportError> {
        self.capture_calls
            .borrow_mut()
            .push((host.to_owned(), remote_command.to_owned()));
        Ok(self.capture_result.clone())
    }

    fn run_with_stdin(
        &self,
        host: &str,
        remote_command: &str,
        local_file: &Path,
    ) -> Result<CapturedOutput, TransportError> {
        self.stdin_calls.borrow_mut().push((
            host.to_owned(),
            remote_command.to_owned(),
            local_file.to_path_buf(),
        ));
        Ok(self.stdin_result.clone())
    }
}

/// A [`Prober`] whose answers are fixed by the test, so `doctor`'s and
/// `discover`'s reachability logic are both checkable without a network. One
/// copy, not two: this used to have a private duplicate inside `discover`'s
/// own test module (WWW-46).
#[derive(Default)]
pub(crate) struct FakeProber {
    reachable: RefCell<Vec<String>>,
    mdns: Vec<String>,
    reachable_calls: RefCell<Vec<(String, Duration)>>,
    mdns_calls: RefCell<Vec<Duration>>,
}

impl FakeProber {
    pub(crate) fn reachable_hosts(hosts: &[&str]) -> Self {
        Self {
            reachable: RefCell::new(hosts.iter().map(|h| (*h).to_owned()).collect()),
            ..Self::default()
        }
    }

    /// Also answers `mdns_candidates` with `hosts`.
    #[must_use]
    pub(crate) fn with_mdns_candidates(self, hosts: &[&str]) -> Self {
        Self {
            mdns: hosts.iter().map(|h| (*h).to_owned()).collect(),
            ..self
        }
    }

    /// Every `(host, timeout)` passed to [`Prober::reachable`], in call order.
    pub(crate) fn reachable_calls(&self) -> Vec<(String, Duration)> {
        self.reachable_calls.borrow().clone()
    }

    /// Every timeout passed to [`Prober::mdns_candidates`], in call order.
    pub(crate) fn mdns_calls(&self) -> Vec<Duration> {
        self.mdns_calls.borrow().clone()
    }
}

impl Prober for FakeProber {
    fn reachable(&self, host: &str, timeout: Duration) -> bool {
        self.reachable_calls
            .borrow_mut()
            .push((host.to_owned(), timeout));
        self.reachable.borrow().iter().any(|h| h == host)
    }

    fn mdns_candidates(&self, timeout: Duration) -> Vec<String> {
        self.mdns_calls.borrow_mut().push(timeout);
        self.mdns.clone()
    }
}
