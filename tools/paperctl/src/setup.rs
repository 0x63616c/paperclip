//! `paperctl setup` — establishing Paperclip on a tablet, in stages (§14).
//!
//! # What §14 asks for, and what each word costs
//!
//! *Idempotent.* Running it twice changes nothing the second time. Setup is
//! the command someone runs when they are not sure what state the device is
//! in, which is exactly when a command that is only safe once is worst.
//!
//! *Staged.* Each stage is named, reported, and stops the ones after it when
//! it fails. A setup that pressed on past "the display library is not here"
//! would report success and leave a tablet that cannot draw.
//!
//! *Version-aware.* It says what is installed and what this `paperctl` is, and
//! does not pretend they are the same question.
//!
//! *Diagnostic.* And specifically: **it inspects the actual prerequisites
//! rather than treating SSH success as proof of display or sandbox support.**
//! Reaching a shell says the network works. It says nothing about whether
//! `libqsgepaper.so` is there, whether cgroup delegation is on, or whether
//! `xochitl.service` is the unit this build expects.
//!
//! # What it will never do
//!
//! Enable Developer Mode, or downgrade firmware. Neither is implemented, and
//! neither is a flag away — there is no code path here that writes outside
//! `/home/root/paperclip`.
//!
//! # What it does not prove
//!
//! That Paperclip works on this tablet. It proves the prerequisites are
//! present. The §17 acceptance sequence is what proves the rest, and it has
//! not been run.

use std::path::PathBuf;

use clap::Args;
use paper_updater::layout::PlatformLayout;
use paper_updater::upgrade::Maintenance;

use crate::error::CommandError;

/// `paperctl setup`.
#[derive(Debug, Args)]
pub(crate) struct SetupArgs {
    /// The platform root.
    #[arg(long, default_value = "/home/root/paperclip")]
    root: PathBuf,
    /// Report only; create nothing.
    #[arg(long)]
    check: bool,
    /// Which unit counts as stock.
    ///
    /// Overridable for the VM harness, which has no `xochitl.service` and must
    /// not be given one. Not a device flag: on a tablet the default is the
    /// name every generated unit references, and changing it would make those
    /// references point at nothing.
    #[arg(long, default_value = paper_host::units::XOCHITL_UNIT)]
    stock_unit: String,
}

/// How a stage went.
enum Stage {
    /// It was already so, or it was made so.
    Ok(String),
    /// Something is missing, and the stages after this one do not run.
    Missing(String),
    /// Present but worth saying out loud.
    Note(String),
}

impl Stage {
    fn mark(&self) -> &'static str {
        match self {
            Stage::Ok(_) => "ok    ",
            Stage::Missing(_) => "MISSING",
            Stage::Note(_) => "note  ",
        }
    }

    fn text(&self) -> &str {
        match self {
            Stage::Ok(text) | Stage::Missing(text) | Stage::Note(text) => text,
        }
    }

    fn blocks(&self) -> bool {
        matches!(self, Stage::Missing(_))
    }
}

/// Prints one stage line. Returns whether it stops the stages after it.
fn report(name: &str, stage: &Stage) -> bool {
    println!("{name:<22} {}  {}", stage.mark(), stage.text());
    stage.blocks()
}

