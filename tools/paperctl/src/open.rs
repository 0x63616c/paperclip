//! `paperctl open` — put a Paperclip screen on the tablet's panel.
//!
//! §4 makes `paperctl` the v1 entry point into Paperclip, and this is its first
//! behaviour that reaches glass. One command, run over SSH on the tablet:
//!
//! ```text
//! paperctl open                 # the Home shelf, held a minute, then stock back
//! paperctl open --hold 300      # long enough to photograph properly
//! paperctl open --dry-run       # the render and its digest, no takeover
//! ```
//!
//! The command's job is small and deliberately so: render the screen WWW-2
//! already draws, hand the canvas to [`paper_device`], print what came back.
//! No drawing code lives here, no device sequencing lives here. Everything
//! about stopping Xochitl, the wakelock, the advisory locks and the restore is
//! `paper_device::hold`'s, which is where the ordering that makes it safe is
//! written down and tested (§8: nothing above the device adapter knows what a
//! systemd unit is).
//!
//! ## Two refusals worth reading
//!
//! A build without the vendor engine cannot reach the panel, so `open` refuses
//! rather than stopping Xochitl to present into memory. And a frame that
//! rasterised to nothing at all refuses *before* the takeover — presenting a
//! blank panel and presenting a perfect one produce identical logs on this
//! side of the glass, and that is precisely the mistake §17 exists to stop.

use std::time::Duration;

use paper_device::{DisplayProbe, FrameDigest, HoldPlan, MemoryPanel, Refresh, Waveform};
use paper_sdk::{Canvas, SCREEN};

use crate::ScreenArg;
use crate::error::CommandError;
use crate::screens::{Screen, Screens};

/// `paperctl open`.
#[derive(Debug, clap::Args)]
pub(crate) struct OpenArgs {
    /// Which screen to present.
    #[arg(long, value_enum, default_value = "home")]
    screen: ScreenArg,
    /// How long to leave it on the panel, in seconds.
    #[arg(long, default_value_t = 60)]
    hold: u64,
    /// The waveform the present uses.
    #[arg(long, value_enum, default_value_t = WaveformArg::MonoQuality)]
    waveform: WaveformArg,
    /// Present with `full=0`.
    ///
    /// The default is a full flash because this is the *first* paint over
    /// whatever stock had on the panel, and a partial update leaves that
    /// ghosting underneath. Pass this when the panel already shows a Paperclip
    /// screen and only part of it is changing.
    #[arg(long)]
    partial: bool,
    /// How often to read the connector and rails during the hold, in seconds.
    #[arg(long, default_value_t = 5)]
    sample_every: u64,
    /// Render and digest the frame without touching the display.
    ///
    /// The one form of this command that is useful on a Mac: it produces the
    /// same digest the device run reports, so the two can be compared.
    #[arg(long)]
    dry_run: bool,
    /// Internal: be the presenting half, and nothing else.
    ///
    /// Opens the vendor engine, presents, holds, clears, writes its report to
    /// `--report-to` and exits. It never stops or starts Xochitl — it cannot,
    /// see `paper_device::hold`. Hidden because running it by hand takes the
    /// panel out from under a live Xochitl.
    #[arg(long, hide = true)]
    present_only: bool,
    /// Internal: where the presenting half leaves its report.
    #[arg(long, hide = true, value_name = "PATH")]
    report_to: Option<std::path::PathBuf>,
    #[command(flatten)]
    device: crate::transport::DeviceArgs,
}

/// Which waveform to ask the engine for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum WaveformArg {
    /// Mono mode 0 — the fast table, for live ink.
    MonoInk,
    /// Mono mode 3 — the quality table. What a static screen wants.
    MonoQuality,
    /// Colour mode 4.
    Colour,
}

impl WaveformArg {
    /// The spelling `--waveform` accepts, so the presenting half — or the
    /// remote `paperctl` a Mac forwards to — can be given back exactly what
    /// this half was given.
    fn slug(self) -> &'static str {
        match self {
            WaveformArg::MonoInk => "mono-ink",
            WaveformArg::MonoQuality => "mono-quality",
            WaveformArg::Colour => "colour",
        }
    }

    fn waveform(self) -> Waveform {
        match self {
            WaveformArg::MonoInk => Waveform::INK,
            WaveformArg::MonoQuality => Waveform::MONO_QUALITY,
            WaveformArg::Colour => Waveform::COLOR,
        }
    }
}

