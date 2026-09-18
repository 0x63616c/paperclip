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
// The `RemoteCommand` trait and the shared resolve → banner → run-log → run
// sequence (WWW-48). Gated with `remote`, which it is built entirely on top
// of.
#[cfg(not(target_os = "linux"))]
pub(crate) mod dispatch;
// `paperctl devices` (list/pin/unpin) is useful on either side and stays
// unconditional; the SSH execution engine is only ever called from a
// `#[cfg(not(target_os = "linux"))]` dispatch site, so it is gated the same
// way rather than left to look reachable on a device build that never calls
// it.
#[cfg(not(target_os = "linux"))]
pub(crate) mod remote;
// Not gated, unlike `remote` and `runlog`: `FakeProber` is used by
// `discover`'s own tests, which run on both sides. The Mac-only half
// (`FakeSsh`) carries the gate instead, inside the module.
pub(crate) mod test_doubles;

use clap::Args;

/// Where `paper_telemetry::run_log`'s records live: alongside the device
/// config (the pin and cache), one existing directory convention rather than
/// a second one to keep synchronised with it (ADR-0023).
///
/// The run-log itself moved to `platform/telemetry` (WWW-46) so it could
/// become a `tracing` layer installed once in `main`, rather than a function
/// every dispatch site had to remember to call — but *where* it writes is
/// still paperctl's own convention, not telemetry's to hardcode.
#[cfg(not(target_os = "linux"))]
pub(crate) fn run_log_dir() -> std::path::PathBuf {
    config::default_path()
        .parent()
        .expect("the config path always has a parent directory")
        .join("runs")
}

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
