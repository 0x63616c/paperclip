//! A foreground session that misbehaves to order.
//!
//! The §10 recovery table is a list of things that must happen *when something
//! goes wrong*. Writing a test for that needs something that goes wrong on
//! purpose, reliably, in a named way — so each mode here is one row of that
//! table, or one of the extra hazards WWW-4 adds on top of it.
//!
//! Every mode announces readiness through `sd_notify` first, because a failure
//! before readiness is a different row from a failure after it, and the
//! supervisor must tell them apart.
//!
//! Usage: `paper-fault-app <mode> [--progress PATH] [--lock PATH] [args...]`

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use paper_host::progress::MainLoopProgress;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(mode) = arguments.first().cloned() else {
        eprintln!("paper-fault-app: expected a mode");
        return ExitCode::FAILURE;
    };

    let progress_path = flag(&arguments, "--progress").map(PathBuf::from);
    let lock_path = flag(&arguments, "--lock").map(PathBuf::from);
    let target = flag(&arguments, "--target");
    let ticks: u64 = flag(&arguments, "--ticks")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(u64::MAX);
    // How many main-loop passes to make before misbehaving.
    //
    // Not padding. A session that fails before the supervisor has observed it
    // arrive is testing the *switch* deadline, which is a different row of
    // §10 from the one these modes are for — the failure has to happen to an
    // established session or the case proves the wrong thing.
    let settle: u64 = flag(&arguments, "--settle")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(24);

    let mut progress = progress_path.map(MainLoopProgress::new);

    // Claim the display the way a real session would: write our pid into the
    // ownership registry the supervisor reads. A session that starts and never
    // does this must not count as having arrived.
    if let Some(lock) = &lock_path
        && let Err(error) = fs::write(lock, format!("{}\npaper-fault-app\n", std::process::id()))
    {
        // Loud, and fatal. A session that cannot register itself as the
        // display owner has not taken the display, and the supervisor is
        // right to let the switch time out — but silence here turns an
        // ownership problem into a mysterious deadline expiry.
        eprintln!(
            "paper-fault-app: cannot claim the display registry at {}: {error}",
            lock.display()
        );
        return ExitCode::FAILURE;
    }

    ready();

    match mode.as_str() {
        // `run` is what `paperclip-host.service` invokes. A release whose
        // `bin/paperclip-host` is this binary is a platform release that can
        // be made to come up wrongly on purpose (§13) — the only way to test
        // "the candidate stalled at `device-adapter`" without breaking a
        // display.
        "run" => fake_host(&arguments),
        "healthy" => loop_forever(&mut progress, ticks),
        "hang" => {
            // Tick a few times so the supervisor sees a session that genuinely
            // arrived, then stop advancing the loop while staying alive. This
            // is the row a liveness check cannot catch.
            spin(&mut progress, 3);
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        "heartbeat-only" => {
            // The trap §10 names: a thread that keeps proving the process
            // exists while the loop that draws has stopped. If this is
            // indistinguishable from `healthy`, the supervisor is measuring
            // the wrong thing.
            spin(&mut progress, 3);
            std::thread::spawn(|| {
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        "panic" => {
            spin(&mut progress, settle);
            panic!("paper-fault-app: deliberate panic");
        }
        "abort" => {
            spin(&mut progress, settle);
            std::process::abort();
        }
        "exit" => {
            spin(&mut progress, settle);
            let code: u8 = target.and_then(|raw| raw.parse().ok()).unwrap_or(3);
            ExitCode::from(code)
        }
        "ignore-term" => {
            ignore_termination();
            spin(&mut progress, settle);
            loop {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        "leave-children" => {
            // A child that outlives its parent and reparents to pid 1. Only a
            // cgroup sweep finds this; a parent-pid walk cannot, and that
            // difference is the point of the row.
            for _ in 0..3 {
                let _ = std::process::Command::new("sleep").arg("3600").spawn();
            }
            spin(&mut progress, settle);
            ExitCode::SUCCESS
        }
        "memory-pressure" => {
            spin(&mut progress, settle);
            let mut held: Vec<Vec<u8>> = Vec::new();
            loop {
                // 8 MiB at a time, touched so it is really resident.
                let mut block = vec![0_u8; 8 * 1024 * 1024];
                for byte in block.iter_mut().step_by(4096) {
                    *byte = 1;
                }
                held.push(block);
                if let Some(progress) = progress.as_mut() {
                    let _ = progress.tick();
                }
            }
        }
        "disk-fill" => {
            spin(&mut progress, settle);
            let Some(path) = target.map(PathBuf::from) else {
                eprintln!("paper-fault-app: disk-fill needs --target PATH");
                return ExitCode::FAILURE;
            };
            match fill(&path) {
                Ok(written) => {
                    println!("disk-fill: stopped after {written} bytes");
                    ExitCode::from(9)
                }
                Err(error) => {
                    eprintln!("disk-fill: {error}");
                    ExitCode::from(9)
                }
            }
        }
        "write-outside" => {
            spin(&mut progress, settle);
            let Some(path) = target.map(PathBuf::from) else {
                eprintln!("paper-fault-app: write-outside needs --target PATH");
                return ExitCode::FAILURE;
            };
            match fs::write(&path, b"paperclip should not be able to write this") {
                Ok(()) => {
                    // The interesting failure. A *successful* write here means
                    // the sandbox did not hold, so the harness needs it to be
                    // loud and distinguishable from every other exit code.
                    eprintln!("write-outside: WROTE {} — not contained", path.display());
                    ExitCode::from(42)
                }
                Err(error) => {
                    println!("write-outside: refused ({}): {error}", path.display());
                    ExitCode::SUCCESS
                }
            }
        }
        other => {
            eprintln!("paper-fault-app: unknown mode `{other}`");
            ExitCode::FAILURE
        }
    }
}

/// Stands in for `paperclip-host`, climbing the readiness ladder as far as a
/// script beside the executable says to.
///
/// The script is `<release>/ladder`, read from the directory *above* the one
/// holding this binary — so it travels inside the release directory and each
/// staged release behaves the way its own bundle said it would. Reading it
/// from a fixed path instead would mean the candidate and the fallback could
/// not misbehave differently, which is exactly the case a rollback test needs.
///
/// One line:
///
/// ```text
/// ready              climb to `ready` and stay up
/// stall=<rung>       climb to `<rung>` and stay up, never reaching `ready`
/// panic              climb to `control`, then panic
/// ```
fn fake_host(arguments: &[String]) -> ExitCode {
    let Some(state) = flag(arguments, "--state").map(PathBuf::from) else {
        eprintln!("paper-fault-app run: expected --state");
        return ExitCode::FAILURE;
    };
    let script = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()
                .and_then(|bin| bin.parent())
                .map(Path::to_path_buf)
        })
        .map(|release| release.join("ladder"))
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_else(|| "ready".to_owned());
    let script = script.trim().to_owned();

    let _ = fs::create_dir_all(&state);
    let status = state.join("status");
    let write = |rung: &str, note: &str| {
        let _ = fs::write(
            &status,
            format!(
                "state=stock\nforeground=stock\nmay_relaunch=true\ndiagnosis=\n\
                 ready={rung}\nready_note={note}\nprotocol=1.0\n"
            ),
        );
    };

    let stop_at = script
        .strip_prefix("stall=")
        .unwrap_or(match script.as_str() {
            "panic" => "control",
            _ => "ready",
        });

    // Climb, publishing each rung, exactly as the real supervisor does.
    let ladder = [
        "process",
        "control",
        "protocol",
        "device-adapter",
        "home",
        "ready",
    ];
    let limit = ladder.iter().position(|rung| *rung == stop_at).unwrap_or(0);
    for rung in &ladder[..=limit] {
        write(rung, "");
        std::thread::sleep(Duration::from_millis(50));
    }
    if limit + 1 < ladder.len() {
        write(
            ladder[limit],
            &format!("scripted stall below `{}`", ladder[limit + 1]),
        );
    }

    // `READY=1` whatever the ladder said. §13's point is precisely that these
    // are different claims: the unit is active and can be asked, and the
    // status file says whether it is healthy.
    ready();

    if script == "panic" {
        panic!("paper-fault-app: deliberate panic during startup");
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn loop_forever(progress: &mut Option<MainLoopProgress>, ticks: u64) -> ExitCode {
    let mut done = 0;
    while done < ticks {
        if let Some(progress) = progress.as_mut()
            && progress.tick().is_err()
        {
            return ExitCode::FAILURE;
        }
        done += 1;
        std::thread::sleep(Duration::from_millis(200));
    }
    ExitCode::SUCCESS
}

fn spin(progress: &mut Option<MainLoopProgress>, times: u64) {
    for _ in 0..times {
        if let Some(progress) = progress.as_mut() {
            let _ = progress.tick();
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

fn fill(path: &PathBuf) -> std::io::Result<u64> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(path)?;
    let block = vec![0_u8; 1024 * 1024];
    let mut written = 0;
    loop {
        match file.write_all(&block) {
            Ok(()) => written += block.len() as u64,
            Err(error) => {
                let _ = file.flush();
                return if written == 0 {
                    Err(error)
                } else {
                    Ok(written)
                };
            }
        }
    }
}

fn flag(arguments: &[String], name: &str) -> Option<String> {
    let index = arguments.iter().position(|argument| argument == name)?;
    arguments.get(index + 1).cloned()
}

/// Announces readiness the way a real session does.
fn ready() {
    #[cfg(target_os = "linux")]
    {
        paper_host::linux::systemd::notify_ready();
    }
}

/// Refuses `SIGTERM`, so the supervisor has to escalate.
#[cfg(target_os = "linux")]
fn ignore_termination() {
    // SAFETY: `signal` with `SIG_IGN` sets a disposition and runs no handler,
    // so there is no async-signal-safety question here. SIGTERM is a valid,
    // catchable signal. This is the whole purpose of this binary.
    #[allow(unsafe_code)]
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
}

#[cfg(not(target_os = "linux"))]
fn ignore_termination() {}
