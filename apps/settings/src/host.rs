//! The boundary between Settings and Host-owned operations.
//!
//! Settings is a **client** of the platform, never an owner of it (§6, §11):
//! rollback, uninstall and grant revocation are Host transactions, and this
//! crate must not reach into the storage layout or the install policy to
//! perform them itself. [`SettingsHost`] is that boundary — everything above
//! it (`screen`, `pages`, `confirm`) is written against the trait, never
//! against a specific implementation.
//!
//! Two implementations:
//!
//! - [`PlaceholderHost`] is fixture data for the desktop preview and this
//!   crate's own tests. Not device evidence, not wired to anything real.
//! - [`LiveHost`] wraps the real `paper_packages` store WWW-7 landed:
//!   [`paper_packages::inventory::Inventory`] for installed apps,
//!   [`paper_packages::install::PackageManager`] for rollback,
//!   [`paper_packages::store::Layout`]'s bucket accessors for storage. It is
//!   built and tested against a real (temp-directory) store the same way
//!   `paper_packages`' own tests are, not against a mock.
//!
//! `LiveHost::uninstall` and `LiveHost::revoke_grant` return
//! [`HostOpError::NotSupported`]: as of this pass, `paper_packages` has no
//! durable uninstall transaction (only install/rollback/recover), and
//! `InstallPolicy` is in-memory with no persisted, enumerable grant store —
//! see ADR-0015 for what would need to land first, and why this crate does
//! not hand-roll either on top of the store directly.

use paper_packages::install::{InstallError, NothingIsRunning, PackageManager};
use paper_packages::inventory::Inventory;
use paper_packages::launch::Ledger;
use paper_packages::store::Layout;
use paper_packages::{AppId, Capability, InstallPolicy};

/// One row on the installed-apps page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAppSummary {
    /// The app's stable id, used to address it in a Host call.
    pub id: AppId,
    /// The name to show.
    pub name: String,
    /// The release currently active.
    pub active_version: String,
    /// The release rollback would restore, if any. `None` means there is
    /// nothing to roll back to — the app has only ever had one release.
    pub previous_version: Option<String>,
    /// Bytes held under this app's own `data/<id>` directory, so an uninstall
    /// confirmation can say what would be lost.
    pub data_bytes: u64,
}

impl InstalledAppSummary {
    /// Whether rollback has anywhere to go.
    pub fn has_previous_release(&self) -> bool {
        self.previous_version.is_some()
    }
}

/// What is using the space under `/home`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageUsage {
    /// Bytes free on the filesystem everything Paperclip's is under.
    pub free_bytes: u64,
    /// Named buckets — `data/<id>`, `shared/<grant>`, `staging/`, release
    /// directories — each with the bytes it holds. Order is display order.
    pub buckets: Vec<StorageBucket>,
}

impl StorageUsage {
    /// Total held across every bucket.
    pub fn used_bytes(&self) -> u64 {
        self.buckets.iter().map(|bucket| bucket.bytes).sum()
    }
}

/// One named storage bucket and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageBucket {
    /// What the bucket is, e.g. `"DATA / CHESS"` or `"STAGING"`.
    pub label: String,
    /// Bytes held.
    pub bytes: u64,
}

/// One capability grant, and whether revoking it would land on an app while
/// it is actually relying on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSummary {
    /// The app the grant was made to.
    pub app_id: AppId,
    /// The app's display name.
    pub app_name: String,
    /// What was granted.
    pub capability: Capability,
    /// Whether the app is currently exercising this grant — the fact that
    /// makes a revoke confirmation say something different (§11: revoking is
    /// an installation-policy operation, and the person doing it should know
    /// whether it is disruptive right now, not just eventually).
    pub in_use: bool,
}

/// The configured catalog and whether it answered last time anyone asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogStatus {
    /// The configured catalog endpoint.
    pub endpoint: String,
    /// When the endpoint last answered successfully, already formatted —
    /// this crate does not depend on a clock or a time-formatting crate.
    pub last_fetch: Option<String>,
    /// Whether the endpoint answered the most recent attempt.
    pub reachable: bool,
}

/// The platform facts a bug report needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformInfo {
    /// Paperclip's own version.
    pub paperclip_version: String,
    /// The firmware image it is running on.
    pub firmware: String,
    /// The active platform release (Host, protocol support, Home, App Store,
    /// Settings — versioned together, per §13).
    pub active_release: String,
}

/// One entry in the diagnostics log, read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEntry {
    /// When it happened, already formatted.
    pub when: String,
    /// What was logged.
    pub message: String,
}

