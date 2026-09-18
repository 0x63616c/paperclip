//! `paperctl` — the one entry point into Paperclip from a developer's machine (§4).
//!
//! Stage 1 gave it three jobs: open the desktop preview, capture screenshots
//! at target resolution, and validate a `paper.toml`. Stage 4 adds the three
//! that matter on the device: `isolation` says what a platform really
//! enforces, `units` shows what a session would be given, and `stock` is the
//! independent recovery path — the command a person runs over SSH when the
//! screen is wrong, and the command `paperclip-restore-stock.service` runs
//! when nobody is there to.
//!
//! Stage 7 adds the two halves of software distribution, and the split between
//! them is the point. `key`, `package`, `publish` and `check` hold a signing
//! key and are behind the `publishing` feature, so the device build does not
//! contain them. `install`, `list`, `rollback` and `recover` hold only public
//! keys, and do to a store exactly what the tablet does to its own (§12).
//!
//! Stage 9 adds `upgrade` and `remove`. `upgrade` lives here rather than in a
//! binary of its own because §13 asks for an updater independent of the host
//! being replaced, and `paperctl` already is one: it sits outside `releases/`,
//! because it is what the restore unit runs when the supervisor has died. The
//! word "updater" is internal and appears in no command, screen or
//! user-facing document.
//!
//! # Why this is a library
//!
//! `src/main.rs` is a thin shim over [`main()`] (WWW-48). Everything else
//! lives here so it can be exercised by `tools/paperctl/tests/`, by the unit
//! tests beside each module, and by `cargo xtask docs` (which needs [`Cli`]
//! to generate shell completions and a man page from the same derive that
//! already defines the command line, rather than a second, hand-maintained
//! copy of it).

#[cfg(not(target_os = "linux"))]
mod deploy;
#[cfg(feature = "desktop")]
mod dev;
mod device;
#[cfg(not(target_os = "linux"))]
mod doctor;
mod error;
mod install;
#[cfg(not(target_os = "linux"))]
mod logs;
mod manifest;
#[cfg(feature = "apps")]
mod open;
#[cfg(feature = "publishing")]
mod packaging;
#[cfg(feature = "desktop")]
mod preview;
#[cfg(feature = "publishing")]
mod release_manifest;
#[cfg(feature = "apps")]
mod run;
#[cfg(feature = "apps")]
mod screens;
#[cfg(feature = "apps")]
mod session;
mod setup;
mod transport;
mod upgrade;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::error::CommandError;
#[cfg(feature = "apps")]
use crate::screens::Screen;

/// The Paperclip command line.
///
/// `pub` (unlike everything else here) only so `cargo xtask docs` — a
/// separate crate — can reach [`clap::CommandFactory::command`] on it to
/// generate completions and a man page from the derive itself, rather than
/// hand-maintaining a second copy of the command line.
#[derive(Debug, Parser)]
#[command(name = "paperctl", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the desktop preview window.
    #[cfg(feature = "desktop")]
    Preview(preview::PreviewArgs),
    /// Run a real app in a live window, watching for source changes (§14, §15).
    #[cfg(feature = "desktop")]
    Dev(dev::DevArgs),
    /// Render screens to PNG files without opening a window.
    #[cfg(feature = "apps")]
    Screenshot(ScreenshotArgs),
    /// Present a screen on the tablet's panel, hold it, and give the display
    /// back (§4).
    #[cfg(feature = "apps")]
    Open(open::OpenArgs),
    /// Run an interactive Home/Chess session on the tablet's panel, reading
    /// real input, until `ReturnToStock` (§4, WWW-6).
    #[cfg(feature = "apps")]
    Run(run::RunArgs),
    /// Validate a `paper.toml` manifest.
    Manifest {
        #[command(subcommand)]
        command: manifest::ManifestCommand,
    },
    /// Report what the isolation actually enforces (§11).
    Isolation(device::IsolationArgs),
    /// Show the runtime units a session would be given.
    Units(device::UnitsArgs),
    /// Return the display to stock — the independent recovery path (§10).
    Stock(device::StockArgs),
    /// Manage publishing keys.
    #[cfg(feature = "publishing")]
    Key {
        #[command(subcommand)]
        command: packaging::KeyCommand,
    },
    /// Build a `.paperpkg` from a package directory.
    #[cfg(feature = "publishing")]
    Package(packaging::PackageArgs),
    /// Publish a package into a catalog.
    #[cfg(feature = "publishing")]
    Publish(packaging::PublishArgs),
    /// Verify a catalog, or a single package.
    #[cfg(feature = "publishing")]
    Check(packaging::CheckArgs),
    /// Install or update an app from a catalog.
    Install(install::InstallArgs),
    /// Show what is installed, and optionally what a catalog offers.
    List(install::ListArgs),
    /// Select the previous release of an app.
    Rollback(install::RollbackArgs),
    /// Finish or undo whatever an interrupted install left behind.
    Recover(install::RecoverArgs),
    /// Establish Paperclip on a tablet, in stages (§14).
    Setup(setup::SetupArgs),
    /// Replace Paperclip itself (§13).
    Upgrade(upgrade::UpgradeArgs),
    /// Take Paperclip off the device, keeping notebooks and app data (§14).
    Remove(upgrade::RemoveArgs),
    /// List the tablets auto-discovery found, or pin one (WWW-33).
    ///
    /// Config lives at `~/.config/paperctl/config.toml`
    /// (`$PAPERCTL_CONFIG_DIR/config.toml` or
    /// `$XDG_CONFIG_HOME/paperctl/config.toml` override it).
    Devices(transport::devices::DevicesArgs),
    /// The last run's report — what `open` (and `deploy`) actually did.
    #[cfg(not(target_os = "linux"))]
    Logs(logs::LogsArgs),
    /// Can the tablet be reached, and is it healthy (WWW-34).
    #[cfg(not(target_os = "linux"))]
    Doctor(doctor::DoctorArgs),
    /// Cross-compile `paperctl` for the device and install it (WWW-34).
    #[cfg(not(target_os = "linux"))]
    Deploy(deploy::DeployArgs),
}

