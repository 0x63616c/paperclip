//! The Stage 3 round trip: stock out, Paperclip on the panel, stock back.
//!
//! Run **on the tablet**. It stops Xochitl, presents through the vendor
//! waveform engine, times every swap, reads input, clears the panel and gives
//! the display back — then re-reads stock's health and compares it with what it
//! found. Every restore path is `paper_device::Takeover`'s, so a panic or a
//! signal here still hands the display back.
//!
//! ```text
//! cargo build --release -p paper-device --example takeover \
//!     --features vendor-engine --target aarch64-unknown-linux-gnu
//! ```
//!
//! Needs `PAPERCLIP_QT_INCLUDE` and `PAPERCLIP_VENDOR_LIB_DIR`; see
//! `platform/device/native/README.md`.

use std::time::{Duration, Instant, SystemTime};

use paper_device::panel::{Panel, present};
use paper_device::session::VendorLockState;
use paper_device::stock::{StartBudget, Stock, Systemctl};
use paper_device::takeover::{DetachedWatchdog, Takeover, WAKELOCK_TAG};
use paper_device::vendor::VendorPanel;
use paper_device::{PixelRect, Refresh, WakeLock, Waveform};
use paper_home::{HomeScreen, ShelfEntry, ShelfGlyph, SystemFact};
use paper_sdk::{Canvas, SCREEN};

/// How long the watchdog gives the whole session before restoring stock itself.
const SESSION_BUDGET: Duration = Duration::from_secs(180);

fn main() {
    // Every failure below returns rather than panicking, so the ordinary path
    // and the failure path both run the same release sequence.
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("FAILED: {error}");
            1
        }
    };
    println!("--- exit {code} ---");
    std::process::exit(code);
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let probe = Systemctl;
    let before = probe.health();
    println!("preflight: {before:?}");
    if !before.is_healthy() {
        return Err("stock is not healthy before the session; refusing to take the display".into());
    }
    // Two separate budgets, because they count different things. `StartBudget`
    // counts starts *we* asked for; this counts systemd's own `Restart=`
    // retries, which is what a crashed Xochitl produces. The incident that
    // motivated this consumed two restarts nobody asked for.
    before.allows_takeover()?;
    println!(
        "  restarts remaining before OnFailure: {:?}",
        before.restarts_remaining()
    );

    // Recorded before anything is claimed, so the release can put the vendor
    // locks back exactly as Xochitl left them.
    let locks = VendorLockState::capture()?;
    println!(
        "  vendor registry before takeover: {}",
        if locks.registry.is_some() {
            "present"
        } else {
            "absent"
        }
    );

    // Wakelock before the display, always. On charge the tablet holds
    // `udev.charger` and will not suspend anyway, but relying on that is how a
    // session that runs off charge one day resumes into a second Xochitl.
    let wake = WakeLock::acquire(WAKELOCK_TAG)?;
    println!("wakelock held: {}", wake.tag());

    let stock = Stock::new(Systemctl, StartBudget::shared());
    let session = match Takeover::acquire(
        stock,
        wake,
        DetachedWatchdog::new(),
        locks,
        SESSION_BUDGET,
        SystemTime::now(),
    ) {
        Ok(session) => session,
        Err((error, _wake)) => return Err(error.into()),
    };
    println!("stock stopped; display should be free");

    // The panel is opened, used and dropped inside this block so that it is
    // always closed before the session releases, whatever happens in it.
    let outcome = drive_panel();

    // Report the panel work, then release regardless of how it went.
    match &outcome {
        Ok(report) => println!("{report}"),
        Err(error) => eprintln!("panel: {error}"),
    }

    let released = Takeover::release(session, SystemTime::now());
    println!("stock restored: {released:?}");
    released?;

    let after = probe.health();
    println!("postflight: {after:?}");
    // The check that was missing. Stock ending `active` with nothing failed is
    // not evidence of a clean release: systemd retries, and a third start that
    // works hides two core dumps behind it.
    println!(
        "  NRestarts {:?} -> {:?}; main start {:?} -> {:?}",
        before.n_restarts, after.n_restarts, before.main_start, after.main_start
    );
    let regressions = after.regressions_from(&before);
    if regressions.is_empty() {
        println!("VERIFIED: stock is where it was found, and started once");
    } else {
        return Err(format!("stock regressed: {regressions:?}").into());
    }
    outcome?;
    Ok(())
}

fn drive_panel() -> Result<String, Box<dyn std::error::Error>> {
    let opened = Instant::now();
    let mut panel = VendorPanel::open()?;
    let open_ms = opened.elapsed().as_millis();

    let size = panel.size();
    if size != SCREEN {
        return Err(format!("panel reports {size:?}, the SDK draws for {SCREEN:?}").into());
    }

    let mut canvas = Canvas::new(SCREEN).ok_or("could not allocate a canvas")?;
    let screen = home_screen();
    paper_home::render(&mut canvas, &screen);

    let mut lines = vec![format!(
        "panel {}x{}, opened in {open_ms}ms",
        size.width, size.height
    )];

    // Full-panel presents, one per waveform, timed. These are the numbers §9
    // asks for and nothing in this repository could produce until now.
    for (name, waveform, refresh) in [
        ("mono-quality full", Waveform::UI, Refresh::Full),
        ("colour full", Waveform::CONTENT, Refresh::Full),
        ("mono-ink partial", Waveform::INK, Refresh::Partial),
    ] {
        let started = Instant::now();
        present(&mut panel, &canvas, PixelRect::PANEL, waveform, refresh)?;
        lines.push(format!(
            "  swap {name}: {}ms",
            started.elapsed().as_millis()
        ));
    }

    // A small partial update is the latency that matters for live ink.
    let small = PixelRect::new(200, 400, 300, 300);
    let started = Instant::now();
    present(&mut panel, &canvas, small, Waveform::INK, Refresh::Partial)?;
    lines.push(format!(
        "  swap 300x300 partial ink: {}ms",
        started.elapsed().as_millis()
    ));

    // Clear before release. Not optional: skipping it leaves waveform residue
    // on the stock screen that Xochitl's own repaint does not remove.
    let started = Instant::now();
    panel.clear()?;
    lines.push(format!("  clear: {}ms", started.elapsed().as_millis()));

    Ok(lines.join("\n"))
}

fn home_screen() -> HomeScreen {
    HomeScreen {
        entries: vec![
            ShelfEntry::action("Chess", "two players", ShelfGlyph::Board),
            ShelfEntry::action("App Store", "catalog", ShelfGlyph::Store),
            ShelfEntry::action("Return to stock", "hand back the screen", ShelfGlyph::Stock),
        ],
        facts: vec![
            SystemFact::new("stage", "3 - device adapter proof"),
            SystemFact::new("transport", "vendor waveform engine"),
        ],
        pressed: None,
        status: "WWW-3".to_owned(),
    }
}