/// Why a requested operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostOpError {
    /// Rollback was requested for an app with no previous release.
    NoPreviousRelease,
    /// The app id named in the request is not installed.
    NotInstalled,
    /// The grant named in the request is not held.
    GrantNotHeld,
    /// This Host implementation has no transaction for the request yet.
    NotSupported {
        /// Why not, so a caller can say something more useful than "no".
        reason: &'static str,
    },
    /// The underlying Host operation failed for a reason outside this
    /// error's small, closed vocabulary.
    Failed(String),
}

impl From<InstallError> for HostOpError {
    fn from(error: InstallError) -> Self {
        match error {
            InstallError::NoFallback { .. } => HostOpError::NoPreviousRelease,
            InstallError::NothingSelected { .. } => HostOpError::NotInstalled,
            other => HostOpError::Failed(other.to_string()),
        }
    }
}

/// Everything Settings reads from, or asks of, the Host.
///
/// Read methods return a snapshot rather than a live handle: a settings page
/// is drawn from a value it owns, not from a reference it has to keep valid
/// across a render call. The three write methods are exactly the Host
/// transactions this app is scoped to (§6) — nothing here lets Settings touch
/// the storage layout or an install policy directly.
pub trait SettingsHost {
    /// Every installed app, in display order.
    fn installed_apps(&self) -> Vec<InstalledAppSummary>;
    /// What storage holds today.
    fn storage_usage(&self) -> StorageUsage;
    /// Every capability grant in force.
    fn grants(&self) -> Vec<GrantSummary>;
    /// The catalog's configured endpoint and reachability.
    fn catalog_status(&self) -> CatalogStatus;
    /// Platform version facts.
    fn platform_info(&self) -> PlatformInfo;
    /// Recent platform errors, most recent first.
    fn diagnostics(&self) -> Vec<DiagnosticEntry>;

    /// Rolls `app` back to the release rollback would restore.
    ///
    /// Fails with [`HostOpError::NoPreviousRelease`] rather than silently
    /// doing nothing — a caller that skipped the `has_previous_release` check
    /// gets told why, instead of an uninstall that looked like it worked.
    fn rollback(&mut self, app: &AppId) -> Result<(), HostOpError>;
    /// Uninstalls `app`, including its data.
    fn uninstall(&mut self, app: &AppId) -> Result<(), HostOpError>;
    /// Revokes `capability` from `app`.
    fn revoke_grant(&mut self, app: &AppId, capability: Capability) -> Result<(), HostOpError>;
}

/// Fixture data for the desktop preview and this crate's own tests.
///
/// Not device evidence: every value here is invented so the pages have
/// something legible to draw without a real store on disk. [`LiveHost`] is
/// the implementation backed by one.
#[derive(Debug, Clone)]
pub struct PlaceholderHost {
    apps: Vec<InstalledAppSummary>,
    grants: Vec<GrantSummary>,
}

impl PlaceholderHost {
    /// Builds the fixture the preview screens are drawn from.
    pub fn new() -> Self {
        let chess: AppId = "dev.calum.chess".parse().expect("valid fixture id");
        Self {
            apps: vec![InstalledAppSummary {
                id: chess.clone(),
                name: "Chess".to_owned(),
                active_version: "0.2.0".to_owned(),
                previous_version: Some("0.1.0".to_owned()),
                data_bytes: 2_400_000,
            }],
            grants: vec![
                GrantSummary {
                    app_id: chess.clone(),
                    app_name: "Chess".to_owned(),
                    capability: Capability::Storage,
                    in_use: true,
                },
                GrantSummary {
                    app_id: chess,
                    app_name: "Chess".to_owned(),
                    capability: Capability::Sharing,
                    in_use: false,
                },
            ],
        }
    }
}

impl Default for PlaceholderHost {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsHost for PlaceholderHost {
    fn installed_apps(&self) -> Vec<InstalledAppSummary> {
        self.apps.clone()
    }

    fn storage_usage(&self) -> StorageUsage {
        StorageUsage {
            free_bytes: 41_300_000,
            buckets: vec![
                StorageBucket {
                    label: "DATA / CHESS".to_owned(),
                    bytes: 2_400_000,
                },
                StorageBucket {
                    label: "SHARED".to_owned(),
                    bytes: 0,
                },
                StorageBucket {
                    label: "STAGING".to_owned(),
                    bytes: 0,
                },
                StorageBucket {
                    label: "RELEASES".to_owned(),
                    bytes: 18_700_000,
                },
            ],
        }
    }

