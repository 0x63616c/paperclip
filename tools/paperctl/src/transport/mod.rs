//! The Mac/device boundary: resolving which tablet to talk to, and reaching
//! it over SSH (WWW-33).
//!
//! Nothing on the device needs this. Every device-touching subcommand
//! (`open`, `stock`, `setup`, `install`, `upgrade`, `remove`) already has a
//! local implementation that runs correctly when `paperctl` itself is
//! executing on the tablet — that path is untouched. What was missing is the
//! other side: typed on a Mac, `paperctl open` has to find the tablet and run
//! itself there. This module is that other side, and only that side; it is
//! never reached by a Linux build (`#[cfg(not(target_os = "linux"))]` gates
//! every call site).
//!
//! `system_ssh` prefers the system `ssh` binary over an in-process SSH
//! implementation, per the issue's own instruction: it already carries the
//! user's config, keys and agent, and a second implementation would be a
//! second thing to keep secure.

pub(crate) mod config;
pub(crate) mod devices;
pub(crate) mod discover;
// `paperctl devices` (list/pin/unpin) is useful on either side and stays
// unconditional; the SSH execution engine is only ever called from a
// `#[cfg(not(target_os = "linux"))]` dispatch site, so it is gated the same
// way rather than left to look reachable on a device build that never calls
// it.
#[cfg(not(target_os = "linux"))]
pub(crate) mod remote;
// The Mac-side run history behind `paperctl logs` (WWW-34). `open` and
// `deploy` are the writers; both are Mac-only, so this is gated the same way
// as `remote` rather than compiled into a device build that never uses it.
#[cfg(not(target_os = "linux"))]
pub(crate) mod runlog;
#[cfg(not(target_os = "linux"))]
pub(crate) mod test_doubles;

use clap::Args;

/// Table for a terminal, or JSON for a script — shared by every command that
/// offers both (`devices`, `doctor`, `logs`) so the flag looks and behaves
/// identically everywhere it appears, rather than three near-identical
/// per-command enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OutputFormat {
    Table,
    Json,
}

/// The tablet to reach. Flattened into every device-touching subcommand, and
/// only compiled on the Mac side — on the device there is nothing to reach,
/// so the flag does not exist there rather than existing and doing nothing.
#[derive(Debug, Args, Default)]
pub(crate) struct DeviceArgs {
    /// The tablet to reach, as `ssh` would take it (`host` or `user@host`).
    ///
    /// Overrides auto-discovery and any pin. See `paperctl devices --help`
    /// for the full resolution order.
    // `global = true`: harmless where it is flattened straight into a leaf
    // command, and what lets `paperctl upgrade --device <host> run ...` and
    // `paperctl upgrade run ... --device <host>` both work for the one
    // subcommand group (`upgrade`) that nests further subcommands.
    #[cfg(not(target_os = "linux"))]
    #[arg(long, global = true, value_name = "HOST")]
    pub(crate) device: Option<String>,
}

#[cfg(not(target_os = "linux"))]
impl DeviceArgs {
    pub(crate) fn as_deref(&self) -> Option<&str> {
        self.device.as_deref()
    }
}

/// Reads `PAPERCTL_DEVICE`, the second-highest resolution source.
pub(crate) fn device_env() -> Option<String> {
    std::env::var("PAPERCTL_DEVICE")
        .ok()
        .filter(|value| !value.is_empty())
}
