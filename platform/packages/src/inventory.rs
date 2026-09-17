//! What the App Store shows, without the drawing (§5, §6).
//!
//! One merge of three sources — what is installed, what a catalog offers, and
//! what has actually managed to start — into the rows a person reads. The App
//! Store app renders this; `paperctl list` prints it; both get the same
//! answers because there is one model and not two.
//!
//! # Offline is a state, not an error
//!
//! [`Inventory::survey`] takes an *optional* catalog view, because a device
//! with no reachable catalog still has apps and still has to say so. With a
//! stale cached view the rows are still populated and [`CatalogStatus::stale`]
//! is set; with nothing at all the installed rows stand alone and every
//! `available` is `None`. Neither case is an empty screen (§12).
//!
//! # A version the catalog has forgotten is still installed
//!
//! [`AppState::InstalledNotOffered`] exists for that. A catalog that drops an
//! entry does not uninstall anything, and an App Store that hid the row would
//! be showing the catalog's opinion rather than the device's state.
//!
//! # Release notes are not fetched here
//!
//! Surveying is one catalog read. Notes live in per-release descriptors, so
//! fetching them for every app to draw a list would turn opening the App Store
//! into N round trips over a home LAN that may not be there. [`Inventory::notes`]
//! fetches one, when someone asks to see one.

use semver::Version;

use crate::catalog::{Catalog, CatalogError, CatalogView, Transport};
use crate::id::{AppId, DisplayName};
use crate::launch::{Health, Ledger};
use crate::store::{Layout, StoreError};

/// Which catalog the rows were built against, and how current it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogStatus {
    /// The catalog's name.
    pub name: String,
    /// Its serial.
    pub serial: u64,
    /// Whether this came from the cache because the catalog was unreachable.
    pub stale: bool,
}

/// What a person needs to know about one app at a glance.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AppEntry {
    /// The app's stable id.
    pub app: AppId,
    /// Its display name, from the catalog when it is offered and from the
    /// installed manifest otherwise.
    pub name: DisplayName,
    /// The selected version, if one is selected and complete.
    pub installed: Option<Version>,
    /// The version a rollback would select.
    pub fallback: Option<Version>,
    /// Every complete release on disk, oldest first.
    pub on_disk: Vec<Version>,
    /// The newest stable version the catalog offers.
    pub available: Option<Version>,
    /// The newest version the catalog offers including prereleases, when that
    /// differs from [`Self::available`]. Shown, never installed by default.
    pub available_prerelease: Option<Version>,
    /// What the launch ledger records, if anything.
    pub health: Option<Health>,
    /// The one-word summary.
    pub state: AppState,
}

impl AppEntry {
    /// Whether installing would move this app forward.
    pub fn update_available(&self) -> bool {
        matches!(self.state, AppState::UpdateAvailable)
    }

    /// Whether a rollback has somewhere to go.
    pub fn can_roll_back(&self) -> bool {
        self.fallback.is_some() && self.installed.is_some()
    }
}

/// The one-word state of an app, for the row a person reads.
///
/// Not `#[non_exhaustive]`, for the same reason as
/// [`Step`](crate::install::Step): a state with no word is a blank row, so
/// adding one should break the code that draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    /// Installed and selected, with no catalog to compare against.
    ///
    /// Distinct from [`Self::UpToDate`] on purpose: with no catalog reachable
    /// and none cached, "up to date" is a claim nothing here can support.
    Installed,
    /// Installed, selected, and the newest the catalog offers.
    UpToDate,
    /// Installed, and the catalog offers something newer.
    UpdateAvailable,
    /// Offered by the catalog, not installed here.
    Available,
    /// Installed, and the catalog no longer lists it. Still launchable.
    InstalledNotOffered,
    /// Installed and selected, but it has never managed to start.
    ///
    /// Takes precedence over every other state, because it is the only one
    /// that needs a person to do something.
    Failing,
    /// Release directories on disk, but nothing selected — an install that was
    /// interrupted before activation, or a selection recovery could not repair.
    NothingSelected,
}

impl AppState {
    /// The word to put in the row.
    pub fn label(self) -> &'static str {
        match self {
            AppState::Installed => "installed",
            AppState::UpToDate => "up to date",
            AppState::UpdateAvailable => "update available",
            AppState::Available => "available",
            AppState::InstalledNotOffered => "installed; not in the catalog",
            AppState::Failing => "not starting",
            AppState::NothingSelected => "nothing selected",
        }
    }
}

/// Installed state and catalog offerings, merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    entries: Vec<AppEntry>,
    catalog: Option<CatalogStatus>,
}