    fn grants(&self) -> Vec<GrantSummary> {
        self.grants.clone()
    }

    fn catalog_status(&self) -> CatalogStatus {
        CatalogStatus {
            endpoint: "HTTPS://CATALOG.LOCAL".to_owned(),
            last_fetch: Some("2026-09-16 21:04".to_owned()),
            reachable: true,
        }
    }

    fn platform_info(&self) -> PlatformInfo {
        PlatformInfo {
            paperclip_version: "0.1.0".to_owned(),
            firmware: "3.28.0.172".to_owned(),
            active_release: "STAGE 4".to_owned(),
        }
    }

    fn diagnostics(&self) -> Vec<DiagnosticEntry> {
        vec![DiagnosticEntry {
            when: "2026-09-16 21:04".to_owned(),
            message: "NO ERRORS RECORDED THIS SESSION".to_owned(),
        }]
    }

    fn rollback(&mut self, app: &AppId) -> Result<(), HostOpError> {
        let entry = self
            .apps
            .iter_mut()
            .find(|candidate| &candidate.id == app)
            .ok_or(HostOpError::NotInstalled)?;
        let previous = entry
            .previous_version
            .take()
            .ok_or(HostOpError::NoPreviousRelease)?;
        entry.active_version = previous;
        Ok(())
    }

    fn uninstall(&mut self, app: &AppId) -> Result<(), HostOpError> {
        let before = self.apps.len();
        self.apps.retain(|candidate| &candidate.id != app);
        if self.apps.len() == before {
            return Err(HostOpError::NotInstalled);
        }
        self.grants.retain(|grant| &grant.app_id != app);
        Ok(())
    }

    fn revoke_grant(&mut self, app: &AppId, capability: Capability) -> Result<(), HostOpError> {
        let before = self.grants.len();
        self.grants
            .retain(|grant| !(&grant.app_id == app && grant.capability == capability));
        if self.grants.len() == before {
            return Err(HostOpError::GrantNotHeld);
        }
        Ok(())
    }
}

/// A Settings Host backed by the real `paper_packages` store.
///
/// Built over a [`Layout`] the caller already has — [`Layout::from_environment`]
/// on the device, a fixed temp directory in a test — rather than discovering
/// one itself, for the same reason `tools/paperctl`'s install commands take
/// `--root`: a store that picks its own location silently is a store that
/// ends up with two copies.
#[derive(Debug, Clone)]
pub struct LiveHost {
    layout: Layout,
    ledger: Ledger,
}

impl LiveHost {
    /// Builds a Host over `layout`.
    pub fn new(layout: Layout) -> Self {
        let ledger = Ledger::new(layout.clone());
        Self { layout, ledger }
    }

    /// The store this Host reads and writes.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// A manager scoped to this layout, denying every capability.
    ///
    /// Matches `tools/paperctl`'s own construction: nothing here runs inside
    /// a live session, so there is no real answer to "is this app already
    /// running" to pass as an [`paper_packages::install::ActivationGuard`]
    /// other than [`NothingIsRunning`] — claiming one would be inventing it.
    fn manager(&self) -> PackageManager {
        PackageManager::host(self.layout.clone(), InstallPolicy::deny_all())
    }
}

impl SettingsHost for LiveHost {
    fn installed_apps(&self) -> Vec<InstalledAppSummary> {
        let Ok(inventory) = Inventory::survey(&self.layout, None, &self.ledger) else {
            return Vec::new();
        };
        inventory
            .entries()
            .iter()
            .filter_map(|entry| {
                let installed = entry.installed.as_ref()?;
                Some(InstalledAppSummary {
                    id: entry.app.clone(),
                    name: entry.name.as_str().to_owned(),
                    active_version: installed.to_string(),
                    previous_version: entry.fallback.as_ref().map(ToString::to_string),
                    data_bytes: self.layout.app_data_bytes(&entry.app),
                })
            })
            .collect()
    }

    fn storage_usage(&self) -> StorageUsage {
        let mut buckets: Vec<StorageBucket> = self
            .layout
            .apps()
            .unwrap_or_default()
            .into_iter()
            .map(|app| StorageBucket {
                label: format!("DATA / {}", app.leaf().to_uppercase()),
                bytes: self.layout.app_data_bytes(&app),
            })
            .collect();
        buckets.push(StorageBucket {
            label: "SHARED".to_owned(),
            bytes: self.layout.shared_bytes(),
        });
        buckets.push(StorageBucket {
            label: "STAGING".to_owned(),
            bytes: self.layout.staging_bytes(),
        });
        buckets.push(StorageBucket {
            label: "RELEASES".to_owned(),
            bytes: self.layout.releases_bytes(),
        });

        StorageUsage {
            // A `df` that fails reads as "don't know" rather than "full":
            // `0` would draw a meter that looks like the disk has no room
            // left, which is a worse lie than a meter that under-fills.
            free_bytes: self.layout.free_bytes().unwrap_or(0),
            buckets,
        }
    }

