//! `paperctl run` — an interactive Home/Chess/Settings/Sudoku/App Store
//! session on the panel (§4, WWW-6).
//!
//! `paperctl open` puts one static screen on the glass and gives it back.
//! This is the same takeover, but for as long as someone is actually using
//! the tablet: it presents a frame, reads real evdev input, redraws on every
//! tap, follows a Home shelf tap into Chess, follows `Action::Home` back, and
//! ends the session — panel cleared, stock restored — on `ReturnToStock`.
//!
//! ## The same split as `open`, for the same reason
//!
//! [`paper_device::hold`]'s module doc says it plainly: `EPFramebuffer` is a
//! singleton whose lock only a process *exiting* releases, so the process
//! that opens the vendor engine can never itself start Xochitl. `open`
//! answers that with a parent that never touches the engine and a
//! short-lived, re-exec'd child that does and then exits; this command uses
//! exactly that split; the only difference is that the child's "one present"
//! is now a loop of them, driven by [`paper_device::open_and_run`] rather
//! than [`paper_device::open_and_hold`] — see that function's doc for what it
//! drops (the single-frame digest agreement) and why an interactive session
//! cannot have that check.
//!
//! ## What is out of scope here
//!
//! Sleep, lock and PIN entry belong to WWW-22 and need Calum at the tablet;
//! this command never requests or waits on a suspend. `paperctl dev --device`
//! — rebuild-on-trigger against the real panel — is not built in this pass;
//! see the WWW-6 report for why.

use std::path::PathBuf;

use crate::error::CommandError;

/// `paperctl run`.
#[derive(Debug, clap::Args)]
pub(crate) struct RunArgs {
    /// Which app to start the session on.
    #[arg(long, value_enum, default_value = "home")]
    app: RunAppArg,
    /// How long the session may run before the out-of-process watchdog
    /// restores stock on its own, in seconds.
    ///
    /// Not a display hold: the session keeps running, presenting whatever a
    /// person taps, until `ReturnToStock` or this budget's watchdog margin
    /// (`paper_device::hold::WATCHDOG_MARGIN`) is reached — the same
    /// backstop `paperctl open`'s `--hold` feeds, sized for "a chess game",
    /// not "a photograph".
    #[arg(long, default_value_t = 1800)]
    session_budget: u64,
    /// Internal: be the presenting half, and nothing else.
    ///
    /// Opens the vendor engine, runs the interactive loop, writes its report
    /// to `--report-to` and exits. It never stops or starts Xochitl. Hidden
    /// because running it by hand takes the panel out from under a live
    /// Xochitl.
    #[arg(long, hide = true)]
    present_only: bool,
    /// Internal: where the presenting half leaves its report.
    #[arg(long, hide = true, value_name = "PATH")]
    report_to: Option<PathBuf>,
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

impl RunArgs {
    /// The argv a remote `paperctl run` on the tablet should be given —
    /// everything this half was, except `--present-only` and `--report-to`,
    /// which only mean something to the half that opens the vendor engine.
    #[cfg(not(target_os = "linux"))]
    fn remote_argv(&self) -> Vec<String> {
        vec![
            "run".to_owned(),
            "--app".to_owned(),
            self.app.slug().to_owned(),
            "--session-budget".to_owned(),
            self.session_budget.to_string(),
        ]
    }
}

/// Which app `paperctl run` can start on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum RunAppArg {
    /// The shelf.
    Home,
    /// The chess app, on the real rules core.
    Chess,
    /// Settings, over the real store this device has.
    Settings,
    /// The sudoku app, whose per-cell damage is the reason this loop presents
    /// regions rather than whole panels (WWW-6, WWW-39).
    Sudoku,
    /// The App Store, against `PAPERCLIP_ROOT`'s own store and catalog.
    AppStore,
}

impl RunAppArg {
    const fn slug(self) -> &'static str {
        match self {
            RunAppArg::Home => "home",
            RunAppArg::Chess => "chess",
            RunAppArg::Settings => "settings",
            RunAppArg::Sudoku => "sudoku",
            RunAppArg::AppStore => "app-store",
        }
    }
}

