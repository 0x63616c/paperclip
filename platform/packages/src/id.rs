//! The human label an app carries, beside the [`AppId`] it is keyed by.
//!
//! The id itself lives in `paper_protocol` and is re-exported here: it is on
//! the wire as well as in a manifest, and a [`Hello`](paper_protocol::Hello)
//! is where an app learns its own. A display name is manifest-only — nothing
//! over the wire needs it — so it stays here.

use std::fmt;
use std::str::FromStr;

pub use paper_protocol::AppId;

use crate::error::NameError;

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
    use super::DisplayName;
    use crate::error::NameError;

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
