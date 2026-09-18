//! One module per section of the card. Each exposes a `render` that draws
//! into a content rectangle already carved out by [`crate::nav`], and the
//! two sections with any interaction ([`ghosting`], [`damage`]) also expose
//! their own state and a hit test.

pub(crate) mod colour;
pub(crate) mod damage;
pub(crate) mod geometry;
pub(crate) mod ghosting;
pub(crate) mod greyscale;
pub(crate) mod lines_and_text;
pub(crate) mod waveform;
