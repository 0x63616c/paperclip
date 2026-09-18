//! `paperclip-compositor` — the one process that opens the panel while
//! Paperclip owns the display (WWW-81).
//!
//! Started by `paperclip-compositor.service`, written into
//! `/run/systemd/system` alongside the supervisor's own units and never
//! installed (§8, ADR-0008). Long-running for the whole session — unlike
//! `paperclip-app@.service`, which is started and stopped per foreground
//! owner, this process stays up across an app-to-app switch and is only
//! stopped when the session itself ends (going back to stock).
//!
//! Binds a [`Compositor`] to `--socket` and drives it until asked to stop,
//! writing a small status file to `--status` on every change so the
//! supervisor (which holds no socket of its own to this process, the same
//! "control arrives through a file" choice `paper_host::linux::runtime`
//! makes for its own command file) can tell which client, if any, is
//! foreground.

#![allow(
    unsafe_code,
    reason = "installing a signal handler needs a raw libc call; see below"
)]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use paper_compositor::Compositor;
use paper_device::{MemoryPanel, Panel};
use paper_sdk::SCREEN;

/// How often `run_once` polls, bounded so the shutdown flag below is never
/// stale for longer than this.
const TICK: Duration = Duration::from_millis(200);

/// Set from a `SIGTERM`/`SIGINT` handler; checked once per tick.
///
/// A `static AtomicBool` rather than a channel or a blocking wait: the only
/// thing a signal handler may safely do is a small set of async-signal-safe
/// operations, and setting a flag is the one this binary needs (`std::io`,
/// allocation and anything that can block are all out).
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_signum: libc::c_int) {
    SHOULD_STOP.store(true, Ordering::SeqCst);
}

fn main() -> ExitCode {
    // Journald is Linux-only, same split `paperclip-host`'s bin makes. This
    // binary's own logic beyond that is portable and stays testable on the
    // Mac against `MemoryPanel`; only the device's actual logging sink is
    // gated.
    #[cfg(target_os = "linux")]
    if let Err(error) = paper_telemetry::init_journald() {
        eprintln!("paperclip-compositor: telemetry did not start: {error}");
    }

    let mut socket: Option<PathBuf> = None;
    let mut status: Option<PathBuf> = None;
    let mut panel_kind = default_panel_kind();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => socket = arguments.next().map(PathBuf::from),
            "--status" => status = arguments.next().map(PathBuf::from),
            "--panel" => match arguments.next().as_deref() {
                Some("memory") => panel_kind = PanelKind::Memory,
                Some("vendor") => panel_kind = PanelKind::Vendor,
                other => {
                    tracing::error!("--panel expects `memory` or `vendor`, got {other:?}");
                    return ExitCode::FAILURE;
                }
            },
            other => {
                tracing::error!("unexpected argument `{other}`");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(socket) = socket else {
        tracing::error!("--socket is required");
        return ExitCode::FAILURE;
    };

    install_signal_handlers();

    let panel = match open_panel(panel_kind) {
        Ok(panel) => panel,
        Err(error) => {
            tracing::error!("could not open the panel: {error}");
            return ExitCode::FAILURE;
        }
    };

    // A previous run's socket file survives an unclean stop (systemd's own
    // stop path frees the *bind*, not the directory entry); binding a stale
    // path fails with `AddrInUse` rather than replacing it.
    let _ = std::fs::remove_file(&socket);

    let mut compositor = match Compositor::bind(&socket, panel) {
        Ok(compositor) => compositor,
        Err(error) => {
            tracing::error!("could not bind {}: {error}", socket.display());
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(socket = %socket.display(), "compositor bound, awaiting clients");
    write_status(status.as_deref(), &compositor);

    while !SHOULD_STOP.load(Ordering::SeqCst) {
        let events = compositor.run_once(TICK);
        if !events.is_empty() {
            for event in &events {
                tracing::debug!(?event, "compositor event");
            }
            write_status(status.as_deref(), &compositor);
        }
    }

    tracing::info!("stop requested, clearing the panel before exit");
    // ADR-0009's shutdown ordering: clear, then drop the panel (closing the
    // vendor handle, freeing its buffers) before this process exits and
    // systemd proceeds to restart stock.
    if let Err(error) = compositor_panel_clear(&mut compositor) {
        tracing::error!("clearing the panel on the way out failed: {error}");
    }
    drop(compositor);
    let _ = std::fs::remove_file(&socket);
    ExitCode::SUCCESS
}

#[derive(Debug, Clone, Copy)]
enum PanelKind {
    Memory,
    Vendor,
}

const fn default_panel_kind() -> PanelKind {
    if cfg!(feature = "vendor-engine") {
        PanelKind::Vendor
    } else {
        PanelKind::Memory
    }
}

fn open_panel(kind: PanelKind) -> Result<Box<dyn Panel>, String> {
    match kind {
        PanelKind::Memory => Ok(Box::new(MemoryPanel::new(SCREEN))),
        #[cfg(feature = "vendor-engine")]
        PanelKind::Vendor => paper_device::vendor::VendorPanel::open()
            .map(|panel| Box::new(panel) as Box<dyn Panel>)
            .map_err(|error| error.to_string()),
        #[cfg(not(feature = "vendor-engine"))]
        PanelKind::Vendor => {
            Err("this build was not compiled with the `vendor-engine` feature".to_owned())
        }
    }
}

fn compositor_panel_clear(compositor: &mut Compositor) -> Result<(), String> {
    // `Compositor` owns the panel privately; there is no accessor because
    // nothing before this ticket needed one (ADR-0033/0037 built it as a
    // library with the panel fully encapsulated). Shutdown is the one
    // moment this binary needs to reach past that, so it goes through the
    // one seam the library already exposes for it.
    compositor
        .panel_mut()
        .clear()
        .map_err(|error| error.to_string())
}

/// Installs a handler for `SIGTERM` and `SIGINT` that only ever sets
/// [`SHOULD_STOP`].
///
/// # Safety-relevant note
///
/// `libc::signal` is not `unsafe` to call in the Rust sense used elsewhere in
/// this workspace (it touches no memory this process does not own), but
/// installing a handler is inherently process-global mutable state, which is
/// why this is its own narrow function rather than inlined into `main`.
fn install_signal_handlers() {
    // SAFETY: `request_stop` is `extern "C"`, touches only an `AtomicBool`
    // (async-signal-safe), and never panics or allocates — the only things a
    // signal handler installed this way may safely do. `libc::signal`'s
    // second argument is a function pointer of the correct `sighandler_t`
    // shape once cast.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_stop as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            request_stop as *const () as libc::sighandler_t,
        );
    }
}

/// Writes `state=<foreground|stopped:<label>|none>` plus the connected
/// clients' labels, so the supervisor can tell whether the client it is
/// waiting on has arrived without holding a connection to this process.
fn write_status(path: Option<&std::path::Path>, compositor: &Compositor) {
    let Some(path) = path else { return };
    let foreground = compositor
        .foreground_client()
        .and_then(|id| compositor.client_label(id))
        .map(|label| format!("client:{label}"))
        .or_else(|| {
            compositor
                .stopped_label()
                .map(|label| format!("stopped:{label}"))
        })
        .unwrap_or_else(|| "none".to_owned());
    let contents = format!("foreground={foreground}\n");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Best effort, matching `Supervisor::write_status`: a status file the
    // supervisor cannot read yet is not this process's failure to report.
    if let Ok(mut file) = std::fs::File::create(path) {
        let _ = file.write_all(contents.as_bytes());
    }
}