impl Command {
    /// The name every dispatch of this command is recorded under — in
    /// `paperctl logs`'s `subcommand` column, and in the `paperctl_run` span
    /// `journalctl` would show it nested under (WWW-46). Matches the
    /// subcommand's own name on the command line.
    fn name(&self) -> &'static str {
        match self {
            #[cfg(feature = "desktop")]
            Command::Preview(_) => "preview",
            #[cfg(feature = "desktop")]
            Command::Dev(_) => "dev",
            #[cfg(feature = "apps")]
            Command::Screenshot(_) => "screenshot",
            #[cfg(feature = "apps")]
            Command::Open(_) => "open",
            #[cfg(feature = "apps")]
            Command::Run(_) => "run",
            Command::Manifest { .. } => "manifest",
            Command::Isolation(_) => "isolation",
            Command::Units(_) => "units",
            Command::Stock(_) => "stock",
            #[cfg(feature = "publishing")]
            Command::Key { .. } => "key",
            #[cfg(feature = "publishing")]
            Command::Package(_) => "package",
            #[cfg(feature = "publishing")]
            Command::Publish(_) => "publish",
            #[cfg(feature = "publishing")]
            Command::Check(_) => "check",
            Command::Install(_) => "install",
            Command::List(_) => "list",
            Command::Rollback(_) => "rollback",
            Command::Recover(_) => "recover",
            Command::Setup(_) => "setup",
            Command::Upgrade(_) => "upgrade",
            Command::Remove(_) => "remove",
            Command::Devices(_) => "devices",
            #[cfg(not(target_os = "linux"))]
            Command::Logs(_) => "logs",
            #[cfg(not(target_os = "linux"))]
            Command::Doctor(_) => "doctor",
            #[cfg(not(target_os = "linux"))]
            Command::Deploy(_) => "deploy",
        }
    }
}

/// Which screen to draw.
#[cfg(feature = "apps")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScreenArg {
    /// The home screen.
    Home,
    /// The Chess screen.
    Chess,
    /// The Settings screen.
    Settings,
    /// The App Store screen.
    AppStore,
    /// The Sudoku screen.
    Sudoku,
    /// Every screen, one file each.
    All,
}

#[cfg(feature = "apps")]
impl ScreenArg {
    fn screens(self) -> Vec<Screen> {
        match self {
            ScreenArg::Home => vec![Screen::Home],
            ScreenArg::Chess => vec![Screen::Chess],
            ScreenArg::Settings => vec![Screen::Settings],
            ScreenArg::AppStore => vec![Screen::AppStore],
            ScreenArg::Sudoku => vec![Screen::Sudoku],
            ScreenArg::All => vec![
                Screen::Home,
                Screen::Chess,
                Screen::Settings,
                Screen::AppStore,
                Screen::Sudoku,
            ],
        }
    }
}

/// Render screens to PNG files.
#[cfg(feature = "apps")]
#[derive(Debug, clap::Args)]
struct ScreenshotArgs {
    /// Which screen to capture.
    #[arg(long, value_enum, default_value_t = ScreenArg::All)]
    screen: ScreenArg,
    /// Directory to write PNGs into. Created if it does not exist.
    #[arg(long, default_value = "artifacts")]
    out_dir: std::path::PathBuf,
}

/// Where a Mac build's telemetry goes: human-readable output plus the JSON
/// run-log. The device build (`paperctl stock` running on the tablet itself)
/// has no `logs` command to read that log, so it goes to the journal instead
/// — the same journal a crash is read out of.
#[cfg(not(target_os = "linux"))]
fn init_telemetry() {
    if let Err(error) = paper_telemetry::init_pretty(transport::run_log_dir()) {
        eprintln!("paperctl: telemetry did not start: {error}");
    }
}

#[cfg(target_os = "linux")]
fn init_telemetry() {
    if let Err(error) = paper_telemetry::init_journald() {
        eprintln!("paperctl: telemetry did not start: {error}");
    }
}

