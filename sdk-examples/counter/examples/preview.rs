//! `cargo run -p paper-counter --features preview --example preview`
//!
//! Opens Counter in a real window on the Mac — a look at what
//! [`paper_counter::render`] draws and what a tap on it does, without needing
//! a host, a socket or a launched process. It drives [`CounterScreen`] and
//! [`layout`]/[`draw`] directly rather than going through [`paper_sdk::run`]
//! and the wire protocol [`App`](paper_sdk::App) speaks: this window plays
//! the host's part by hand, which is a shorter path to "something on screen"
//! than standing up a session for a demo that has no host on the other end
//! of it. `tools/paperctl`'s own `dev` command is the real host-side loop —
//! see its module doc for how a launched, unmodified app fits into one.

use paper_counter::{CounterScreen, draw, layout};
use paper_sdk::desktop::{self, PreviewControl, PreviewEvent, PreviewOptions};
use paper_sdk::{Action, SCREEN};

fn main() {
    let mut screen = CounterScreen::default();

    let options = PreviewOptions::new("paperctl \u{2014} paper-counter preview", SCREEN);
    let ran = desktop::run(options, |event| match event {
        PreviewEvent::Render(canvas) => {
            draw(canvas, &screen, &layout(SCREEN));
            PreviewControl::Continue
        }
        PreviewEvent::Pointer(pointer) => match screen.press(&layout(SCREEN), &pointer) {
            Action::Home => {
                println!("preview: HOME pressed \u{2014} closing (nothing to hand back to)");
                PreviewControl::Exit
            }
            _ => PreviewControl::Continue,
        },
        _ => PreviewControl::Continue,
    });

    if let Err(error) = ran {
        eprintln!("paper-counter preview: {error}");
        std::process::exit(1);
    }
}
