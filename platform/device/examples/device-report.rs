//! A read-only readiness report, to be run on the tablet.
//!
//! Answers, in one run and without touching anything, the questions the next
//! device session would otherwise spend its first ten minutes on:
//!
//! - which `/dev/input` node is the pen and which is the touchscreen, resolved
//!   by advertised capability rather than by node number;
//! - whether this process can take a wakelock without root;
//! - who the vendor's advisory display locks say holds the panel.
//!
//! **It writes nothing and changes nothing.** The wakelock check is a
//! permission test on the node, not an attempt to take it: taking one here
//! would change the tablet's suspend behaviour for as long as the process ran.
//!
//! Build and run:
//!
//! ```text
//! cargo build --release --example device-report --target aarch64-unknown-linux-gnu
//! scp target/aarch64-unknown-linux-gnu/release/examples/device-report remarkable-wifi:/home/root/paperclip/
//! ssh remarkable-wifi /home/root/paperclip/device-report
//! ```
//!
//! Redact the machine id and boot id before putting the output on an issue.

use std::path::Path;

use paper_device::input::{InputRole, nodes};
use paper_device::session::{self, DisplayLocks};
use paper_device::{PointerTransform, is_real_device};

fn main() {
    println!("paperclip device report (read-only)");
    println!(
        "  vendor engine linked into this build: {}",
        is_real_device()
    );

    println!("\ninput nodes");
    match nodes::enumerate() {
        Ok(found) if found.is_empty() => println!("  none — is this the tablet?"),
        Ok(found) => {
            for node in &found {
                println!(
                    "  {:<22} {:<28} role={}",
                    node.path.display(),
                    node.name,
                    node.role
                );
            }
            report_resolution(&found, InputRole::Pen, PointerTransform::pen());
            report_resolution(&found, InputRole::Touch, PointerTransform::touch());
        }
        Err(error) => println!("  could not enumerate: {error}"),
    }

    println!("\nwakelock");
    for path in [session::WAKE_LOCK, session::WAKE_UNLOCK] {
        let node = Path::new(path);
        if !node.exists() {
            println!("  {path}: absent");
            continue;
        }
        // Opening for append proves writability without writing a byte.
        match std::fs::OpenOptions::new().append(true).open(node) {
            Ok(_) => println!("  {path}: writable by this process"),
            Err(error) => println!("  {path}: NOT writable ({error}) — check group xochitl"),
        }
    }

    println!("\nvendor display locks");
    match DisplayLocks::inspect() {
        Ok(locks) => {
            println!(
                "  {}: {}",
                session::EPD_LOCK,
                if locks.epd_lock_present {
                    "present"
                } else {
                    "absent"
                }
            );
            match &locks.holder {
                Some(holder) => println!(
                    "  {}: held by {}",
                    session::EPFRAMEBUFFER_LOCK,
                    holder.describe()
                ),
                None => println!("  {}: absent", session::EPFRAMEBUFFER_LOCK),
            }
            match session::current_boot_id() {
                Ok(boot) => match locks.ensure_free(&boot) {
                    Ok(()) => println!("  the panel is free to take"),
                    Err(error) => println!("  {error} — stop xochitl cleanly first, never kill it"),
                },
                Err(error) => println!("  could not read this boot's id: {error}"),
            }
        }
        Err(error) => println!("  could not inspect: {error}"),
    }

    println!("\nnothing was written. Stock Xochitl is untouched.");
}

fn report_resolution(found: &[nodes::InputNode], role: InputRole, transform: PointerTransform) {
    let resolution = nodes::resolve(found, role);
    let verdict = match &resolution {
        nodes::Resolution::Confirmed(node) => {
            format!("{} (name and capabilities agree)", node.path.display())
        }
        nodes::Resolution::NameChanged { node, expected } => format!(
            "{} — RENAMED: expected {expected:?}, found {:?}. Usable, but record it.",
            node.path.display(),
            node.name
        ),
        nodes::Resolution::Contradictory { node } => format!(
            "REFUSED: {} carries the expected name but classifies as {}. Do not guess.",
            node.path.display(),
            node.role
        ),
        nodes::Resolution::Unresolved => "ambiguous or missing; do not guess".to_owned(),
    };
    println!("  resolved {role} -> {verdict}");

    if let Some(node) = resolution.node()
        && !nodes::is_observed_node(&node.path)
    {
        println!("    note: node order differs from what WWW-20 saw — resolution handled it");
    }
    println!(
        "    transform assumes a {}x{} digitizer",
        transform.extent().width,
        transform.extent().height
    );
}