impl OpenArgs {
    /// The spelling `--screen` accepts for whatever was chosen.
    fn screen_slug(&self) -> &'static str {
        match self.screen {
            ScreenArg::Home => "home",
            ScreenArg::Chess => "chess",
            ScreenArg::Settings => "settings",
            ScreenArg::AppStore => "app-store",
            ScreenArg::All => "home",
        }
    }

    /// The argv a remote `paperctl open` on the tablet should be given —
    /// everything this half was, except `--dry-run`, `--present-only` and
    /// `--report-to`, which only mean something to the half that opens the
    /// vendor engine.
    #[cfg(not(target_os = "linux"))]
    fn remote_argv(&self) -> Vec<String> {
        let mut argv = vec![
            "open".to_owned(),
            "--screen".to_owned(),
            self.screen_slug().to_owned(),
            "--hold".to_owned(),
            self.hold.to_string(),
            "--waveform".to_owned(),
            self.waveform.slug().to_owned(),
            "--sample-every".to_owned(),
            self.sample_every.to_string(),
        ];
        if self.partial {
            argv.push("--partial".to_owned());
        }
        argv
    }
}

/// Renders the screen and presents it, or explains why it will not.
pub(crate) fn run(args: &OpenArgs) -> Result<(), CommandError> {
    let screen = args
        .screen
        .screens()
        .first()
        .copied()
        .unwrap_or(Screen::Home);
    let canvas = render(screen)?;
    let plan = plan_from(args);

    // Said before the display is touched, so a log that ends abruptly still
    // records what was about to be put on the panel and where it came from.
    println!("screen   {} ({})", screen.slug(), provenance(screen));
    println!(
        "frame    {}",
        FrameDigest::of(&canvas).map_err(CommandError::Device)?
    );
    println!(
        "plan     waveform mode {} {:?}, full={}, hold {}s, sampled every {}s",
        plan.waveform.mode(),
        plan.waveform.content(),
        plan.refresh.full_flag(),
        plan.hold.as_secs(),
        plan.sample_every.as_secs()
    );

    if args.dry_run {
        return dry_run(&canvas, &plan);
    }
    if args.present_only {
        return present_only(&canvas, &plan, args.report_to.as_deref());
    }
    present(&canvas, &plan, args)
}

/// Draws the screen at panel resolution, through WWW-2's renderer.
///
/// There is no second drawing path and no placeholder: `Screens` is the same
/// code `paperctl screenshot` and `paperctl preview` use, so what the tablet
/// shows is what the PNG shows, pixel for pixel. If this fails, the command
/// fails — substituting a test pattern would make the one thing this run is
/// trying to establish unfalsifiable.
fn render(screen: Screen) -> Result<Canvas, CommandError> {
    let mut screens = Screens::new()?;
    screens.render_offscreen(screen)
}

fn provenance(screen: Screen) -> &'static str {
    match screen {
        Screen::Home => "real render of the Home shelf, paper_home::render — not a test pattern",
        Screen::Chess => "real render of the Chess board, paper_chess::render",
        Screen::Settings => "real render of Settings, paper_settings::render",
        Screen::AppStore => "real render of the App Store, paper_app_store::render",
    }
}

fn plan_from(args: &OpenArgs) -> HoldPlan {
    HoldPlan {
        hold: Duration::from_secs(args.hold),
        waveform: args.waveform.waveform(),
        refresh: if args.partial {
            Refresh::Partial
        } else {
            Refresh::Full
        },
        sample_every: Duration::from_secs(args.sample_every.max(1)),
    }
}