/// `paperctl`'s whole run: parse, dispatch under one telemetry span, and turn
/// the result into a process exit code. `src/main.rs` is nothing but a call to
/// this.
pub fn main() -> ExitCode {
    init_telemetry();
    let cli = Cli::parse();
    // One span per dispatch, entered for the whole thing — this is what
    // `paperctl logs` now reads instead of a `runlog::wrap` call at each
    // site (WWW-46): a layer watching for this span cannot be forgotten the
    // way a function call at nine different sites could be, and was.
    let span = paper_telemetry::run_log::run_span(cli.command.name());
    let result = span.in_scope(|| run_cli(cli));
    let exit_code = match &result {
        Ok(code) => i64::from(*code),
        Err(error) => i64::from(error.exit_code()),
    };
    span.record("exit_code", exit_code);
    drop(span);
    to_exit_code(result)
}

fn to_exit_code(result: Result<u8, CommandError>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            report(&error);
            ExitCode::from(error.exit_code())
        }
    }
}

/// Dispatches to the chosen subcommand. `doctor` is the one command that
/// reports a three-way verdict (0/1/2, WWW-34 acceptance criterion 9) as its
/// real exit code instead of the plain 0/1 every other command uses; it needs
/// no special case above this match to get there — [`doctor::run`] already
/// returns the raw code, same as every other arm does once mapped.
fn run_cli(cli: Cli) -> Result<u8, CommandError> {
    match cli.command {
        #[cfg(feature = "desktop")]
        Command::Preview(args) => preview::run(args).map(|()| 0),
        #[cfg(feature = "desktop")]
        Command::Dev(args) => dev::run(&args).map(|()| 0),
        #[cfg(feature = "apps")]
        Command::Screenshot(args) => screenshot(&args).map(|()| 0),
        #[cfg(feature = "apps")]
        Command::Open(args) => open::run(&args).map(|()| 0),
        #[cfg(feature = "apps")]
        Command::Run(args) => run::run(&args).map(|()| 0),
        Command::Manifest { command } => manifest::run(command).map(|()| 0),
        Command::Isolation(args) => device::run(device::DeviceCommand::Isolation(args)).map(|()| 0),
        Command::Units(args) => device::run(device::DeviceCommand::Units(args)).map(|()| 0),
        Command::Stock(args) => device::run(device::DeviceCommand::Stock(args)).map(|()| 0),
        #[cfg(feature = "publishing")]
        Command::Key { command } => packaging::key(command).map(|()| 0),
        #[cfg(feature = "publishing")]
        Command::Package(args) => packaging::package(&args).map(|()| 0),
        #[cfg(feature = "publishing")]
        Command::Publish(args) => packaging::publish(&args).map(|()| 0),
        #[cfg(feature = "publishing")]
        Command::Check(args) => packaging::check(&args).map(|()| 0),
        Command::Install(args) => install::install(&args).map(|()| 0),
        Command::List(args) => install::list(&args).map(|()| 0),
        Command::Rollback(args) => install::rollback(&args).map(|()| 0),
        Command::Recover(args) => install::recover(&args).map(|()| 0),
        Command::Setup(args) => setup::run(&args).map(|()| 0),
        Command::Upgrade(args) => upgrade::run(args).map(|()| 0),
        Command::Remove(args) => upgrade::remove(&args).map(|()| 0),
        Command::Devices(args) => transport::devices::run(&args).map(|()| 0),
        #[cfg(not(target_os = "linux"))]
        Command::Logs(args) => logs::run(&args).map(|()| 0),
        #[cfg(not(target_os = "linux"))]
        Command::Doctor(args) => doctor::run(&args),
        #[cfg(not(target_os = "linux"))]
        Command::Deploy(args) => deploy::run(&args).map(|()| 0),
    }
}

#[cfg(feature = "apps")]
fn screenshot(args: &ScreenshotArgs) -> Result<(), CommandError> {
    let mut screens = screens::Screens::new()?;
    for screen in args.screen.screens() {
        if screen == Screen::Settings {
            // Settings is one `Screen` but six pages plus a confirmation
            // dialog (WWW-22): every one of them gets its own file rather
            // than only the page the app happens to open on.
            for (slug, canvas) in screens.render_settings_pages()? {
                write_canvas(&canvas, &slug, &args.out_dir)?;
            }
            continue;
        }
        let canvas = screens.render_offscreen(screen)?;
        write_canvas(&canvas, screen.slug(), &args.out_dir)?;
    }
    Ok(())
}

/// Writes one canvas to `<out_dir>/<slug>.png` and reports where it went.
#[cfg(feature = "apps")]
fn write_canvas(
    canvas: &paper_sdk::Canvas,
    slug: &str,
    out_dir: &std::path::Path,
) -> Result<(), CommandError> {
    let path = out_dir.join(format!("{slug}.png"));
    canvas
        .write_png(&path)
        .map_err(|source| CommandError::Write {
            path: path.clone(),
            source,
        })?;
    let size = canvas.size();
    println!(
        "{slug} -> {} ({}x{})",
        path.display(),
        size.width,
        size.height
    );
    Ok(())
}

/// Prints an error and everything under it.
///
/// A one-line "invalid manifest" tells a user nothing; the chain is where the
/// actual answer lives (§7: contextual executable errors).
fn report(error: &CommandError) {
    eprintln!("paperctl: {error}");
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
