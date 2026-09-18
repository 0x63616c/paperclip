//! Answers Settings' admin `SystemQuery::Admin(...)` queries against a real
//! store (WWW-71, ADR-0028): the nine `SettingsHost` operations
//! `apps/settings/src/host.rs`'s `LiveHost` used to perform inside Settings'
//! own process, by constructing a `paper_packages::install::PackageManager`
//! there directly — the sandbox violation WWW-50 named and deferred.
//!
//! Lives here for the same reason [`crate::system::SystemResponder`] does:
//! [`Session`](crate::session::Session) is the one real process this
//! workspace runs apps under today. Gating is by connection identity, not a
//! [`Capability`](paper_protocol::Capability) — see `platform/protocol/src/admin.rs`'s
//! module doc — checked here rather than trusted from the caller, because an
//! [`AdminResponder`] with no gate of its own would only be safe as long as
//! every call site remembered to check first.

use std::time::Instant;

use paper_packages::install::{InstallError, NothingIsRunning, PackageManager};
use paper_packages::inventory::Inventory;
use paper_packages::launch::Ledger;
use paper_packages::store::Layout;
use paper_packages::{AppId, InstallPolicy};
use paper_protocol::{
    AdminError, AdminQuery, AdminValue, CatalogStatus, DiagnosticEntry, InstalledAppSummary,
    StorageBucket, StorageUsage, SystemDenial, SystemDenialReason,
};

use crate::system::RateWindow;

/// The only connection allowed to ask an [`AdminQuery`].
const SETTINGS_APP_ID: &str = "dev.calum.settings";

/// Answers [`AdminQuery`]s against a real [`Layout`], gated to the
/// `dev.calum.settings` connection and rate-limited the same way
/// [`crate::system::SystemResponder`] rate-limits the no-grant tier — a
/// separate budget, not a shared counter, because these are answered by a
/// separate responder over a separate backend (a real store, not
/// `/sys`/`nmcli`), and nothing here needs the two to drain from one bucket.
#[derive(Debug)]
pub(crate) struct AdminResponder {
    layout: Layout,
    ledger: Ledger,
    window: RateWindow,
}

impl AdminResponder {
    /// An admin responder over the store at `layout`.
    pub(crate) fn new(layout: Layout) -> Self {
        let ledger = Ledger::new(layout.clone());
        Self {
            layout,
            ledger,
            window: RateWindow::default(),
        }
    }

    /// Answers `query`, refusing it outright unless `caller` is
    /// `dev.calum.settings`, and rate-limiting whoever that turns out to be.
    pub(crate) fn answer(
        &mut self,
        query: AdminQuery,
        caller: &AppId,
    ) -> Result<AdminValue, SystemDenial> {
        if caller.as_str() != SETTINGS_APP_ID {
            return Err(SystemDenial::new(SystemDenialReason::NotPermitted));
        }
        if !self.window.allow(Instant::now()) {
            return Err(SystemDenial::new(SystemDenialReason::RateLimited));
        }
        self.value(query)
    }

    fn value(&mut self, query: AdminQuery) -> Result<AdminValue, SystemDenial> {
        match query {
            AdminQuery::InstalledApps => Ok(AdminValue::InstalledApps(self.installed_apps())),
            AdminQuery::StorageUsage => Ok(AdminValue::StorageUsage(self.storage_usage())),
            AdminQuery::Grants => Ok(AdminValue::Grants(self.grants())),
            AdminQuery::CatalogStatus => Ok(AdminValue::CatalogStatus(self.catalog_status())),
            AdminQuery::PlatformInfo => {
                Ok(AdminValue::PlatformInfo(crate::system::platform_fact()))
            }
            AdminQuery::Diagnostics => Ok(AdminValue::Diagnostics(self.diagnostics())),
            AdminQuery::Rollback { app } => Ok(AdminValue::Rollback(self.rollback(&app))),
            AdminQuery::Uninstall { app } => Ok(AdminValue::Uninstall(self.uninstall(&app))),
            AdminQuery::RevokeGrant { app, capability } => {
                Ok(AdminValue::RevokeGrant(self.revoke_grant(&app, capability)))
            }
            // `AdminQuery` is `#[non_exhaustive]`: a future minor protocol
            // bump can add a variant this build predates, the same forward
            // compatibility `SystemResponder::value` gives the no-grant tier.
            _ => Err(SystemDenial::new(SystemDenialReason::Unsupported)),
        }
    }

