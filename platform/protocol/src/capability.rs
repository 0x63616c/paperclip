//! The capability vocabulary.
//!
//! Only the *names* live here, because both halves of the contract need to say
//! the same words: a host tells an app what it holds in [`Hello`](crate::Hello),
//! and `paper_packages::InstallPolicy` decides what that is at install time.
//!
//! The grant boundary — the fact that an app cannot award itself anything —
//! is enforced in `paper_packages`, not here. This enum is deliberately inert:
//! naming a capability is not holding one, and a [`Capability`] on the wire is
//! the host reporting a decision it already took.

use std::fmt;

/// Something an installed app may be permitted to do.
///
/// Small and closed on purpose. A capability that nothing in the platform
/// checks is a comment pretending to be a type, so this list grows only when
/// the host gains the enforcement point that goes with it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
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
    /// enforcement point is `paper_packages::install::PackageManager::on_behalf_of`,
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

    /// The stable name used in host policy files, on the wire and in
    /// `paperctl` output.
    pub const fn name(self) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::Capability;

    /// The wire spelling is the same string as the policy-file spelling. If
    /// these ever drift, a host policy granting `storage` would silently not
    /// be the `storage` an app is told it holds.
    #[test]
    fn wire_spelling_matches_the_policy_name() {
        for capability in Capability::ALL {
            let json = serde_json::to_string(&capability).unwrap();
            assert_eq!(json, format!("\"{}\"", capability.name()));
            let back: Capability = serde_json::from_str(&json).unwrap();
            assert_eq!(back, capability);
        }
    }
}
