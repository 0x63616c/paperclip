//! Battery state, behind a seam.
//!
//! Reads the generic Linux `power_supply` class rather than a hard-coded
//! device path. WWW-1 never confirmed which `power_supply` node this tablet
//! exposes for its cell, and this project has already paid once for guessing
//! a hardware detail instead of scanning for it (the 4bpp-packing premise the
//! project description records) — scanning for whichever node reports
//! `type: Battery` is the same lesson applied here: it works on any node name
//! without a claim about which one this device uses.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Which direction the battery is moving, coarsened from the kernel's own
/// `status` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeDirection {
    /// Drawing power from the mains and gaining charge.
    Charging,
    /// Running on battery.
    Discharging,
    /// Charging and at capacity.
    Full,
    /// The node reported something this reader does not recognise.
    Unknown,
}

/// One reading of the battery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryReading {
    /// 0–100, clamped to that range in case a driver reports slightly over.
    pub percent: u8,
    /// Which way it is moving.
    pub direction: ChargeDirection,
}

/// Reading the battery, behind a seam.
pub trait PowerSource: fmt::Debug {
    /// The current reading, or `None` when no battery node could be found or
    /// read — a device with no battery, and a device whose battery node this
    /// reader could not open, look the same from here on purpose: either way
    /// there is nothing honest to report.
    fn read(&self) -> Option<BatteryReading>;
}

/// The real one: scans `/sys/class/power_supply` for a `Battery`-typed node.
#[derive(Debug, Clone)]
pub struct SystemPowerSource {
    root: PathBuf,
}

impl Default for SystemPowerSource {
    fn default() -> Self {
        Self::new(Path::new("/sys/class/power_supply"))
    }
}

impl SystemPowerSource {
    /// A reader scanning `root` for the battery node — `/sys/class/power_supply`
    /// on the real device, a fixture directory in a test.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl PowerSource for SystemPowerSource {
    fn read(&self) -> Option<BatteryReading> {
        let entries = fs::read_dir(&self.root).ok()?;
        for entry in entries.flatten() {
            let node = entry.path();
            let Ok(kind) = fs::read_to_string(node.join("type")) else {
                continue;
            };
            if kind.trim() != "Battery" {
                continue;
            }
            let Ok(percent) = fs::read_to_string(node.join("capacity")) else {
                continue;
            };
            let Ok(percent) = percent.trim().parse::<u8>() else {
                continue;
            };
            let status = fs::read_to_string(node.join("status")).unwrap_or_default();
            let direction = match status.trim() {
                "Charging" => ChargeDirection::Charging,
                "Discharging" => ChargeDirection::Discharging,
                "Full" => ChargeDirection::Full,
                _ => ChargeDirection::Unknown,
            };
            return Some(BatteryReading {
                percent: percent.min(100),
                direction,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(root: &Path, name: &str, kind: &str, capacity: &str, status: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("type"), kind).unwrap();
        fs::write(dir.join("capacity"), capacity).unwrap();
        fs::write(dir.join("status"), status).unwrap();
    }

    #[test]
    fn reads_the_node_whose_type_is_battery_and_skips_the_others() {
        let dir = tempfile::tempdir().unwrap();
        node(dir.path(), "ADP1", "Mains", "0", "Unknown");
        node(dir.path(), "BAT0", "Battery", "63", "Discharging");

        let reading = SystemPowerSource::new(dir.path()).read().unwrap();
        assert_eq!(reading.percent, 63);
        assert_eq!(reading.direction, ChargeDirection::Discharging);
    }

    #[test]
    fn an_unreadable_root_is_none_not_a_panic() {
        let reading = SystemPowerSource::new("/does/not/exist/at/all").read();
        assert!(reading.is_none());
    }

    #[test]
    fn a_status_this_reader_does_not_recognise_is_unknown_not_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        node(dir.path(), "BAT0", "Battery", "50", "Not charging");

        let reading = SystemPowerSource::new(dir.path()).read().unwrap();
        assert_eq!(reading.direction, ChargeDirection::Unknown);
    }
}