impl Inventory {
    /// Builds the rows.
    ///
    /// `view` is optional: `None` is a device that has never reached a catalog,
    /// and a view with `stale` set is one that cannot reach it now. Both
    /// produce a full list of what is installed.
    pub fn survey(
        layout: &Layout,
        view: Option<&CatalogView>,
        ledger: &Ledger,
    ) -> Result<Self, StoreError> {
        let mut entries: Vec<AppEntry> = Vec::new();

        for app in layout.apps()? {
            let installed = layout.current(&app)?;
            let on_disk = layout.installed_versions(&app)?;
            if installed.is_none() && on_disk.is_empty() {
                // An app directory with nothing complete in it is debris, not
                // an app. `PackageManager::recover` is what removes it.
                continue;
            }
            let offered = view.and_then(|view| view.index.newest_stable(&app));
            let prerelease = view.and_then(|view| view.index.newest_any(&app));
            let health = ledger.health(&app)?;

            let name = offered
                .map(|entry| entry.name().clone())
                .or_else(|| installed_name(layout, &app, installed.as_ref()))
                .unwrap_or_else(|| fallback_name(&app));

            entries.push(AppEntry {
                state: state_of(
                    installed.as_ref(),
                    offered.map(|e| e.version()),
                    &on_disk,
                    health.as_ref(),
                    view.is_some(),
                ),
                app,
                name,
                installed,
                fallback: None,
                on_disk,
                available: offered.map(|e| e.version().clone()),
                available_prerelease: prerelease
                    .map(|e| e.version().clone())
                    .filter(|v| !v.pre.is_empty()),
                health,
            });
        }

        // Fallbacks need a second pass only because `previous` is a separate
        // read; doing it inline would not make it cheaper.
        for entry in &mut entries {
            entry.fallback = layout.previous(&entry.app)?;
        }

        if let Some(view) = view {
            for offered in view.index.entries() {
                if entries.iter().any(|e| &e.app == offered.app()) {
                    continue;
                }
                let newest = view.index.newest_stable(offered.app());
                let Some(newest) = newest else { continue };
                entries.push(AppEntry {
                    app: offered.app().clone(),
                    name: offered.name().clone(),
                    installed: None,
                    fallback: None,
                    on_disk: Vec::new(),
                    available: Some(newest.version().clone()),
                    available_prerelease: view
                        .index
                        .newest_any(offered.app())
                        .map(|e| e.version().clone())
                        .filter(|v| !v.pre.is_empty()),
                    health: None,
                    state: AppState::Available,
                });
            }
        }

        entries.sort_by(|a, b| a.app.cmp(&b.app));
        entries.dedup_by(|a, b| a.app == b.app);

        Ok(Self {
            entries,
            catalog: view.map(|view| CatalogStatus {
                name: view.index.catalog().to_owned(),
                serial: view.index.serial(),
                stale: view.stale,
            }),
        })
    }

    /// Every row, in app id order.
    pub fn entries(&self) -> &[AppEntry] {
        &self.entries
    }

    /// One row, if the app is known at all.
    pub fn entry(&self, app: &AppId) -> Option<&AppEntry> {
        self.entries.iter().find(|entry| &entry.app == app)
    }

    /// Which catalog these rows were built against, if any.
    pub fn catalog(&self) -> Option<&CatalogStatus> {
        self.catalog.as_ref()
    }

    /// Whether the catalog could not be reached, so the rows may be behind.
    pub fn is_stale(&self) -> bool {
        self.catalog.as_ref().is_some_and(|status| status.stale)
    }

    /// Rows a person would want to act on: updates, and anything not starting.
    pub fn needing_attention(&self) -> impl Iterator<Item = &AppEntry> {
        self.entries.iter().filter(|entry| {
            matches!(
                entry.state,
                AppState::UpdateAvailable | AppState::Failing | AppState::NothingSelected
            )
        })
    }

    /// Fetches the release notes for one offered version.
    ///
    /// Separate from [`Self::survey`] and deliberately one app at a time: this
    /// is a signed descriptor fetch, and doing it for every row would make
    /// opening a list N round trips.
    pub fn notes<T: Transport>(
        catalog: &Catalog<T>,
        view: &CatalogView,
        app: &AppId,
        version: &Version,
    ) -> Result<String, CatalogError> {
        let entry =
            view.index
                .exact(app, version)
                .ok_or_else(|| CatalogError::IndexDisagreement {
                    listed: format!("{app} {version}"),
                    signed: "nothing the catalog offers".to_owned(),
                })?;
        Ok(catalog.release(entry)?.release().notes().to_owned())
    }
}

/// The state a row is in, from the four facts that decide it.
fn state_of(
    installed: Option<&Version>,
    offered: Option<&Version>,
    on_disk: &[Version],
    health: Option<&Health>,
    have_catalog: bool,
) -> AppState {
    // A release that has never started outranks everything else: it is the one
    // state where the person has to do something.
    if let Some(health) = health
        && let Some(installed) = installed
        && &health.version == installed
        && !health.started
        && health.attempts >= Ledger::MAX_ATTEMPTS
    {
        return AppState::Failing;
    }
    match (installed, offered) {
        (None, Some(_)) => AppState::Available,
        (None, None) if !on_disk.is_empty() => AppState::NothingSelected,
        (None, None) => AppState::Available,
        (Some(_), None) if have_catalog => AppState::InstalledNotOffered,
        (Some(_), None) => AppState::Installed,
        (Some(installed), Some(offered)) if offered > installed => AppState::UpdateAvailable,
        (Some(_), Some(_)) => AppState::UpToDate,
    }
}

