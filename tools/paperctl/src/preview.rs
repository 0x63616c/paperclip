//! `paperctl preview` — the desktop window.

use std::path::PathBuf;

use paper_sdk::SCREEN;
use paper_sdk::desktop::{Capture, PreviewControl, PreviewEvent, PreviewOptions};

use crate::ScreenArg;
use crate::error::CommandError;
use crate::screens::{Screen, Screens};

/// Open the desktop preview window.
#[derive(Debug, clap::Args)]
pub(crate) struct PreviewArgs {
    /// Which screen to open on.
    #[arg(long, value_enum, default_value = "home")]
    screen: ScreenArg,
    /// Tallest the window may open, in logical points.
    #[arg(long, default_value_t = 880.0)]
    max_height: f64,
    /// Exact window size in logical points, as `WIDTHxHEIGHT`. Overrides
    /// `--max-height`; a window whose shape does not match the canvas is how
    /// you see the letterbox.
    #[arg(long, value_name = "WIDTHxHEIGHT", value_parser = parse_logical_size)]
    window_size: Option<(f64, f64)>,
    /// Write the canvas at full target resolution to this PNG once the first
    /// frame has been presented.
    #[arg(long, value_name = "PATH")]
    capture_canvas: Option<PathBuf>,
    /// Write the whole window buffer, letterbox included, to this PNG.
    #[arg(long, value_name = "PATH")]
    capture_window: Option<PathBuf>,
    /// Close the window as soon as the capture is written.
    #[arg(long)]
    exit_after_capture: bool,
}

fn parse_logical_size(raw: &str) -> Result<(f64, f64), String> {
    let (width, height) = raw
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected `WIDTHxHEIGHT`, got `{raw}`"))?;
    let parse = |part: &str, axis: &str| {
        part.trim()
            .parse::<f64>()
            .ok()
            .filter(|value| *value > 0.0)
            .ok_or_else(|| format!("`{part}` is not a positive {axis}"))
    };
    Ok((parse(width, "width")?, parse(height, "height")?))
}

/// Opens the preview window and runs until it closes.
pub(crate) fn run(args: PreviewArgs) -> Result<(), CommandError> {
    let mut screens = Screens::new()?;
    screens.show(
        args.screen
            .screens()
            .first()
            .copied()
            .unwrap_or(Screen::Home),
    );

    let mut options = PreviewOptions::new("Paperclip preview", SCREEN);
    options.max_logical_height = args.max_height;
    options.logical_size = args.window_size;
    options.capture = Capture {
        canvas: args.capture_canvas,
        window: args.capture_window,
        exit_after: args.exit_after_capture,
    };

    eprintln!(
        "preview: {}x{} canvas \u{00B7} keys: h home, c chess, s settings, f flip, n clear, tab next, esc quit",
        SCREEN.width, SCREEN.height
    );

    paper_sdk::desktop::run(options, |event| {
        match event {
            PreviewEvent::Render(canvas) => screens.render(canvas),
            PreviewEvent::Pointer(pointer) => screens.pointer(pointer),
            PreviewEvent::Key(key) => screens.key(key),
            // `PreviewEvent` is non-exhaustive so the backend can gain an
            // event without breaking every consumer. Ignoring the unknown
            // one is right: a preview that has not learned about it has
            // nothing to do with it.
            _ => {}
        }
        PreviewControl::Continue
    })?;

    Ok(())
}