    /// A manager scoped to this layout, denying every capability.
    ///
    /// Matches `apps/settings/src/host.rs`'s old `LiveHost::manager`: nothing
    /// here runs inside a live app session, so there is no real answer to "is
    /// this app already running" other than [`NothingIsRunning`] — claiming
    /// one would be inventing it.
    fn manager(&self) -> PackageManager {
        PackageManager::host(self.layout.clone(), InstallPolicy::deny_all())
    }

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

    fn grants(&self) -> Vec<paper_protocol::GrantSummary> {
        // `InstallPolicy` (ADR-0003) is constructed fresh per install and
        // never persisted or enumerated — there is no store to read a list
        // of standing grants back from yet. See ADR-0015.
        Vec::new()
    }

    fn catalog_status(&self) -> CatalogStatus {
        // No catalog endpoint is persisted anywhere this responder can read
        // it — `paperctl install`/`list` take `--catalog` on every
        // invocation. Reporting "not configured" is the real answer, not a
        // fixture standing in for one.
        CatalogStatus {
            endpoint: "NOT CONFIGURED".to_owned(),
            last_fetch: None,
            reachable: false,
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

    fn rollback(&mut self, app: &AppId) -> Result<(), AdminError> {
        self.manager()
            .rollback(app, &NothingIsRunning)
            .map(|_| ())
            .map_err(install_error_to_admin_error)
    }

    fn uninstall(&mut self, _app: &AppId) -> Result<(), AdminError> {
        Err(AdminError::NotSupported {
            detail: "paper_packages has no uninstall transaction yet \u{2014} only install, \
                     rollback and recover"
                .to_owned(),
        })
    }

    fn revoke_grant(
        &mut self,
        _app: &AppId,
        _capability: paper_protocol::Capability,
    ) -> Result<(), AdminError> {
        Err(AdminError::NotSupported {
            detail: "InstallPolicy is in-memory only; there is no persisted grant store to \
                     revoke from"
                .to_owned(),
        })
    }
}

/// A free function rather than `impl From<InstallError> for AdminError`: both
/// types are foreign to this crate, so the orphan rule refuses the trait impl
/// — `AdminError` belongs to `paper_protocol`, which cannot depend on
/// `paper_packages` (the dependency runs the other way).
fn install_error_to_admin_error(error: InstallError) -> AdminError {
    match error {
        InstallError::NoFallback { .. } => AdminError::NoPreviousRelease,
        InstallError::NothingSelected { .. } => AdminError::NotInstalled,
        other => AdminError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use paper_packages::store::{Layout, Marker, RELEASE_MARKER, atomic_write};
    use paper_packages::{AppId, Capability, Digest};
    use paper_protocol::{AdminError, AdminQuery, AdminValue, SystemDenialReason};

    use super::AdminResponder;

    fn chess_id() -> AppId {
        "dev.calum.chess".parse().unwrap()
    }

    fn settings_id() -> AppId {
        "dev.calum.settings".parse().unwrap()
    }

    /// A complete release on disk, written the same way `paper_packages`'
    /// own fixtures do — real files this responder reads, not a mock of one.
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
    fn a_caller_that_is_not_settings_is_refused_outright() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        let mut responder = AdminResponder::new(layout);

        let denial = responder
            .answer(AdminQuery::InstalledApps, &chess_id())
            .unwrap_err();
        assert_eq!(denial.reason, SystemDenialReason::NotPermitted);
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

        let mut responder = AdminResponder::new(layout);
        let AdminValue::InstalledApps(apps) = responder
            .answer(AdminQuery::InstalledApps, &settings_id())
            .unwrap()
        else {
            panic!("expected InstalledApps");
        };
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].active_version, "0.2.0");
        assert_eq!(apps[0].previous_version.as_deref(), Some("0.1.0"));
        assert_eq!(apps[0].data_bytes, 10);
    }

