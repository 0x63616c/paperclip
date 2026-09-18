//! The Settings app.
//!
//! Installed apps, storage, grants, catalog, platform facts and diagnostics —
//! six read-mostly pages plus one always-available action. Settings does not
//! reimplement anything stock owns and it does not reach into the storage
//! layout or an install policy directly (§6, §11): every read and every
//! write is a [`paper_sdk::AdminQuery`] sent over the protocol and answered
//! by whichever process is running this app's connection (WWW-71,
//! ADR-0028) — this crate no longer depends on `paper-packages` at all.

mod app;
mod confirm;
mod nav;
mod pages;
mod screen;

pub use app::SettingsApp;
pub use confirm::{ConfirmDialog, ConfirmLayout};
pub use nav::{NAV_HEIGHT, NavLayout, SettingsPage};
pub use pages::{AppRowLayout, AppsLayout, GrantsLayout};
pub use screen::{PageLayout, Press, SettingsLayout, SettingsScreen, render};
