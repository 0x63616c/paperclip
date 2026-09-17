//! The capability grant boundary.
//!
//! The rule this module exists to make unstateable-in-code: **an app cannot
//! grant itself anything.** That is not enforced by a review checklist or a
//! runtime `if`; it is enforced by there being no path from a file on disk to
//! a [`GrantedCapabilities`].
//!
//! - [`Manifest`](crate::Manifest) has no capability field, and rejects
//!   `capabilities` as a parse error.
//! - [`GrantedCapabilities`] implements neither `Deserialize` nor `Default`
//!   and has no public constructor.
//! - The only way to obtain one is [`InstallPolicy::grant`], which is fed by
//!   the host's own policy and never by package content.
//! - The only way to pair a manifest with capabilities is
//!   [`InstalledApp::install`], which requires a policy to be present.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::id::AppId;
use crate::manifest::Manifest;

/// Something an installed app may be permitted to do.
///
/// Small and closed on purpose. A capability that nothing in the platform
/// checks is a comment pretending to be a type, so this list grows only when
/// the host gains the enforcement point that goes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Capability {
    /// Read and write the app's own private storage directory.
    Storage,
    /// Reach the local network. Not granted by default, even on a personal device.
    Network,
    /// Take part in explicit cross-app sharing (§4): receive or offer content
    /// when the user initiates the exchange.
    Sharing,
    /// Install, update and roll back packages (§12).
    ///
    /// The App Store holds this and nothing else does. It is what makes the
    /// App Store a *client* of package management rather than its owner: the
    /// enforcement point is
    /// [`PackageManager::on_behalf_of`](crate::install::PackageManager::on_behalf_of),
    /// which refuses to hand an installer to an app that was not granted this.
    Packages,
}

impl Capability {
    /// Every capability the platform currently knows how to enforce.
    pub const ALL: [Capability; 4] = [
        Capability::Storage,
        Capability::Network,
        Capability::Sharing,
        Capability::Packages,
    ];

    /// The stable name used in host policy files and `paperctl` output.
    pub fn name(self) -> &'static str {
        match self {
            Capability::Storage => "storage",
            Capability::Network => "network",
            Capability::Sharing => "sharing",
            Capability::Packages => "packages",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What an installed app actually holds.
///
/// Deliberately not `Deserialize`, not `Default`, and with no public
/// constructor: a value of this type is evidence that a host policy decided
/// something, and nothing else can manufacture that evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedCapabilities {
    granted: BTreeSet<Capability>,
}

impl GrantedCapabilities {
    /// Whether this app holds `capability`.
    pub fn holds(&self, capability: Capability) -> bool {
        self.granted.contains(&capability)
    }

    /// Everything held, in a stable order.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.granted.iter().copied()
    }

    /// Whether nothing at all was granted.
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }
}

/// The host's decision about what each app may do.
///
/// Starts denying everything. On a personal single-device install this is
/// edited by hand and read by the host at install time; it is never shipped
/// inside a package, which is the whole point.
#[derive(Debug, Clone, Default)]
pub struct InstallPolicy {
    per_app: BTreeMap<AppId, BTreeSet<Capability>>,
}

impl InstallPolicy {
    /// A policy that grants nothing to anyone.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Grants `capability` to `app` when it is installed.
    pub fn allow(&mut self, app: &AppId, capability: Capability) -> &mut Self {
        self.per_app
            .entry(app.clone())
            .or_default()
            .insert(capability);
        self
    }

    /// Decides what `manifest`'s app holds.
    ///
    /// Takes the manifest so the decision is recorded against a specific app,
    /// and reads nothing from it but the id — there is no manifest content
    /// that can widen the answer.
    pub fn grant(&self, manifest: &Manifest) -> GrantedCapabilities {
        GrantedCapabilities {
            granted: self.per_app.get(manifest.id()).cloned().unwrap_or_default(),
        }
    }
}

/// A manifest paired with the capabilities the host decided it holds.
///
/// The only constructor requires a policy, so an `InstalledApp` cannot exist
/// without a host decision behind it.
#[derive(Debug, Clone)]
pub struct InstalledApp {
    manifest: Manifest,
    capabilities: GrantedCapabilities,
}

impl InstalledApp {
    /// Installs `manifest` under `policy`.
    pub fn install(manifest: Manifest, policy: &InstallPolicy) -> Self {
        let capabilities = policy.grant(&manifest);
        Self {
            manifest,
            capabilities,
        }
    }

    /// The manifest as parsed.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// What the host granted.
    pub fn capabilities(&self) -> &GrantedCapabilities {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::{Capability, InstallPolicy, InstalledApp};
    use crate::manifest::Manifest;

    fn manifest(id: &str) -> Manifest {
        Manifest::parse(&format!(
            r#"
            [app]
            id = "{id}"
            name = "Test"
            version = "0.1.0"
            protocol = "1.0"
            entrypoint = "bin/test"
            "#
        ))
        .expect("fixture manifest should be valid")
    }

    #[test]
    fn nothing_is_granted_by_default() {
        let chess = manifest("dev.calum.chess");
        let installed = InstalledApp::install(chess, &InstallPolicy::deny_all());
        assert!(installed.capabilities().is_empty());
        for capability in Capability::ALL {
            assert!(!installed.capabilities().holds(capability));
        }
    }

    #[test]
    fn policy_grants_are_per_app() {
        let chess = manifest("dev.calum.chess");
        let store = manifest("dev.calum.app-store");

        let mut policy = InstallPolicy::deny_all();
        policy.allow(store.id(), Capability::Network);

        let installed_chess = InstalledApp::install(chess, &policy);
        let installed_store = InstalledApp::install(store, &policy);

        assert!(!installed_chess.capabilities().holds(Capability::Network));
        assert!(installed_store.capabilities().holds(Capability::Network));
        assert!(!installed_store.capabilities().holds(Capability::Storage));
    }

    #[test]
    fn granted_capabilities_iterate_in_a_stable_order() {
        let chess = manifest("dev.calum.chess");
        let mut policy = InstallPolicy::deny_all();
        policy
            .allow(chess.id(), Capability::Sharing)
            .allow(chess.id(), Capability::Storage);

        let installed = InstalledApp::install(chess, &policy);
        let held: Vec<_> = installed.capabilities().iter().collect();
        assert_eq!(held, vec![Capability::Storage, Capability::Sharing]);
    }
}
