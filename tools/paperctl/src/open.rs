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
    fn waveform(self) -> Waveform {
        match self {
            WaveformArg::MonoInk => Waveform::INK,
            WaveformArg::MonoQuality => Waveform::MONO_QUALITY,
            WaveformArg::Colour => Waveform::COLOR,
        }
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
    present(&canvas, &plan)
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

#[cfg(not(target_os = "linux"))]
fn present(_canvas: &Canvas, _plan: &HoldPlan) -> Result<(), CommandError> {
    Err(CommandError::NotOnDevice {
        what: "presenting a screen on the panel (try --dry-run)",
    })
}

#[cfg(target_os = "linux")]
fn present(canvas: &Canvas, plan: &HoldPlan) -> Result<(), CommandError> {
    // A device build without `vendor-engine` opens a `MemoryPanel`. Stopping
    // Xochitl to draw into one would be all of the risk and none of the point,
    // so it is refused here rather than discovered in the report afterwards.
    if !paper_device::is_real_device() {
        return Err(CommandError::NoVendorEngine);
    }
    let report = paper_device::open_and_hold(canvas, plan).map_err(CommandError::Device)?;
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