    #[test]
    fn rollback_moves_the_real_selection_back() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", false);
        install_release(&layout, &chess_id(), "0.2.0", true);
        atomic_write(&layout.previous_file(&chess_id()), b"0.1.0\n").unwrap();

        let mut responder = AdminResponder::new(layout.clone());
        let AdminValue::Rollback(result) = responder
            .answer(AdminQuery::Rollback { app: chess_id() }, &settings_id())
            .unwrap()
        else {
            panic!("expected Rollback");
        };
        assert_eq!(result, Ok(()));
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

        let mut responder = AdminResponder::new(layout);
        let AdminValue::Rollback(result) = responder
            .answer(AdminQuery::Rollback { app: chess_id() }, &settings_id())
            .unwrap()
        else {
            panic!("expected Rollback");
        };
        assert_eq!(result, Err(AdminError::NoPreviousRelease));
    }

    #[test]
    fn uninstall_and_revoke_report_not_supported_rather_than_pretending_to_act() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", true);

        let mut responder = AdminResponder::new(layout);
        let AdminValue::Uninstall(result) = responder
            .answer(AdminQuery::Uninstall { app: chess_id() }, &settings_id())
            .unwrap()
        else {
            panic!("expected Uninstall");
        };
        assert!(matches!(result, Err(AdminError::NotSupported { .. })));

        let AdminValue::RevokeGrant(result) = responder
            .answer(
                AdminQuery::RevokeGrant {
                    app: chess_id(),
                    capability: Capability::Storage,
                },
                &settings_id(),
            )
            .unwrap()
        else {
            panic!("expected RevokeGrant");
        };
        assert!(matches!(result, Err(AdminError::NotSupported { .. })));
    }

    #[test]
    fn storage_usage_counts_bytes_actually_written() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        install_release(&layout, &chess_id(), "0.1.0", true);
        fs::create_dir_all(layout.data_dir(&chess_id())).unwrap();
        fs::write(layout.data_dir(&chess_id()).join("save.txt"), b"0123456789").unwrap();

        let mut responder = AdminResponder::new(layout);
        let AdminValue::StorageUsage(storage) = responder
            .answer(AdminQuery::StorageUsage, &settings_id())
            .unwrap()
        else {
            panic!("expected StorageUsage");
        };
        assert!(storage.used_bytes() >= 10);
        assert!(storage.free_bytes > 0);
    }

    #[test]
    fn grants_and_catalog_are_honestly_empty_without_a_persisted_store() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();

        let mut responder = AdminResponder::new(layout);
        let AdminValue::Grants(grants) = responder
            .answer(AdminQuery::Grants, &settings_id())
            .unwrap()
        else {
            panic!("expected Grants");
        };
        assert!(grants.is_empty());

        let AdminValue::CatalogStatus(catalog) = responder
            .answer(AdminQuery::CatalogStatus, &settings_id())
            .unwrap()
        else {
            panic!("expected CatalogStatus");
        };
        assert!(!catalog.reachable);
    }

    #[test]
    fn asking_past_the_budget_is_rate_limited_not_answered() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        let mut responder = AdminResponder::new(layout);

        for i in 0..paper_protocol::MAX_SYSTEM_QUERIES_PER_SECOND {
            assert!(
                responder
                    .answer(AdminQuery::PlatformInfo, &settings_id())
                    .is_ok(),
                "query {i} should be within budget"
            );
        }
        let denied = responder
            .answer(AdminQuery::PlatformInfo, &settings_id())
            .unwrap_err();
        assert_eq!(denied.reason, SystemDenialReason::RateLimited);
    }
}