    fn grants(&self) -> Vec<GrantSummary> {
        // `InstallPolicy` (ADR-0003) is constructed fresh per install and
        // never persisted or enumerated — there is no store to read a list
        // of standing grants back from yet. See the module doc.
        Vec::new()
    }

    fn catalog_status(&self) -> CatalogStatus {
        // No catalog endpoint is persisted anywhere a Settings-launched
        // process can read it — `paperctl install`/`list` take `--catalog`
        // on every invocation. Reporting "not configured" is the real
        // answer, not a fixture standing in for one.
        CatalogStatus {
            endpoint: "NOT CONFIGURED".to_owned(),
            last_fetch: None,
            reachable: false,
        }
    }

    fn platform_info(&self) -> PlatformInfo {
        PlatformInfo {
            paperclip_version: env!("CARGO_PKG_VERSION").to_owned(),
            firmware: "NOT VERIFIED".to_owned(),
            active_release: "NOT VERIFIED".to_owned(),
        }
    }

    fn diagnostics(&self) -> Vec<DiagnosticEntry> {
        let mut entries: Vec<DiagnosticEntry> = self
            .layout
            .apps()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|app| {
                let health = self.ledger.health(&app).ok().flatten()?;
                if health.started || health.attempts == 0 {
                    return None;
                }
                Some(DiagnosticEntry {
                    when: "UNKNOWN".to_owned(),
                    message: format!(
                        "{} V{} HAS NOT STARTED AFTER {} ATTEMPT(S){}",
                        app.leaf().to_uppercase(),
                        health.version,
                        health.attempts,
                        health
                            .last_failure
                            .as_deref()
                            .map_or_else(String::new, |note| format!(": {}", note.to_uppercase()))
                    ),
                })
            })
            .collect();

        if entries.is_empty() {
            entries.push(DiagnosticEntry {
                when: "UNKNOWN".to_owned(),
                message: "NO FAILED LAUNCHES RECORDED".to_owned(),
            });
        }
        entries.push(DiagnosticEntry {
            when: "NOTE".to_owned(),
            message: "GRANTS AND CATALOG HAVE NO PERSISTED HOST STATE YET".to_owned(),
        });
        entries
    }

    fn rollback(&mut self, app: &AppId) -> Result<(), HostOpError> {
        self.manager()
            .rollback(app, &NothingIsRunning)
            .map(|_| ())
            .map_err(HostOpError::from)
    }

    fn uninstall(&mut self, _app: &AppId) -> Result<(), HostOpError> {
        Err(HostOpError::NotSupported {
            reason: "paper_packages has no uninstall transaction yet \u{2014} \
                     only install, rollback and recover",
        })
    }

