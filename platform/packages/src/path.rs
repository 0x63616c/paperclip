//! Package-relative paths, re-exported.
//!
//! [`RelativePath`] moved to `paper_protocol` in WWW-5. Both halves of the
//! contract need it — a `paper.toml` declares an entrypoint and a list of
//! assets, and an app asks its SDK to open an asset by name — and two
//! definitions of the same rule drift. This module is the name the rest of
//! this crate knows it by; the crate's public surface re-exports it from
//! `lib.rs` as it always did.

pub(crate) use paper_protocol::RelativePath;
