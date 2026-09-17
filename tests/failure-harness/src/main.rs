//! `paperclip-failure-harness` — causes every §10 failure and checks the
//! machine afterwards.
//!
//! Run it as root in an aarch64 Linux VM with systemd:
//!
//! ```sh
//! cargo build --release -p paper-host -p paperctl -p paper-fault-app \
//!                       -p paper-failure-harness
//! sudo ./target/release/paperclip-failure-harness \
//!     --bin-dir ./target/release --report docs/device/www-4-harness.md
//! ```
//!
//! It refuses to run anywhere else. That refusal is the point: a harness that
//! degraded into a skip on macOS would put a green line in the output that
//! means nothing, and the whole reason this exists is that the project has a
//! standing rule against exactly that.

mod cases;
mod fixture;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use crate::cases::{CASES, Case};
use crate::fixture::Fixture;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.iter().any(|a| a == "--list") {
        for case in CASES {
            println!("{:<26} {}", case.name, case.row);
        }
        return ExitCode::SUCCESS;
    }

    let bin_dir = flag(&arguments, "--bin-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/release"));
    let report_path = flag(&arguments, "--report").map(PathBuf::from);
    let only = flag(&arguments, "--case");

    let selected: Vec<&Case> = match &only {
        None => CASES.iter().collect(),
        Some(name) => CASES.iter().filter(|case| case.name == *name).collect(),
    };
    if selected.is_empty() {
        eprintln!("harness: no case matches `{}`", only.unwrap_or_default());
        return ExitCode::FAILURE;
    }

    let fixture = match Fixture::build(&bin_dir) {
        Ok(fixture) => fixture,
        Err(error) => {
            eprintln!("harness: {error}");
            return ExitCode::FAILURE;
        }
    };

    let machine = describe_machine();
    println!("{machine}\n");

    let mut results = Vec::new();
    for case in selected {
        // Every case starts from the same place, whatever the last one left
        // behind. A case that inherited a broken fixture would report a
        // failure that belongs to its predecessor.
        if let Err(error) = fixture.reset() {
            println!("  {:<26} SETUP FAILED  {error}", case.name);
            results.push((
                case,
                false,
                vec![format!("fixture reset failed: {error}")],
                0.0,
            ));
            continue;
        }
        let started = Instant::now();
        let outcome = (case.run)(&fixture);
        let took = started.elapsed().as_secs_f32();
        match outcome {
            Ok(evidence) => {
                println!("  {:<26} pass   {took:>5.1}s", case.name);
                for line in &evidence {
                    println!("      {line}");
                }
                results.push((case, true, evidence, took));
            }
            Err(error) => {
                println!("  {:<26} FAIL   {took:>5.1}s  {error}", case.name);
                results.push((case, false, vec![error], took));
            }
        }
    }

    let _ = fixture.reset();
    fixture.teardown();

    let passed = results.iter().filter(|(_, ok, _, _)| *ok).count();
    println!("\n{passed}/{} cases passed", results.len());

    if let Some(path) = report_path {
        let report = render(&machine, &results);
        match std::fs::write(&path, report) {
            Ok(()) => println!("report written to {}", path.display()),
            Err(error) => eprintln!("harness: cannot write {}: {error}", path.display()),
        }
    }

    if passed == results.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Records the machine, because a result without one is not evidence.
fn describe_machine() -> String {
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let systemd = std::process::Command::new("systemctl")
        .arg("--version")
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_owned()
        })
        .unwrap_or_default();
    let distribution = std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .find_map(|line| {
            line.strip_prefix("PRETTY_NAME=")
                .map(|v| v.trim_matches('"').to_owned())
        })
        .unwrap_or_default();
    format!(
        "machine   {distribution}\narch      {}\nkernel    {}\nsystemd   {systemd}",
        std::env::consts::ARCH,
        kernel.trim()
    )
}

fn render(machine: &str, results: &[(&Case, bool, Vec<String>, f32)]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# WWW-4 failure harness\n");
    let _ = writeln!(
        out,
        "Generated by `paperclip-failure-harness`. Every row below was produced by causing the\n\
         failure on this machine and then reading unit states, cgroup membership and the pid\n\
         table — not by asserting that a function was called.\n"
    );
    let _ = writeln!(out, "```\n{machine}\n```\n");
    let _ = writeln!(
        out,
        "**This is a Linux VM, not the tablet.** It proves the supervision path is enforced by\n\
         a real systemd on aarch64. It is not device qualification, and the isolation report it\n\
         prints for this machine is not the one that applies to the Paper Pro — see the\n\
         `facilities` case, which asserts the two are different.\n"
    );
    let passed = results.iter().filter(|(_, ok, _, _)| *ok).count();
    let _ = writeln!(out, "{passed}/{} cases passed.\n", results.len());
    let _ = writeln!(out, "| Case | Requirement | Result | Took |");
    let _ = writeln!(out, "|---|---|---|---|");
    for (case, ok, _, took) in results {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {took:.1}s |",
            case.name,
            case.row,
            if *ok { "**pass**" } else { "**FAIL**" }
        );
    }
    let _ = writeln!(out, "\n## What each case observed\n");
    for (case, ok, evidence, _) in results {
        let _ = writeln!(
            out,
            "### `{}` — {}\n",
            case.name,
            if *ok { "pass" } else { "FAIL" }
        );
        let _ = writeln!(out, "{}\n", case.row);
        for line in evidence {
            let _ = writeln!(out, "- {line}");
        }
        let _ = writeln!(out);
    }
    out
}

fn flag(arguments: &[String], name: &str) -> Option<String> {
    let index = arguments.iter().position(|argument| argument == name)?;
    arguments.get(index + 1).cloned()
}