/// Runs setup.
///
/// # Errors
///
/// A prerequisite that is not there, or a root that cannot be created.
pub(crate) fn run(args: &SetupArgs) -> Result<(), CommandError> {
    let layout = PlatformLayout::new(&args.root);
    let mut blocked = false;

    // Stage 1 — the machine. Nothing is written yet, and nothing after this
    // runs if something here is missing: a setup that pressed on past "the
    // display library is not there" would report success and leave a tablet
    // that cannot draw.
    for (name, stage) in prerequisites(&args.stock_unit) {
        blocked |= report(name, &stage);
    }
    if blocked {
        return Err(CommandError::SetupIncomplete);
    }

    // Stage 2 — the platform root. Idempotent by construction.
    if args.check {
        report(
            "platform root",
            &if layout.is_established() {
                Stage::Ok(format!("{} is established", args.root.display()))
            } else {
                Stage::Note(format!("{} would be created", args.root.display()))
            },
        );
    } else {
        layout.ensure()?;
        report(
            "platform root",
            &Stage::Ok(format!("{} is established", args.root.display())),
        );
    }

    // Stage 3 — reconcile anything an interrupted upgrade left behind, before
    // anyone is told the device is ready. A tablet handed over mid-transaction
    // is a tablet that will surprise someone later.
    if !args.check {
        let reconciled = Maintenance::new(&layout).reconcile()?;
        report("interrupted upgrade", &Stage::Ok(format!("{reconciled:?}")));
    }

    // Stage 4 — versions. Two different questions, asked separately.
    let status = Maintenance::new(&layout).status()?;
    report(
        "installed platform",
        &match &status.current {
            Some(version) => Stage::Ok(format!("{version} selected")),
            None => Stage::Note("nothing selected; run `paperctl upgrade run`".to_owned()),
        },
    );
    report("this paperctl", &Stage::Ok(env!("CARGO_PKG_VERSION").to_owned()));
    report(
        "trusted keys",
        &if layout
            .keys_dir()
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_some())
        {
            Stage::Ok(format!("{}", layout.keys_dir().display()))
        } else {
            Stage::Note(format!(
                "none in {}; copy the public key there before upgrading",
                layout.keys_dir().display()
            ))
        },
    );

    println!(
        "\nPrerequisites are present. That is not the same as Paperclip working here:\n\
         the §17 acceptance sequence is what says that, and it has not been run."
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn prerequisites(_stock_unit: &str) -> Vec<(&'static str, Stage)> {
    vec![(
        "machine",
        Stage::Missing(
            "this is not Linux; setup inspects a tablet, and a Mac cannot stand in for one"
                .to_owned(),
        ),
    )]
}

#[cfg(target_os = "linux")]
fn prerequisites(stock_unit: &str) -> Vec<(&'static str, Stage)> {
    use std::path::Path;
    use std::process::Command;

    let mut stages = Vec::new();

    // The service manager. Everything §10 promises is systemd doing it.
    let systemd = Command::new("systemctl")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success());
    stages.push((
        "systemd",
        match &systemd {
            Some(output) => Stage::Ok(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("present")
                    .to_owned(),
            ),
            None => Stage::Missing("no usable systemctl".to_owned()),
        },
    ));

    // Stock's unit, by the name the generated units reference. A tablet where
    // this is called something else is a tablet where every `Conflicts=` in
    // those units points at nothing.
    let stock = Command::new("systemctl")
        .args(["show", stock_unit, "-p", "LoadState"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    stages.push((
        "stock unit",
        if stock.contains("LoadState=loaded") {
            Stage::Ok(format!("{stock_unit} is loaded"))
        } else {
            Stage::Missing(format!(
                "{stock_unit} is `{stock}`; the generated units reference it by that name"
            ))
        },
    ));

    // Runtime units go here, and `/run` being a tmpfs is what makes a reboot
    // come up stock (WWW-11). A `/run` that persisted would quietly turn that
    // guarantee off.
    let run_is_tmpfs = std::fs::read_to_string("/proc/mounts")
        .unwrap_or_default()
        .lines()
        .any(|line| {
            let mut fields = line.split_whitespace();
            fields.next();
            fields.next() == Some("/run") && fields.next() == Some("tmpfs")
        });
    stages.push((
        "runtime units",
        if run_is_tmpfs {
            Stage::Ok("/run is a tmpfs, so a reboot comes up stock".to_owned())
        } else {
            Stage::Missing("/run is not a tmpfs; the reboot guarantee would not hold".to_owned())
        },
    ));

    // What the sandbox really enforces. Not "does systemd accept the
    // directive" — `MemoryMax=` is accepted on this tablet and enforces
    // nothing.
    stages.push((
        "isolation",
        match paper_host::probe::probe() {
            // What is *enforced*, not what is accepted. `MemoryMax=` is
            // accepted on this tablet and enforces nothing, which is the
            // difference §11 exists to keep visible.
            Ok(facilities) => Stage::Ok(format!(
                "cgroup {:?}; memory {}, pids {}",
                facilities.cgroup,
                enforcement(facilities.memory),
                enforcement(facilities.pids)
            )),
            Err(error) => Stage::Missing(format!("cannot probe: {error}")),
        },
    ));

    // The display library. SSH working says nothing about this, which is the
    // sentence §14 is built around.
    let vendor = ["/usr/lib/libqsgepaper.so", "/usr/lib/plugins/libqsgepaper.so"]
        .into_iter()
        .find(|path| Path::new(path).exists());
    stages.push((
        "display library",
        match vendor {
            Some(path) => Stage::Ok(path.to_owned()),
            None => Stage::Note(
                "no libqsgepaper.so at the paths WWW-1 recorded; presentation will not work"
                    .to_owned(),
            ),
        },
    ));

    // The wakelock. A takeover holds it; an upgrade checks it is released.
    let wake_lock = paper_device::session::WAKE_LOCK;
    stages.push((
        "wakelock",
        if Path::new(wake_lock).exists() {
            Stage::Ok(wake_lock.to_owned())
        } else {
            Stage::Note(format!("no {wake_lock}; the tablet may suspend mid-session"))
        },
    ));

    stages
}

/// Whether a controller's limits are enforced here, in one word.
#[cfg(target_os = "linux")]
fn enforcement(controller: paper_host::facilities::Controller) -> &'static str {
    if controller.enforces() {
        "enforced"
    } else {
        "accepted but ineffective"
    }
}