/// Runs the session, or explains why it will not start.
pub(crate) fn run(args: &RunArgs) -> Result<(), CommandError> {
    if args.present_only {
        return present_only(args);
    }
    println!("app      {}", args.app.slug());
    println!(
        "budget   {}s before the watchdog can act",
        args.session_budget
    );
    present(args)
}

/// On a Mac, this is a forward to the tablet's own `paperctl run` over SSH
/// (WWW-33), same as `open`, `stock`, `setup`, `install`, `upgrade` and
/// `remove` already do — WWW-35 found `run` was the one device-touching
/// subcommand still missing this and its `--device` flag, so a session
/// could only be started by SSHing onto the tablet by hand.
///
/// Run detached and polled, not blocking: WWW-23 found a live SSH session
/// does not reliably survive a long-lived takeover, and an interactive
/// session can run for the whole of `--session-budget` (default 30
/// minutes), far longer than any one SSH channel should be trusted for.
#[cfg(not(target_os = "linux"))]
fn present(args: &RunArgs) -> Result<(), CommandError> {
    let (host, source) = crate::transport::remote::resolve_device(args.device.as_deref())?;
    println!("device   {host} ({source})");
    paper_telemetry::run_log::record_device(&host);
    crate::transport::remote::run_open(
        &host,
        &args.remote_argv(),
        std::time::Duration::from_secs(args.session_budget),
    )?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn present_only(_args: &RunArgs) -> Result<(), CommandError> {
    Err(CommandError::NotOnDevice {
        what: "an interactive session on the panel",
    })
}

/// The presenting half: open the engine, run the session, write down what
/// happened, and — most importantly — **exit**. See the module doc.
#[cfg(target_os = "linux")]
fn present_only(args: &RunArgs) -> Result<(), CommandError> {
    if !paper_device::is_real_device() {
        return Err(CommandError::NoVendorEngine);
    }
    let record = interactive::run_on_panel(args.app.slug()).map_err(CommandError::Device)?;
    for line in &record.lines {
        println!("{line}");
    }
    if let Some(path) = args.report_to.as_deref() {
        record.write(path).map_err(CommandError::Device)?;
    }
    Ok(())
}

/// The surviving half: preflight, take the display, run the presenter as a
/// child, put stock back and check it. Never opens the vendor engine itself.
#[cfg(target_os = "linux")]
fn present(args: &RunArgs) -> Result<(), CommandError> {
    if !paper_device::is_real_device() {
        return Err(CommandError::NoVendorEngine);
    }
    let budget = std::time::Duration::from_secs(args.session_budget);
    let report = paper_device::open_and_run(budget, || spawn_presenter(args))
        .map_err(CommandError::Device)?;
    println!("{report}");
    if !report.stock_restored_cleanly() {
        return Err(CommandError::StockRegressed {
            regressions: if report.regressions.is_empty() {
                format!("stock is not healthy after the session: {:?}", report.after)
            } else {
                report.regressions.join("; ")
            },
        });
    }
    Ok(())
}

/// Runs this same binary as the presenting half and waits for it to finish.
///
/// See `open.rs::spawn_presenter` for why the wait is load-bearing and why
/// `/tmp`. This is that function, for `run` instead of `open`.
#[cfg(target_os = "linux")]
fn spawn_presenter(args: &RunArgs) -> Result<paper_device::PanelRecord, paper_device::DeviceError> {
    use std::process::Command;

    let binary = std::env::current_exe().map_err(|source| {
        paper_device::DeviceError::io("find this binary for", "paperctl", source)
    })?;
    let report =
        std::path::PathBuf::from(format!("/tmp/paperclip-run-{}.report", std::process::id()));
    let _ = std::fs::remove_file(&report);

    let status = Command::new(&binary)
        .arg("run")
        .arg("--present-only")
        .arg("--report-to")
        .arg(&report)
        .arg("--app")
        .arg(args.app.slug())
        .arg("--session-budget")
        .arg(args.session_budget.to_string())
        .status()
        .map_err(|source| {
            paper_device::DeviceError::io("run the presenting half of", &binary, source)
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&report);
        return Err(paper_device::DeviceError::unexpected(format!(
            "the presenting half exited with {status}; the display was not left taken, \
             but nothing was shown"
        )));
    }
    let record = paper_device::PanelRecord::read(&report);
    let _ = std::fs::remove_file(&report);
    record
}

/// The interactive loop itself — its own module so the safety-critical
/// parent/child split above stays readable next to `open.rs`'s.
#[cfg(target_os = "linux")]
mod interactive {
    use std::path::PathBuf;
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::time::Duration;

    use paper_device::{
        ContactIds, DeviceError, FrameDigest, InputRole, PanelRecord, PenDecoder, PixelRect,
        PointerTransform, Refresh, TouchDecoder, Waveform,
    };
    use paper_protocol::{ExitReason, Request};
    use paper_sdk::PointerEvent;

    use crate::session;

    /// Durable app storage on the device. Never `/tmp`: that is tmpfs and is
    /// cleared at reboot, and a saved chess game is exactly the kind of state
    /// this project's own rules say belongs under `/home/root/paperclip`.
    fn storage_root() -> PathBuf {
        PathBuf::from("/home/root/paperclip/apps")
    }

    /// Why the app loop ended.
    enum Exit {
        /// Switch to another app — a Home tap, or the app's own `Action::Home`.
        Switch(String),
        /// The person, or the app, asked to leave.
        ReturnToStock,
    }

    /// How many queued pointer samples to fold into one paint.
    ///
    /// Bounded so a stream of input cannot starve the exit checks below; the
    /// panel is the slow part, so this only needs to outrun one waveform.
    const MAX_COALESCED_POINTERS: usize = 64;

    /// Runs the whole interactive session against the real panel.
    ///
    /// This is the half that opens the vendor engine
    /// ([`paper_device::open_panel`]) and therefore the half that must exit
    /// before Xochitl can start again. Nothing here stops or starts Xochitl,
    /// takes the wakelock, or holds an opinion about systemd — that is
    /// entirely [`paper_device::open_and_run`]'s, in the parent process.
    pub(super) fn run_on_panel(app_slug: &str) -> Result<PanelRecord, DeviceError> {
        let mut panel = paper_device::open_panel()?;
        let real_panel = paper_device::is_real_device();
        let panel_size = panel.size();
        let storage_root = storage_root();

        let (sink, events) = mpsc::channel();
        spawn_input_readers(&sink)?;
        drop(sink);

        let mut current_slug = app_slug.to_owned();
        let mut first_digest: Option<String> = None;
        let mut frames_presented: u64 = 0;
        let mut first_present = true;
        let mut lines = Vec::new();

        loop {
            let mut current = session::open_session(
                &current_slug,
                &storage_root,
                "DEVICE",
                "paperctl run \u{2014} presenting on the panel",
            )
            .map_err(|error| DeviceError::unexpected(error.to_string()))?;

            let canvas = current.frame();
            let digest = FrameDigest::of(&canvas)?;
            if !digest.looks_drawn() {
                return Err(DeviceError::unexpected(format!(
                    "{current_slug}'s first frame is entirely background; refusing to present \
                     an empty panel"
                )));
            }
            if first_digest.is_none() {
                first_digest = Some(format!("sha256:{}", digest.to_hex()));
            }
            let refresh = if first_present {
                Refresh::Full
            } else {
                Refresh::Partial
            };
            first_present = false;
            paper_device::present(
                panel.as_mut(),
                &canvas,
                PixelRect::whole(panel_size),
                Waveform::UI,
                refresh,
            )?;
            frames_presented += 1;
            lines.push(format!("present  {current_slug} ({refresh:?})"));

            let exit = loop {
                match events.recv_timeout(Duration::from_millis(200)) {
                    Ok(pointer) => {
                        // Coalesce. A finger on the glass emits samples far
                        // faster than any waveform can land, and presenting
                        // one-per-sample puts the panel permanently behind the
                        // hand. Feed every queued sample to the app — it needs
                        // them all to track a drag — then paint once, from the
                        // state they leave behind.
                        let mut request = current
                            .pointer(pointer)
                            .map_err(|error| DeviceError::unexpected(error.to_string()))?;
                        let mut coalesced = 0_usize;
                        while coalesced < MAX_COALESCED_POINTERS {
                            let Ok(queued) = events.try_recv() else { break };
                            if let Some(later) = current
                                .pointer(queued)
                                .map_err(|error| DeviceError::unexpected(error.to_string()))?
                            {
                                request = Some(later);
                            }
                            coalesced += 1;
                        }

                        // Paint only what the app says changed. No publish
                        // means nothing to show: the cheapest update is the
                        // one that never reaches the panel. Previously this
                        // cloned the whole 14 MB canvas, hashed it, and drove
                        // a full-panel waveform for every single sample.
                        if let Some(damage) = current.take_damage() {
                            let rect = PixelRect::covering(&damage, panel_size);
                            if !rect.is_empty() {
                                current.with_frame(|canvas| {
                                    paper_device::present(
                                        panel.as_mut(),
                                        canvas,
                                        rect,
                                        Waveform::INK,
                                        Refresh::Partial,
                                    )
                                })?;
                                frames_presented += 1;
                            }
                        }
                        match request {
                            Some(Request::Home) => break Exit::Switch("home".to_owned()),
                            Some(Request::Launch(id)) => match session::launch_target(&id) {
                                Some(slug) => break Exit::Switch(slug.to_owned()),
                                None => lines.push(format!(
                                    "ignored: `{id}` is not one of the apps `paperctl run` can launch"
                                )),
                            },
                            Some(Request::ReturnToStock) => break Exit::ReturnToStock,
                            _ => {}
                        }
                    }
                    // The idle tick: nothing arrived, nothing to redraw or
                    // switch. Interactive sessions cost nothing while no one
                    // is touching the glass.
                    Err(RecvTimeoutError::Timeout) => {}
                    // Both readers gave up (or neither node ever resolved) —
                    // no input can reach this session, so holding the
                    // display any longer buys nothing.
                    Err(RecvTimeoutError::Disconnected) => break Exit::ReturnToStock,
                }
            };

            let reason = match &exit {
                Exit::Switch(_) => ExitReason::SwitchedAway,
                Exit::ReturnToStock => ExitReason::ReturnToStock,
            };
            match current.shutdown(reason) {
                Ok(true) => {}
                Ok(false) => {
                    lines.push(format!(
                        "{current_slug}: the app reported that its save failed"
                    ));
                }
                Err(error) => {
                    lines.push(format!(
                        "{current_slug}: could not end the session cleanly: {error}"
                    ));
                }
            }

            current_slug = match exit {
                Exit::Switch(next) => next,
                Exit::ReturnToStock => break,
            };
        }

        panel.clear()?;
        drop(panel);
        lines.push(format!(
            "{frames_presented} frame(s) presented across the session"
        ));

        Ok(PanelRecord {
            real_panel,
            digest: first_digest.expect("the loop above presents at least one frame"),
            // An interactive session presents frame after frame and digests
            // none of the engine's buffers afterwards, so it has no verdict to
            // report — not a negative one. `paperctl open` is the command that
            // reads back, and the one that exits non-zero on the answer.
            holds_sent: None,
            lines,
        })
    }

    /// Resolves the pen and touch nodes and, for each one found, spawns a
    /// thread that decodes it straight onto `sink`.
    ///
    /// A node that does not resolve is logged and skipped rather than
    /// failing the session — a tablet with a working touchscreen and an
    /// unresolved pen node should still run Home and Chess with a finger.
    fn spawn_input_readers(sink: &Sender<PointerEvent>) -> Result<(), DeviceError> {
        let nodes = paper_device::input::enumerate()?;
        let ids = ContactIds::new();

        match paper_device::input::resolve(&nodes, InputRole::Touch).node() {
            Some(node) => {
                let file = paper_device::open_node(&node.path)?;
                let mut decoder = TouchDecoder::new(PointerTransform::touch(), ids.clone());
                let sink = sink.clone();
                std::thread::spawn(move || paper_device::drive(file, |e| decoder.feed(e), &sink));
            }
            None => eprintln!(
                "paperctl run: no touch node resolved; touch input stays silent this session"
            ),
        }

        match paper_device::input::resolve(&nodes, InputRole::Pen).node() {
            Some(node) => {
                let file = paper_device::open_node(&node.path)?;
                let mut decoder = PenDecoder::new(PointerTransform::pen(), ids.clone());
                let sink = sink.clone();
                std::thread::spawn(move || paper_device::drive(file, |e| decoder.feed(e), &sink));
            }
            None => {
                eprintln!("paperctl run: no pen node resolved; pen input stays silent this session")
            }
        }

        Ok(())
    }
}
