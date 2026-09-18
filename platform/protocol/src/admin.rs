//! Settings' admin surface, carried over [`crate::system::SystemQuery`] and
//! [`crate::system::SystemAnswer`] as one more query kind (WWW-71, ADR-0028).
//!
//! Before this module, the nine operations here (`apps/settings/src/host.rs`'s
//! `SettingsHost` trait) were served by `LiveHost`, which constructed
//! `paper_packages::install::PackageManager` inside Settings' own process —
//! the sandbox violation WWW-50 named and deferred. They now travel the same
//! wire the no-grant tier does, answered host-side by whichever process is
//! actually running Settings' connection (`tools/paperctl/src/admin.rs`
//! today), and gated by checking that connection's [`AppId`](crate::AppId)
//! against `dev.calum.settings` — not a [`Capability`](crate::Capability), so
//! `GrantedCapabilities`'s closed set (no `Deserialize`, no `Default`, no
//! public constructor) is untouched.
//!
//! Unlike the no-grant tier, every read here can be asked for on its own
//! ([`AdminQuery`]) and every write answers with its own
//! [`Result`] rather than a bare value: a rollback that failed because there
//! was nothing to roll back to is not the same shape of failure as a battery
//! reading nobody could take, and folding the two into one
//! [`SystemDenial`](crate::system::SystemDenial) would have lost that
//! distinction.

use crate::capability::Capability;
use crate::id::AppId;

/// One row on the installed-apps page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledAppSummary {
    /// The app's stable id, used to address it in a further [`AdminQuery`].
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageBucket {
    /// What the bucket is, e.g. `"DATA / CHESS"` or `"STAGING"`.
    pub label: String,
    /// Bytes held.
    pub bytes: u64,
}

/// One capability grant, and whether revoking it would land on an app while
/// it is actually relying on it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CatalogStatus {
    /// The configured catalog endpoint.
    pub endpoint: String,
    /// When the endpoint last answered successfully, already formatted —
    /// nothing on this wire depends on a clock or a time-formatting crate.
    pub last_fetch: Option<String>,
    /// Whether the endpoint answered the most recent attempt.
    pub reachable: bool,
}

/// One entry in the diagnostics log, read-only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticEntry {
    /// When it happened, already formatted.
    pub when: String,
    /// What was logged.
    pub message: String,
}

/// One of Settings' nine admin operations.
///
/// The six read-only variants each ask for one page's worth of data rather
/// than everything at once: a client that only wants a fresh grants list
/// after a revoke should not have to pay for five answers it will discard.
/// The three write variants name what they act on rather than repeating a
/// whole record, the same way [`AdminQuery::Rollback`] only needs an
/// [`AppId`] — a Host transaction re-reads whatever it needs to act.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AdminQuery {
    /// Every installed app, in display order.
    InstalledApps,
    /// What storage holds today.
    StorageUsage,
    /// Every capability grant in force.
    Grants,
    /// The catalog's configured endpoint and reachability.
    CatalogStatus,
    /// Platform version facts.
    PlatformInfo,
    /// Recent platform errors, most recent first.
    Diagnostics,
    /// Rolls `app` back to the release rollback would restore.
    Rollback {
        /// Which app.
        app: AppId,
    },
    /// Uninstalls `app`, including its data.
    Uninstall {
        /// Which app.
        app: AppId,
    },
    /// Revokes `capability` from `app`.
    RevokeGrant {
        /// Which app.
        app: AppId,
        /// What to revoke.
        capability: Capability,
    },
}

/// Why one of the three write [`AdminQuery`] variants did not go through.
///
/// A closed, machine-readable set rather than a string, for the same reason
/// [`SystemDenial`](crate::system::SystemDenial) is: a caller that skipped
/// the client-side "does this app have a previous release" check gets told
/// why, instead of a rollback that looked like it worked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AdminError {
    /// Rollback was requested for an app with no previous release.
    NoPreviousRelease,
    /// The app id named in the request is not installed.
    NotInstalled,
    /// The grant named in the request is not held.
    GrantNotHeld,
    /// The host answering this connection has no transaction for the request
    /// yet.
    NotSupported {
        /// Why not, so a caller can say something more useful than "no".
        detail: String,
    },
    /// The underlying Host operation failed for a reason outside this
    /// error's small, closed vocabulary.
    Failed(String),
}

/// The value one [`AdminQuery`] resolved to.
///
/// The three write variants carry a `Result` rather than being answered
/// through [`SystemDenial`](crate::system::SystemDenial): a
/// [`SystemDenial`](crate::system::SystemDenial) says the *question* was
/// refused (wrong caller, asking too fast), while an [`AdminError`] says the
/// *transaction* the question named did not go through. Folding the two
/// together would have lost that distinction the way it matters most — a
/// destructive confirmation reporting why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AdminValue {
    /// Answers [`AdminQuery::InstalledApps`].
    InstalledApps(Vec<InstalledAppSummary>),
    /// Answers [`AdminQuery::StorageUsage`].
    StorageUsage(StorageUsage),
    /// Answers [`AdminQuery::Grants`].
    Grants(Vec<GrantSummary>),
    /// Answers [`AdminQuery::CatalogStatus`].
    CatalogStatus(CatalogStatus),
    /// Answers [`AdminQuery::PlatformInfo`].
    PlatformInfo(crate::system::PlatformFact),
    /// Answers [`AdminQuery::Diagnostics`].
    Diagnostics(Vec<DiagnosticEntry>),
    /// Answers [`AdminQuery::Rollback`].
    Rollback(Result<(), AdminError>),
    /// Answers [`AdminQuery::Uninstall`].
    Uninstall(Result<(), AdminError>),
    /// Answers [`AdminQuery::RevokeGrant`].
    RevokeGrant(Result<(), AdminError>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_query_round_trips_through_its_wire_form() {
        let query = AdminQuery::Rollback {
            app: "dev.calum.chess".parse().unwrap(),
        };
        let json = serde_json::to_string(&query).unwrap();
        assert_eq!(serde_json::from_str::<AdminQuery>(&json).unwrap(), query);
    }

    #[test]
    fn a_failed_write_carries_why_rather_than_a_bare_no() {
        let value = AdminValue::Rollback(Err(AdminError::NoPreviousRelease));
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("no-previous-release"), "{json}");
        assert_eq!(serde_json::from_str::<AdminValue>(&json).unwrap(), value);
    }

    #[test]
    fn a_successful_write_round_trips_as_an_empty_ok() {
        let value = AdminValue::Uninstall(Ok(()));
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<AdminValue>(&json).unwrap(), value);
    }
}
