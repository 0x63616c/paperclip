//! The boundary between Settings and Host-owned operations.
//!
//! Settings is a **client** of the platform, never an owner of it (§6, §11):
//! rollback, uninstall and grant revocation are Host transactions, and this
//! crate must not reach into the storage layout or the install policy to
//! perform them itself. [`SettingsHost`] is that boundary, named so it is the
//! one place that changes when WWW-7 lands a real Host client — everything
//! above it (`screen`, `pages`, `confirm`) is written against the trait, not
//! against [`PlaceholderHost`].
//!
//! [`PlaceholderHost`] is fixture data for the desktop preview and is not
//! wired to anything real. It exists so every page has something honest to
//! draw before WWW-7 exists, in the same spirit as `tools/paperctl`'s
//! compiled-in manifests.

use paper_packages::{AppId, Capability};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOpError {
    /// Rollback was requested for an app with no previous release.
    NoPreviousRelease,
    /// The app id named in the request is not installed.
    NotInstalled,
    /// The grant named in the request is not held.
    GrantNotHeld,
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

/// Fixture data for the desktop preview.
///
/// Not device evidence and not a Host implementation: every value here is
/// invented so the pages have something legible to draw. Swapping this for a
/// real Host client is the only change WWW-7 landing should require above
/// this module.
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