/// Runs this same binary as the presenting half and waits for it to finish.
///
/// `wait` is load-bearing: it is what turns "the presenter is done" into "the
/// process holding the vendor lock no longer exists", which is what makes the
/// Xochitl start that follows able to succeed. stdout and stderr are inherited
/// so the vendor engine's own diagnostics — panel lot, waveform table, pmic
/// rails — reach the operator live rather than being parsed out of a pipe.
///
/// `/tmp` for the report, never `/home` or the root filesystem: tmpfs, cleared
/// at reboot, and the same place the start record already lives.
#[cfg(target_os = "linux")]
fn spawn_presenter(
    args: &OpenArgs,
) -> Result<paper_device::PanelRecord, paper_device::DeviceError> {
    use std::process::Command;

    let binary = std::env::current_exe().map_err(|source| {
        paper_device::DeviceError::io("find this binary for", "paperctl", source)
    })?;
    let report =
        std::path::PathBuf::from(format!("/tmp/paperclip-open-{}.report", std::process::id()));
    let _ = std::fs::remove_file(&report);

    let mut command = Command::new(&binary);
    command
        .arg("open")
        .arg("--present-only")
        .arg("--report-to")
        .arg(&report)
        .arg("--screen")
        .arg(args.screen_slug())
        .arg("--hold")
        .arg(args.hold.to_string())
        .arg("--waveform")
        .arg(args.waveform.slug())
        .arg("--sample-every")
        .arg(args.sample_every.to_string());
    if args.partial {
        command.arg("--partial");
    }

    let status = command.status().map_err(|source| {
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

/// Everything except the glass: the render, the digest, the swap it would ask
/// for. The hold is skipped — there is nothing to look at.
fn dry_run(canvas: &Canvas, plan: &HoldPlan) -> Result<(), CommandError> {
    let mut panel = MemoryPanel::new(SCREEN);
    let plan = HoldPlan {
        hold: Duration::ZERO,
        ..plan.clone()
    };
    let work = paper_device::present_and_hold(
        &mut panel,
        canvas,
        &plan,
        &DisplayProbe::device(),
        false,
        |_| {},
    )
    .map_err(CommandError::Device)?;
    println!("{work}");
    println!("dry run: the display was not touched and stock was not stopped");
    Ok(())
}

/// On a Mac, presenting means forwarding to the tablet's own `paperctl open`
/// over SSH (WWW-33) — detached, because a live SSH session does not
/// reliably survive a long hold (WWW-23).
#[cfg(not(target_os = "linux"))]
fn present(_canvas: &Canvas, plan: &HoldPlan, args: &OpenArgs) -> Result<(), CommandError> {
    let (host, source) = crate::transport::remote::resolve_device(args.device.as_deref())?;
    println!("device   {host} ({source})");
    crate::transport::remote::run_open(&host, &args.remote_argv(), plan.hold)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn present_only(
    _canvas: &Canvas,
    _plan: &HoldPlan,
    _report_to: Option<&std::path::Path>,
) -> Result<(), CommandError> {
    Err(CommandError::NotOnDevice {
        what: "presenting a screen on the panel (try --dry-run)",
    })
}

/// The presenting half: open the engine, show the frame, write down what
/// happened, and — most importantly — **exit**.
///
/// Its exit is the whole safety property. `libqsgepaper`'s `EPFramebuffer` is
/// a singleton whose lock `checkLockFile()` takes and nothing gives back, so
/// while this process lives no Xochitl can start. WWW-23 found that out by
/// rebooting the tablet.
#[cfg(target_os = "linux")]
fn present_only(
    canvas: &Canvas,
    plan: &HoldPlan,
    report_to: Option<&std::path::Path>,
) -> Result<(), CommandError> {
    if !paper_device::is_real_device() {
        return Err(CommandError::NoVendorEngine);
    }
    let work = paper_device::present_only(canvas, plan).map_err(CommandError::Device)?;
    println!("{work}");
    if let Some(path) = report_to {
        paper_device::PanelRecord::of(&work)
            .write(path)
            .map_err(CommandError::Device)?;
    }
    Ok(())
}

/// The surviving half: preflight, take the display, run the presenter as a
/// child, put stock back and check it.
///
/// This process never opens the vendor engine. That is not tidiness — it is
/// the reason it is able to start Xochitl at all.
#[cfg(target_os = "linux")]
fn present(canvas: &Canvas, plan: &HoldPlan, args: &OpenArgs) -> Result<(), CommandError> {
    // A device build without `vendor-engine` opens a `MemoryPanel`. Stopping
    // Xochitl to draw into one would be all of the risk and none of the point,
    // so it is refused here rather than discovered in the report afterwards.
    if !paper_device::is_real_device() {
        return Err(CommandError::NoVendorEngine);
    }
    let report = paper_device::open_and_hold(canvas, plan, || spawn_presenter(args))
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

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    fn args(hold: u64, partial: bool) -> OpenArgs {
        OpenArgs {
            screen: ScreenArg::Chess,
            hold,
            waveform: WaveformArg::MonoInk,
            partial,
            sample_every: 5,
            dry_run: false,
            present_only: false,
            report_to: None,
            device: crate::transport::DeviceArgs::default(),
        }
    }

    #[test]
    fn remote_argv_carries_the_screen_hold_waveform_and_sampling() {
        let argv = args(45, false).remote_argv();
        assert_eq!(
            argv,
            vec![
                "open",
                "--screen",
                "chess",
                "--hold",
                "45",
                "--waveform",
                "mono-ink",
                "--sample-every",
                "5",
            ]
        );
    }

    #[test]
    fn remote_argv_adds_partial_only_when_asked() {
        assert!(
            args(45, true)
                .remote_argv()
                .contains(&"--partial".to_owned())
        );
        assert!(
            !args(45, false)
                .remote_argv()
                .contains(&"--partial".to_owned())
        );
    }
}