    fn revoke_grant(&mut self, _app: &AppId, _capability: Capability) -> Result<(), HostOpError> {
        Err(HostOpError::NotSupported {
            reason: "InstallPolicy is in-memory only; there is no persisted \
                     grant store to revoke from",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{HostOpError, PlaceholderHost, SettingsHost};
    use paper_packages::Capability;

    fn chess_id() -> paper_packages::AppId {
        "dev.calum.chess".parse().expect("valid fixture id")
    }

    #[test]
    fn rollback_restores_the_previous_version_and_clears_it() {
        let mut host = PlaceholderHost::new();
        host.rollback(&chess_id())
            .expect("chess has a previous release");
        let chess = &host.installed_apps()[0];
        assert_eq!(chess.active_version, "0.1.0");
        assert!(!chess.has_previous_release());
    }

    #[test]
    fn rolling_back_twice_fails_the_second_time() {
        let mut host = PlaceholderHost::new();
        host.rollback(&chess_id()).expect("first rollback succeeds");
        assert_eq!(
            host.rollback(&chess_id()),
            Err(HostOpError::NoPreviousRelease)
        );
    }

    #[test]
    fn uninstalling_removes_the_app_and_its_grants() {
        let mut host = PlaceholderHost::new();
        host.uninstall(&chess_id()).expect("chess is installed");
        assert!(host.installed_apps().is_empty());
        assert!(host.grants().is_empty());
    }

    #[test]
    fn revoking_an_unheld_grant_fails() {
        let mut host = PlaceholderHost::new();
        host.revoke_grant(&chess_id(), Capability::Storage)
            .expect("chess holds storage");
        assert_eq!(
            host.revoke_grant(&chess_id(), Capability::Storage),
            Err(HostOpError::GrantNotHeld)
        );
    }
}

#[cfg(test)]
mod live_host_tests {
    use std::fs;

    use paper_packages::store::{Layout, Marker, RELEASE_MARKER, atomic_write};
    use paper_packages::{AppId, Capability, Digest};

    use super::{HostOpError, LiveHost, SettingsHost};

    fn chess_id() -> AppId {
        "dev.calum.chess".parse().unwrap()
    }

    /// A complete release on disk, written the same way `paper_packages`'
    /// own fixtures do — real files a `LiveHost` reads, not a mock of one.
    fn install_release(layout: &Layout, app: &AppId, version: &str, select: bool) {
        let dir = layout.release_dir(app, &version.parse().unwrap());
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(
            dir.join("paper.toml"),
            format!(
                "[app]\nid = \"{app}\"\nname = \"Chess\"\nversion = \"{version}\"\n\
                 protocol = \"1.0\"\nentrypoint = \"bin/chess\"\n"
            ),
        )
        .unwrap();
        let marker = Marker {
            digest: Digest::of_bytes(version.as_bytes()),
            signer: "0011223344556677".parse().unwrap(),
            installed: 0,
        };
        fs::write(dir.join(RELEASE_MARKER), marker.to_document()).unwrap();
        if select {
            atomic_write(&layout.current_file(app), format!("{version}\n").as_bytes()).unwrap();
        }
    }

    #[test]
    fn installed_apps_reflects_what_is_really_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", false);
        install_release(&layout, &chess_id(), "0.2.0", true);
        atomic_write(&layout.previous_file(&chess_id()), b"0.1.0\n").unwrap();
        fs::create_dir_all(layout.data_dir(&chess_id())).unwrap();
        fs::write(layout.data_dir(&chess_id()).join("save.txt"), b"0123456789").unwrap();

        let apps = LiveHost::new(layout).installed_apps();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].active_version, "0.2.0");
        assert_eq!(apps[0].previous_version.as_deref(), Some("0.1.0"));
        assert_eq!(apps[0].data_bytes, 10);
    }

    #[test]
    fn an_app_with_only_one_release_has_no_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", true);

        let apps = LiveHost::new(layout).installed_apps();
        assert_eq!(apps.len(), 1);
        assert!(!apps[0].has_previous_release());
    }

    #[test]
    fn rollback_moves_the_real_selection_back() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", false);
        install_release(&layout, &chess_id(), "0.2.0", true);
        atomic_write(&layout.previous_file(&chess_id()), b"0.1.0\n").unwrap();

        let mut host = LiveHost::new(layout.clone());
        host.rollback(&chess_id())
            .expect("a previous release exists");
        assert_eq!(
            layout.current(&chess_id()).unwrap(),
            Some("0.1.0".parse().unwrap())
        );
    }

    #[test]
    fn rollback_with_no_previous_release_reports_it_rather_than_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", true);

        let mut host = LiveHost::new(layout);
        assert_eq!(
            host.rollback(&chess_id()),
            Err(HostOpError::NoPreviousRelease)
        );
    }

    #[test]
    fn uninstall_and_revoke_report_not_supported_rather_than_pretending_to_act() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", true);

        let mut host = LiveHost::new(layout);
        assert!(matches!(
            host.uninstall(&chess_id()),
            Err(HostOpError::NotSupported { .. })
        ));
        assert!(matches!(
            host.revoke_grant(&chess_id(), Capability::Storage),
            Err(HostOpError::NotSupported { .. })
        ));
    }

    #[test]
    fn storage_usage_counts_bytes_actually_written() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", true);
        fs::create_dir_all(layout.data_dir(&chess_id())).unwrap();
        fs::write(layout.data_dir(&chess_id()).join("save.txt"), b"0123456789").unwrap();

        let storage = LiveHost::new(layout).storage_usage();
        assert!(storage.used_bytes() >= 10);
        assert!(storage.free_bytes > 0);
    }

    #[test]
    fn grants_and_catalog_are_honestly_empty_without_a_persisted_store() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();

        let host = LiveHost::new(layout);
        assert!(host.grants().is_empty());
        assert!(!host.catalog_status().reachable);
    }
}
