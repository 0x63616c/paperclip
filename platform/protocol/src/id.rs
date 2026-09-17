//! The app id: who an app is, everywhere it is named.
//!
//! This lives in the protocol crate rather than the package crate because both
//! sides of the contract need it. A `paper.toml` declares one; a
//! [`Hello`](crate::Hello) **tells an app what its own id is**, which is the
//! only place an app ever learns it.
//!
//! That direction matters and is the whole reason this type is here. An app
//! never asserts its identity: there is no message carrying a caller-supplied
//! id, so there is nothing for the host to take on trust. Identity comes from
//! which connection the message arrived on, and the host already knows what it
//! launched on that connection.

use std::fmt;
use std::str::FromStr;

/// A stable, reverse-DNS app id such as `dev.calum.chess`.
///
/// Stable is the point: the id keys install state, saved games and catalog
/// entries, so it survives renames of the display name and is never derived
/// from one. The character set is deliberately narrow — lowercase ASCII,
/// digits and `-` — so an id is safe as a directory name, a systemd unit
/// fragment and a URL path segment without any escaping anywhere.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct AppId(String);

impl AppId {
    /// Longest permitted id, in bytes.
    pub const MAX_LEN: usize = 128;

    /// The id as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last segment — `chess` in `dev.calum.chess`.
    ///
    /// Useful for default file names. Never use it as an identity.
    pub fn leaf(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or(&self.0)
    }
}

impl FromStr for AppId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() > Self::MAX_LEN {
            return Err(IdError::TooLong {
                len: s.len(),
                max: Self::MAX_LEN,
            });
        }
        let segments: Vec<&str> = s.split('.').collect();
        if segments.len() < 2 {
            return Err(IdError::TooFewSegments);
        }
        for segment in segments {
            if segment.is_empty() {
                return Err(IdError::EmptySegment);
            }
            let mut chars = segment.chars();
            let first = chars.next().unwrap_or('\0');
            if !first.is_ascii_lowercase() {
                return Err(IdError::SegmentStart {
                    segment: segment.to_owned(),
                });
            }
            if !segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err(IdError::SegmentCharacter {
                    segment: segment.to_owned(),
                });
            }
            if segment.ends_with('-') {
                return Err(IdError::SegmentEnd {
                    segment: segment.to_owned(),
                });
            }
        }
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for AppId {
    type Error = IdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<AppId> for String {
    fn from(value: AppId) -> Self {
        value.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a string is not a valid [`AppId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    /// An app id must be at least `vendor.app`.
    #[error("needs at least two dot-separated segments, e.g. `dev.calum.chess`")]
    TooFewSegments,
    /// An empty segment, from a leading, trailing or doubled dot.
    #[error("has an empty segment")]
    EmptySegment,
    /// A segment started with something other than an ASCII lowercase letter.
    #[error("segment `{segment}` must start with a lowercase letter")]
    SegmentStart {
        /// The offending segment.
        segment: String,
    },
    /// A segment contained a character outside `[a-z0-9-]`.
    #[error("segment `{segment}` may only contain lowercase letters, digits and `-`")]
    SegmentCharacter {
        /// The offending segment.
        segment: String,
    },
    /// A segment ended with `-`.
    #[error("segment `{segment}` may not end with `-`")]
    SegmentEnd {
        /// The offending segment.
        segment: String,
    },
    /// The whole id exceeded [`AppId::MAX_LEN`].
    #[error("is {len} characters, over the {max} character limit")]
    TooLong {
        /// Actual length.
        len: usize,
        /// Permitted length.
        max: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::{AppId, IdError};

    #[test]
    fn accepts_reverse_dns_ids() {
        let id: AppId = "dev.calum.chess".parse().unwrap();
        assert_eq!(id.as_str(), "dev.calum.chess");
        assert_eq!(id.leaf(), "chess");
        assert!("app-store.local.v2".parse::<AppId>().is_ok());
    }

    #[test]
    fn rejects_single_segment_ids() {
        assert_eq!("chess".parse::<AppId>(), Err(IdError::TooFewSegments));
    }

    #[test]
    fn rejects_empty_segments() {
        assert_eq!("dev..chess".parse::<AppId>(), Err(IdError::EmptySegment));
        assert_eq!(".chess".parse::<AppId>(), Err(IdError::EmptySegment));
        assert_eq!("dev.chess.".parse::<AppId>(), Err(IdError::EmptySegment));
    }

    #[test]
    fn rejects_uppercase_and_punctuation() {
        assert!(matches!(
            "dev.Calum.chess".parse::<AppId>(),
            Err(IdError::SegmentStart { .. })
        ));
        assert!(matches!(
            "dev.calum_webb.chess".parse::<AppId>(),
            Err(IdError::SegmentCharacter { .. })
        ));
        assert!(matches!(
            "dev.calum-.chess".parse::<AppId>(),
            Err(IdError::SegmentEnd { .. })
        ));
    }

    #[test]
    fn rejects_overlong_ids() {
        let long = format!("dev.{}", "a".repeat(AppId::MAX_LEN));
        assert!(matches!(
            long.parse::<AppId>(),
            Err(IdError::TooLong { .. })
        ));
    }

    /// An id arriving over the wire goes through the same parser as one from a
    /// `paper.toml`. Without `try_from` on the deserialiser, a host that
    /// forwarded an id from a catalog would be able to mint `../../etc` as an
    /// `AppId` and hand it to something that uses ids as directory names.
    #[test]
    fn deserialising_an_id_validates_it() {
        let good: AppId = serde_json::from_str("\"dev.calum.chess\"").unwrap();
        assert_eq!(good.as_str(), "dev.calum.chess");
        assert!(serde_json::from_str::<AppId>("\"../../etc\"").is_err());
        assert!(serde_json::from_str::<AppId>("\"Chess\"").is_err());
    }
}
