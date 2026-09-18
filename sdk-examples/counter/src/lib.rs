//! Counter — the SDK's worked example, and what `paperctl new app` scaffolds.
//!
//! One screen: a count, a button that increments it, and the footer HOME
//! action every screen carries (§7). Small enough to read end to end in one
//! sitting, and everything in it is the pattern a real app is expected to
//! follow, not a shortcut a real app should not take:
//!
//! - [`screen::layout`] computes every rectangle from the viewport size
//!   alone — no [`paper_sdk::Canvas`], because geometry does not need one.
//!   [`app::CounterApp`] calls it straight from
//!   [`Event`](paper_sdk::Event), so a tap is hit-testable before this app
//!   has ever drawn a frame.
//! - [`screen::draw`] takes that layout and only draws; it invents no
//!   rectangle [`screen::layout`] did not already decide.
//! - [`screen::render`] is [`screen::layout`] then [`screen::draw`], for the
//!   caller that wants both — see WWW-51's report for why a third function
//!   is what keeps the first two honest rather than one function that
//!   sometimes takes a canvas and sometimes does not.
//!
//! Copy this directory, `paperctl new app <name>` does exactly that, then:
//!
//! - `paper.toml`: the app id, name and entrypoint.
//! - `Cargo.toml`: the crate name and the `[[bin]]` name.
//! - Everything under `src/`: `Counter`/`counter` renamed to your own.

mod app;
mod screen;

pub use app::CounterApp;
pub use screen::{CounterLayout, CounterScreen, draw, layout, render};
