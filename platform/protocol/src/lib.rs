//! Version identifiers for the contract between the platform host and the apps
//! it runs.
//!
//! This crate deliberately contains no wire types yet. The app lifecycle,
//! input and rendering messages are Stage 6 work (§18 item 5); what Stage 1
//! needs is a stable way for a `paper.toml` manifest to say which contract it
//! was built against, and a stable rule for deciding whether the host can
//! honour it.

use std::fmt;
use std::str::FromStr;

/// The app protocol version this build of the platform speaks.
///
/// Stays at `1.0` until the first message is actually defined; bumping it
/// before there is a wire format to bump would be theatre.
pub const CURRENT: ProtocolVersion = ProtocolVersion::new(1, 0);

/// A `major.minor` version of the platform/app protocol.
///
/// Patch numbers are intentionally absent: a protocol either changed shape or
/// it did not, and a version that cannot change behaviour is not worth
/// carrying in a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
    major: u16,
    minor: u16,
}

impl ProtocolVersion {
    /// Builds a version from its parts.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Breaking-change counter. Different major means incompatible, full stop.
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Additive-change counter within a major.
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Whether a host speaking `self` can run an app built against `app`.
    ///
    /// The rule is one-directional on purpose: a host may be newer than the
    /// app it runs, never older. An app that needs `1.3` will not start on a
    /// `1.1` host, because the messages it expects do not exist there.
    pub const fn can_run(self, app: ProtocolVersion) -> bool {
        self.major == app.major && self.minor >= app.minor
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Why a `major.minor` string could not be read as a [`ProtocolVersion`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The string was not exactly two dot-separated parts.
    #[error("expected `major.minor`, got `{0}`")]
    Shape(String),
    /// A part was not a number that fits in `u16`.
    #[error("`{part}` in `{input}` is not a version number")]
    NotANumber {
        /// The offending part, as written.
        part: String,
        /// The whole string it came from.
        input: String,
    },
}

impl FromStr for ProtocolVersion {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (major, minor) = s
            .split_once('.')
            .ok_or_else(|| ParseError::Shape(s.to_owned()))?;
        if minor.contains('.') {
            return Err(ParseError::Shape(s.to_owned()));
        }
        let parse = |part: &str| {
            part.parse::<u16>().map_err(|_| ParseError::NotANumber {
                part: part.to_owned(),
                input: s.to_owned(),
            })
        };
        Ok(Self::new(parse(major)?, parse(minor)?))
    }
}

impl serde::Serialize for ProtocolVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{CURRENT, ParseError, ProtocolVersion};

    #[test]
    fn parses_major_minor() {
        assert_eq!(
            "2.7".parse::<ProtocolVersion>().unwrap(),
            ProtocolVersion::new(2, 7)
        );
    }

    #[test]
    fn rejects_semver_shaped_input() {
        assert!(matches!(
            "1.0.0".parse::<ProtocolVersion>(),
            Err(ParseError::Shape(_))
        ));
        assert!(matches!(
            "1".parse::<ProtocolVersion>(),
            Err(ParseError::Shape(_))
        ));
    }

    #[test]
    fn rejects_non_numeric_parts() {
        assert!(matches!(
            "1.x".parse::<ProtocolVersion>(),
            Err(ParseError::NotANumber { .. })
        ));
    }

    #[test]
    fn host_runs_equal_or_older_minor_only() {
        let host = ProtocolVersion::new(1, 2);
        assert!(host.can_run(ProtocolVersion::new(1, 0)));
        assert!(host.can_run(ProtocolVersion::new(1, 2)));
        assert!(!host.can_run(ProtocolVersion::new(1, 3)));
        assert!(!host.can_run(ProtocolVersion::new(2, 0)));
        assert!(!host.can_run(ProtocolVersion::new(0, 9)));
    }

    #[test]
    fn current_round_trips_through_its_own_text_form() {
        assert_eq!(
            CURRENT.to_string().parse::<ProtocolVersion>().unwrap(),
            CURRENT
        );
    }
}
