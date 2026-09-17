//! The two identity fields every app carries: a stable id and a human label.

use std::fmt;
use std::str::FromStr;

use crate::error::{IdError, NameError};

/// A stable, reverse-DNS app id such as `dev.calum.chess`.
///
/// Stable is the point: the id keys install state, saved games and catalog
/// entries, so it survives renames of the display name and is never derived
/// from one. The character set is deliberately narrow — lowercase ASCII,
/// digits and `-` — so an id is safe as a directory name, a systemd unit
/// fragment and a URL path segment without any escaping anywhere.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The label shown on the home shelf and in the App Store.
///
/// Free-form apart from three rules that exist so a shelf tile always has
/// something legible to draw: not blank, not enormous, no control characters.
/// Surrounding whitespace is trimmed on the way in rather than rejected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayName(String);

impl DisplayName {
    /// Longest permitted name, in characters.
    pub const MAX_LEN: usize = 48;

    /// The name as it should be drawn.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for DisplayName {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(NameError::Empty);
        }
        let len = trimmed.chars().count();
        if len > Self::MAX_LEN {
            return Err(NameError::TooLong {
                len,
                max: Self::MAX_LEN,
            });
        }
        if trimmed.chars().any(char::is_control) {
            return Err(NameError::ControlCharacter);
        }
        Ok(Self(trimmed.to_owned()))
    }
}

impl fmt::Display for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{AppId, DisplayName};
    use crate::error::{IdError, NameError};

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

    #[test]
    fn trims_display_names() {
        assert_eq!(
            "  Chess \n".parse::<DisplayName>().unwrap().as_str(),
            "Chess"
        );
    }

    #[test]
    fn rejects_blank_and_control_names() {
        assert_eq!("   ".parse::<DisplayName>(), Err(NameError::Empty));
        assert_eq!(
            "Che\u{7}ss".parse::<DisplayName>(),
            Err(NameError::ControlCharacter)
        );
        assert!(matches!(
            "n".repeat(DisplayName::MAX_LEN + 1).parse::<DisplayName>(),
            Err(NameError::TooLong { .. })
        ));
    }
}
