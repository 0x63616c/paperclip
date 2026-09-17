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

    /// A drawable name from text that may not be one, never failing.
    ///
    /// For the one situation where refusing is not an option: a shelf or an
    /// App Store row has to put *something* legible where the name goes, and
    /// a blank tile is worse than a truncated one. Control characters go,
    /// surrounding whitespace goes, anything over [`Self::MAX_LEN`] is
    /// truncated on a character boundary, and text that had nothing usable in
    /// it becomes a visible placeholder rather than an empty string.
    ///
    /// Not a way around validation. `FromStr` is still what a manifest goes
    /// through, and it still rejects; this is only for text the platform is
    /// deriving for itself.
    pub fn from_lossy(text: &str) -> Self {
        let cleaned: String = text
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .trim()
            .chars()
            .take(Self::MAX_LEN)
            .collect();
        if cleaned.is_empty() {
            Self("Unnamed app".to_owned())
        } else {
            Self(cleaned)
        }
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
    fn a_lossy_name_is_always_drawable() {
        assert_eq!(DisplayName::from_lossy("Chess").as_str(), "Chess");
        assert_eq!(DisplayName::from_lossy("  Chess \n").as_str(), "Chess");
        assert_eq!(DisplayName::from_lossy("Che\u{7}ss").as_str(), "Chess");
        assert_eq!(DisplayName::from_lossy("").as_str(), "Unnamed app");
        assert_eq!(
            DisplayName::from_lossy("\u{7}\u{7}").as_str(),
            "Unnamed app"
        );

        let long = DisplayName::from_lossy(&"n".repeat(DisplayName::MAX_LEN * 2));
        assert_eq!(long.as_str().chars().count(), DisplayName::MAX_LEN);
        // Whatever it produces must itself be a valid name.
        assert!(long.as_str().parse::<DisplayName>().is_ok());
    }

    #[test]
    fn a_lossy_name_survives_multi_byte_truncation() {
        let emoji = DisplayName::from_lossy(&"\u{1f600}".repeat(DisplayName::MAX_LEN * 2));
        assert_eq!(emoji.as_str().chars().count(), DisplayName::MAX_LEN);
        assert!(emoji.as_str().parse::<DisplayName>().is_ok());
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
