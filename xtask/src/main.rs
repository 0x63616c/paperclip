//! `cargo xtask` — the `just` recipes with real logic behind them (WWW-45):
//! the device bundle, ADR scaffolding, and the release planner. Everything
//! simple enough to be a one-line `cargo` invocation lives in the `justfile`
//! instead; this crate exists for the recipes that are not.

mod adr;
mod device_bundle;
mod install_hooks;
mod plan_release;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, thiserror::Error)]
enum XtaskError {
    #[error(transparent)]
    DeviceBundle(#[from] device_bundle::DeviceBundleError),
    #[error(transparent)]
    Adr(#[from] adr::AdrError),
    #[error(transparent)]
    PlanRelease(#[from] plan_release::PlanError),
    #[error(transparent)]
    InstallHooks(#[from] install_hooks::InstallHooksError),
}

#[derive(Debug, Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Cross-builds `paperctl` for the tablet and stages it with its digest.
    DeviceBundle,
    /// Scaffolds a new ADR and adds its row to `docs/adr/README.md`.
    NewAdr {
        /// The ADR's title, e.g. "A fourth request: `Launch`".
        title: String,
    },
    /// "What needs publishing?" — declared app and platform versions against
    /// what GitHub already lists (WWW-61).
    PlanRelease {
        /// `owner/name` on GitHub.
        #[arg(long, default_value = "0x63616c/paperclip")]
        repo: String,
        /// `text` for a human table, `json` for a workflow step to parse.
        #[arg(long, value_enum, default_value_t = PlanFormat::Text)]
        format: PlanFormat,
    },
    /// Points this checkout's git hooks at `.githooks/` (WWW-66). Runs
    /// automatically on every workspace build (`xtask/build.rs`); this is
    /// for a checkout that never triggers that, or to confirm it took.
    InstallHooks,
}

/// How `plan-release` prints its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PlanFormat {
    /// A table for a person at a terminal.
    Text,
    /// The plan, as JSON, for a workflow step to parse.
    Json,
}

/// The workspace root: two directories up from this crate (`xtask/`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory")
        .to_path_buf()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = repo_root();

    match cli.command {
        Command::DeviceBundle => exit_code(
            device_bundle::run(&root)
                .map(|bundle| {
                    println!("staged: {}", bundle.binary.display());
                    println!("sha256: {}", bundle.digest_hex);
                })
                .map_err(XtaskError::from),
        ),
        Command::NewAdr { title } => exit_code(
            adr::run(&root, &title)
                .map(|stem| println!("scaffolded: {}/{stem}.md", adr::ADR_DIR))
                .map_err(XtaskError::from),
        ),
        // A published conflict is not a tool failure to print and discard —
        // it is the finding — so this reports the plan and only then decides
        // the exit code, rather than fitting into `exit_code`'s "printed
        // already, only the error case is left to report" shape.
        Command::PlanRelease { repo, format } => {
            match plan_release::plan(&root, &repo, &plan_release::GithubReleaseSource) {
                Ok(plan) => {
                    print_plan(&plan, format);
                    if plan.has_conflicts() {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    }
                }
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::InstallHooks => exit_code(
            install_hooks::run(&root)
                .map(|outcome| {
                    println!(
                        "{}",
                        match outcome {
                            install_hooks::HooksOutcome::AlreadyInstalled =>
                                "core.hooksPath already points at .githooks",
                            install_hooks::HooksOutcome::Installed =>
                                "core.hooksPath now points at .githooks",
                            install_hooks::HooksOutcome::Skipped =>
                                "not a git checkout, or .githooks/pre-commit is missing: skipped",
                        }
                    )
                })
                .map_err(XtaskError::from),
        ),
    }
}

/// `Ok` printed its own result already; this only reports an `Err`.
fn exit_code(result: Result<(), XtaskError>) -> ExitCode {
    if let Err(error) = result {
        eprintln!("xtask: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Prints a plan as a human table or as JSON, per `format`.
fn print_plan(plan: &plan_release::ReleasePlan, format: PlanFormat) {
    match format {
        PlanFormat::Json => {
            // `ReleasePlan` and its fields all derive `Serialize`; this can
            // only fail on a writer error, which stdout does not produce.
            println!(
                "{}",
                serde_json::to_string_pretty(plan).expect("plan serialises")
            );
        }
        PlanFormat::Text => {
            for entry in std::iter::once(&plan.platform).chain(plan.apps.iter()) {
                println!(
                    "{:<9} {:<24} {:<10} {}",
                    format!("{:?}", entry.action).to_lowercase(),
                    entry.name,
                    entry.version,
                    entry.tag
                );
            }
            if plan.has_conflicts() {
                println!(
                    "\nconflict: a declared version is already published under different content"
                );
            } else if !plan.needs_publishing() {
                println!("\nnothing to publish");
            }
        }
    }
}
