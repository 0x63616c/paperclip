//! Controlling one named systemd unit, for `platform/device`'s stock policy.
//!
//! `paper_device::stock::Stock` owns the rules about Xochitl — the start
//! budget, `reset-failed` before every start, one guarded retry and then stop
//! — and reaches systemd through
//! [`ServiceControl`](paper_device::stock::ServiceControl). Its own
//! implementation is hardcoded to `xochitl.service`, which is correct on the
//! tablet and impossible to exercise anywhere else.
//!
//! This is the same operations against a unit named at runtime, so the VM
//! harness can drive the real policy against a stand-in. That matters for one
//! case in particular: "Xochitl fails to start" is a §10 row, and arranging
//! for the real Xochitl to fail is precisely the thing that drops the tablet
//! onto a serial console.
//!
//! Like the trait, it has no `kill` and no `restart`, because neither is ever
//! correct.

use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use paper_device::error::DeviceError;
use paper_device::stock::ServiceControl;

/// [`ServiceControl`] over `systemctl`, for a unit named at construction.
#[derive(Debug, Clone)]
pub struct UnitControl {
    unit: String,
}

impl UnitControl {
    /// Controls `unit`.
    pub fn new(unit: &str) -> Self {
        Self {
            unit: unit.to_owned(),
        }
    }

    /// Which unit this controls.
    pub fn unit(&self) -> &str {
        &self.unit
    }

    fn run(&self, arguments: &[&str]) -> Result<(bool, String), DeviceError> {
        let output = Command::new("systemctl")
            .args(arguments)
            .arg(&self.unit)
            .output()
            .map_err(|source| DeviceError::io("run systemctl for", &self.unit, source))?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    }
}

impl ServiceControl for UnitControl {
    fn is_active(&mut self) -> Result<bool, DeviceError> {
        Ok(self.run(&["is-active"])?.1 == "active")
    }

    fn stop(&mut self) -> Result<(), DeviceError> {
        let (ok, text) = self.run(&["stop"])?;
        if ok {
            Ok(())
        } else {
            Err(DeviceError::unexpected(format!(
                "systemctl stop {} failed: {text}",
                self.unit
            )))
        }
    }

    fn reset_failed(&mut self) -> Result<(), DeviceError> {
        // Not fatal: the unit may simply not be failed.
        let _ = self.run(&["reset-failed"])?;
        Ok(())
    }

    fn start(&mut self) -> Result<(), DeviceError> {
        // Deliberately not an error. `start` can report failure while the unit
        // still comes up, and the caller decides on `wait_active` — treating
        // it as fatal here would tempt a retry that must not happen.
        let _ = self.run(&["start"])?;
        Ok(())
    }

    fn wait_active(&mut self, timeout: Duration) -> Result<bool, DeviceError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_active()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
}
