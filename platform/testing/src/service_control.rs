//! A fake for `paper_device::stock::ServiceControl`.
//!
//! Replaces `platform/device/src/takeover.rs`'s test-only `FakeService` and
//! `WatchingService` (WWW-46): the same recording behaviour, plus an
//! `on_start` hook so the one test that needed to observe something at the
//! exact moment `start` is called — "the vendor lock is put back before
//! stock is started", the regression test for the 2026-09-17 incident — can
//! configure that hook on the shared fake instead of defining its own
//! one-off `ServiceControl` implementation.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use paper_device::error::DeviceError;
use paper_device::stock::ServiceControl;

/// Records every call `Stock` makes, in order, and can be told to misbehave.
#[derive(Default)]
pub struct FakeServiceControl {
    log: Rc<RefCell<Vec<String>>>,
    active: bool,
    /// Starts that silently do not bring the unit up.
    failed_starts_remaining: u32,
    stop_fails: bool,
    /// Run at the start of every [`ServiceControl::start`] call, before the
    /// fake updates its own state — the hook a test uses to read something
    /// else (a lock file, a registry) at that exact moment.
    #[allow(clippy::type_complexity)]
    on_start: Option<Rc<dyn Fn()>>,
}

impl std::fmt::Debug for FakeServiceControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeServiceControl")
            .field("log", &self.log)
            .field("active", &self.active)
            .field("failed_starts_remaining", &self.failed_starts_remaining)
            .field("stop_fails", &self.stop_fails)
            .field("on_start", &self.on_start.is_some())
            .finish()
    }
}

impl FakeServiceControl {
    /// A service that starts out active, records nothing yet, and never
    /// misbehaves until told to.
    pub fn new() -> Self {
        Self {
            active: true,
            ..Self::default()
        }
    }

    /// A handle onto the call log, to keep after `self` is moved into a
    /// `Stock`.
    pub fn log_handle(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.log)
    }

    /// Every call so far, in order (`"is-active"`, `"stop"`, `"reset-failed"`,
    /// `"start"`, `"wait-active"`).
    pub fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    /// The next `n` starts accept but do not bring the unit up.
    #[must_use]
    pub fn failing_starts(mut self, n: u32) -> Self {
        self.failed_starts_remaining = n;
        self
    }

    /// Every `stop` call is refused.
    #[must_use]
    pub fn refusing_stop(mut self) -> Self {
        self.stop_fails = true;
        self
    }

    /// Runs `hook` at the moment `start` is called, before this fake updates
    /// its own state.
    #[must_use]
    pub fn on_start(mut self, hook: impl Fn() + 'static) -> Self {
        self.on_start = Some(Rc::new(hook));
        self
    }

    fn note(&self, what: &str) {
        self.log.borrow_mut().push(what.to_owned());
    }
}

impl ServiceControl for FakeServiceControl {
    fn is_active(&mut self) -> Result<bool, DeviceError> {
        self.note("is-active");
        Ok(self.active)
    }

    fn stop(&mut self) -> Result<(), DeviceError> {
        self.note("stop");
        if self.stop_fails {
            return Err(DeviceError::unexpected("stop refused"));
        }
        self.active = false;
        Ok(())
    }

    fn reset_failed(&mut self) -> Result<(), DeviceError> {
        self.note("reset-failed");
        Ok(())
    }

    fn start(&mut self) -> Result<(), DeviceError> {
        if let Some(hook) = &self.on_start {
            hook();
        }
        self.note("start");
        if self.failed_starts_remaining > 0 {
            self.failed_starts_remaining -= 1;
        } else {
            self.active = true;
        }
        Ok(())
    }

    fn wait_active(&mut self, _timeout: Duration) -> Result<bool, DeviceError> {
        self.note("wait-active");
        Ok(self.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_calls_in_order() {
        let mut service = FakeServiceControl::new();
        service.stop().expect("stops");
        service.reset_failed().expect("resets");
        service.start().expect("starts");
        assert_eq!(service.calls(), vec!["stop", "reset-failed", "start"]);
    }

    #[test]
    fn failing_starts_do_not_bring_the_unit_up() {
        let mut service = FakeServiceControl::new().failing_starts(2);
        service.stop().expect("stops");
        service.start().expect("accepted");
        assert!(!service.is_active().expect("reads"));
        service.start().expect("accepted");
        assert!(!service.is_active().expect("reads"));
        service.start().expect("accepted");
        assert!(
            service.is_active().expect("reads"),
            "the third start comes up"
        );
    }

    #[test]
    fn refusing_stop_returns_an_error() {
        let mut service = FakeServiceControl::new().refusing_stop();
        service.stop().expect_err("refused");
    }

    #[test]
    fn on_start_runs_before_the_fake_updates_its_own_state() {
        let seen = Rc::new(RefCell::new(false));
        let seen_in_hook = Rc::clone(&seen);
        let mut service = FakeServiceControl::new().on_start(move || {
            *seen_in_hook.borrow_mut() = true;
        });
        service.stop().expect("stops");
        assert!(!*seen.borrow());
        service.start().expect("starts");
        assert!(*seen.borrow());
    }
}
