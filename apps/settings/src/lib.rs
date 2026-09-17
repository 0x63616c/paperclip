//! The Settings app.
//!
//! Installed apps, storage, grants, catalog, platform facts and diagnostics —
//! six read-mostly pages plus one always-available action, all built on top
//! of [`host::SettingsHost`] (see `host` for its two implementations).
//! Settings does not reimplement anything stock owns and it does not
//! manipulate the storage layout or an install policy directly (§6, §11):
//! every read and every write goes through that trait.

mod app;
mod confirm;
mod host;
mod nav;
mod pages;
mod screen;

pub use app::SettingsApp;
pub use confirm::{ConfirmDialog, ConfirmLayout};
pub use host::{
    CatalogStatus, DiagnosticEntry, GrantSummary, HostOpError, InstalledAppSummary, LiveHost,
    PlaceholderHost, PlatformInfo, SettingsHost, StorageBucket, StorageUsage,
};
pub use nav::{NAV_HEIGHT, NavLayout, SettingsPage};
pub use pages::{AppRowLayout, AppsLayout, GrantsLayout};
pub use screen::{PageLayout, Press, SettingsLayout, SettingsScreen, render};
