//! Network connectivity, behind a seam.
//!
//! Built on [`Process`] rather than a socket API, the same way
//! [`Systemctl`](crate::Systemctl) is built on it: this project's backup
//! constraints already record that NetworkManager owns Wi-Fi on this device
//! (the connection profiles `nmcli` reads are exactly what the WWW-1 backup's
//! `NetworkManager profiles with Wi-Fi PSKs` refers to), so `nmcli` is the
//! interface this reads rather than a lower-level one this workspace would
//! have to reverse-engineer.
//!
//! **Unverified on hardware.** Nothing has run this against the tablet's own
//! `nmcli` yet — whether it is on `PATH` in the environment Paperclip's
//! processes run under, and whether its terse output matches the shape
//! assumed here, are both open until a device session checks them.

use std::fmt;

use crate::process::{Process, ProcessCommand, SystemProcess};

/// One reading of network connectivity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkReading {
    /// Whether an access point reported itself active.
    pub connected: bool,
    /// The network's name, when connected.
    pub ssid: Option<String>,
    /// Signal strength, 0–100, when connected.
    pub signal_percent: Option<u8>,
}

impl NetworkReading {
    /// No connection at all — `nmcli` ran and named nothing active.
    pub const DISCONNECTED: Self = Self {
        connected: false,
        ssid: None,
        signal_percent: None,
    };
}

/// Reading network connectivity, behind a seam.
pub trait Network: fmt::Debug {
    /// The current reading, or `None` when the backend could not be asked at
    /// all — `nmcli` missing or failing to run, as opposed to `nmcli` running
    /// and honestly reporting nothing connected (see [`NetworkReading::DISCONNECTED`]).
    fn read(&self) -> Option<NetworkReading>;
}

/// [`Network`] over `nmcli -t -f active,ssid,signal dev wifi`, via an
/// injected [`Process`] — the same composition [`Systemctl`](crate::Systemctl)
/// uses over `systemctl`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NmcliNetwork<P: Process = SystemProcess> {
    process: P,
}

impl<P: Process> NmcliNetwork<P> {
    /// A reader that asks `nmcli` through `process`.
    pub fn new(process: P) -> Self {
        Self { process }
    }
}

impl<P: Process> Network for NmcliNetwork<P> {
    fn read(&self) -> Option<NetworkReading> {
        let command =
            ProcessCommand::new("nmcli").args(["-t", "-f", "active,ssid,signal", "dev", "wifi"]);
        let output = self.process.run(&command).ok()?;
        if !output.success {
            return None;
        }
        Some(
            output
                .stdout
                .lines()
                .find_map(parse_active_line)
                .unwrap_or(NetworkReading::DISCONNECTED),
        )
    }
}

/// Parses one `nmcli -t` line, returning `Some` only for the active network.
///
/// `nmcli`'s terse mode escapes `:` and `\` inside a field as `\:` and `\\`;
/// this unescapes both because an SSID is free-text and either character is
/// legal in one.
fn parse_active_line(line: &str) -> Option<NetworkReading> {
    let fields = split_unescaped(line);
    let [active, ssid, signal] = fields.as_slice() else {
        return None;
    };
    if active != "yes" {
        return None;
    }
    Some(NetworkReading {
        connected: true,
        ssid: (!ssid.is_empty()).then(|| unescape(ssid)),
        signal_percent: signal.parse().ok(),
    })
}

/// Splits on `:`, treating `\:` as a literal colon rather than a separator.
fn split_unescaped(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ':' => {
                fields.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    fields.push(current);
    fields
}

/// Undoes `nmcli`'s own escaping within a field already split by
/// [`split_unescaped`] — `\\` becomes `\`.
fn unescape(field: &str) -> String {
    field.replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessOutput;

    #[derive(Debug)]
    struct StaticProcess(ProcessOutput);

    impl Process for StaticProcess {
        fn run(
            &self,
            _command: &ProcessCommand,
        ) -> Result<ProcessOutput, crate::process::ProcessError> {
            Ok(self.0.clone())
        }
    }

    fn success(stdout: &str) -> StaticProcess {
        StaticProcess(ProcessOutput {
            success: true,
            code: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    #[test]
    fn the_active_line_is_reported_connected_with_its_ssid_and_signal() {
        let process = success("no:OtherWifi:40\nyes:Home Network:78\n");
        let reading = NmcliNetwork::new(process).read().unwrap();
        assert!(reading.connected);
        assert_eq!(reading.ssid.as_deref(), Some("Home Network"));
        assert_eq!(reading.signal_percent, Some(78));
    }

    #[test]
    fn no_active_line_reads_as_honestly_disconnected() {
        let process = success("no:OtherWifi:40\n");
        let reading = NmcliNetwork::new(process).read().unwrap();
        assert_eq!(reading, NetworkReading::DISCONNECTED);
    }

    #[test]
    fn an_escaped_colon_in_the_ssid_does_not_split_the_line() {
        let process = success("yes:Cafe\\:Free Wifi:55\n");
        let reading = NmcliNetwork::new(process).read().unwrap();
        assert_eq!(reading.ssid.as_deref(), Some("Cafe:Free Wifi"));
        assert_eq!(reading.signal_percent, Some(55));
    }

    #[test]
    fn a_failed_nmcli_invocation_is_none_not_a_fabricated_reading() {
        let process = StaticProcess(ProcessOutput {
            success: false,
            code: Some(1),
            stdout: String::new(),
            stderr: "nmcli: command not found".to_owned(),
        });
        assert!(NmcliNetwork::new(process).read().is_none());
    }
}