/// The display name from the installed release's own manifest.
fn installed_name(
    layout: &Layout,
    app: &AppId,
    installed: Option<&Version>,
) -> Option<DisplayName> {
    let version = installed?;
    crate::manifest::Manifest::read_package(&layout.release_dir(app, version))
        .ok()
        .map(|manifest| manifest.name().clone())
}

/// The last resort when neither the catalog nor a manifest can name an app.
///
/// The id's leaf is not a display name and is not pretending to be one, but it
/// beats the blank row the alternative would draw.
fn fallback_name(app: &AppId) -> DisplayName {
    DisplayName::from_lossy(app.leaf())
}

#[cfg(all(test, feature = "publishing"))]
mod tests {
    use std::fs;

    use semver::Version;

    use super::{AppState, Inventory};
    use crate::id::AppId;
    use crate::launch::Ledger;
    use crate::store::{Layout, Marker, RELEASE_MARKER, atomic_write};

    fn app() -> AppId {
        "dev.calum.chess".parse().unwrap()
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// A complete release on disk, with a manifest that names the app.
    fn install(layout: &Layout, app: &AppId, text: &str, select: bool) {
        let version = version(text);
        let dir = layout.release_dir(app, &version);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("paper.toml"),
            format!(
                "[app]\nid = \"{app}\"\nname = \"Chess\"\nversion = \"{text}\"\n\
                 protocol = \"1.0\"\nentrypoint = \"bin/chess\"\n"
            ),
        )
        .unwrap();
        let marker = Marker {
            digest: crate::digest::Digest::of_bytes(text.as_bytes()),
            signer: "0011223344556677".parse().unwrap(),
            installed: 0,
        };
        fs::write(dir.join(RELEASE_MARKER), marker.to_document()).unwrap();
        if select {
            atomic_write(&layout.current_file(app), format!("{text}\n").as_bytes()).unwrap();
        }
    }

    #[test]
    fn an_installed_app_with_no_catalog_at_all_still_appears() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install(&layout, &app(), "0.1.0", true);

        let inventory = Inventory::survey(&layout, None, &Ledger::new(layout.clone())).unwrap();
        let entry = inventory.entry(&app()).unwrap();

        assert_eq!(entry.installed, Some(version("0.1.0")));
        assert_eq!(entry.available, None);
        // Not `UpToDate`: with no catalog, nothing here can say that.
        assert_eq!(entry.state, AppState::Installed);
        assert_eq!(entry.name.as_str(), "Chess");
        assert!(inventory.catalog().is_none());
        assert!(!inventory.is_stale());
    }

    #[test]
    fn a_failing_release_outranks_every_other_state() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install(&layout, &app(), "0.1.0", true);

        let ledger = Ledger::new(layout.clone());
        for _ in 0..Ledger::MAX_ATTEMPTS {
            ledger.record_attempt(&app(), &version("0.1.0")).unwrap();
        }
        ledger
            .record_failure(&app(), &version("0.1.0"), "exited immediately")
            .unwrap();

        let inventory = Inventory::survey(&layout, None, &ledger).unwrap();
        let entry = inventory.entry(&app()).unwrap();
        assert_eq!(entry.state, AppState::Failing);
        assert_eq!(
            entry.health.as_ref().unwrap().last_failure.as_deref(),
            Some("exited immediately")
        );
        assert_eq!(inventory.needing_attention().count(), 1);
    }

    #[test]
    fn a_release_directory_with_nothing_selected_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install(&layout, &app(), "0.1.0", false);

        let inventory = Inventory::survey(&layout, None, &Ledger::new(layout.clone())).unwrap();
        let entry = inventory.entry(&app()).unwrap();
        assert_eq!(entry.state, AppState::NothingSelected);
        assert_eq!(entry.on_disk, vec![version("0.1.0")]);
    }

    #[test]
    fn an_app_directory_holding_only_debris_is_not_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        // A release directory with no completion marker.
        fs::create_dir_all(layout.release_dir(&app(), &version("0.1.0"))).unwrap();

        let inventory = Inventory::survey(&layout, None, &Ledger::new(layout.clone())).unwrap();
        assert!(inventory.entries().is_empty());
    }

    #[test]
    fn rollback_is_offered_only_when_there_is_somewhere_to_go() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install(&layout, &app(), "0.1.0", false);
        install(&layout, &app(), "0.2.0", true);

        let before = Inventory::survey(&layout, None, &Ledger::new(layout.clone())).unwrap();
        assert!(!before.entry(&app()).unwrap().can_roll_back());

        atomic_write(&layout.previous_file(&app()), b"0.1.0\n").unwrap();
        let after = Inventory::survey(&layout, None, &Ledger::new(layout.clone())).unwrap();
        let entry = after.entry(&app()).unwrap();
        assert!(entry.can_roll_back());
        assert_eq!(entry.fallback, Some(version("0.1.0")));
    }
}
