//! Package-relative paths that cannot point outside the package.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::error::PathError;

/// A path inside a package directory, checked to stay there.
///
/// The check happens once, at parse time, so every later consumer — the
/// installer, the updater, the host that spawns the entrypoint — can join it
/// onto a package root without re-deriving whether that is safe. `..`,
/// absolute paths, backslashes, `~` and NUL are all rejected rather than
/// normalised away: silently rewriting a path an author wrote means shipping
/// something they did not ask for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath(String);

impl RelativePath {
    /// Longest permitted path, in bytes.
    pub const MAX_LEN: usize = 512;

    /// The path as written, always `/`-separated.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// This path resolved against a package root.
    ///
    /// Safe by construction: the value cannot contain `..` or a root, so the
    /// result is always inside `root`.
    pub fn resolve_within(&self, root: &Path) -> PathBuf {
        let mut resolved = root.to_path_buf();
        for component in self.0.split('/') {
            resolved.push(component);
        }
        resolved
    }
}

impl FromStr for RelativePath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(PathError::Empty);
        }
        if s.len() > Self::MAX_LEN {
            return Err(PathError::TooLong {
                len: s.len(),
                max: Self::MAX_LEN,
            });
        }
        if s.contains('\0') {
            return Err(PathError::Nul);
        }
        if s.contains('\\') || s.as_bytes().get(1) == Some(&b':') {
            return Err(PathError::NotPosix);
        }
        if s.starts_with('/') {
            return Err(PathError::Absolute);
        }
        if s.starts_with('~') {
            return Err(PathError::HomeExpansion);
        }
        for component in s.split('/') {
            match component {
                "" | "." => return Err(PathError::EmptyComponent),
                ".." => return Err(PathError::ParentEscape),
                _ => {}
            }
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::RelativePath;
    use crate::error::PathError;

    fn parse(s: &str) -> Result<RelativePath, PathError> {
        s.parse()
    }

    #[test]
    fn accepts_ordinary_package_paths() {
        assert_eq!(parse("bin/chess").unwrap().as_str(), "bin/chess");
        assert_eq!(parse("icon.png").unwrap().as_str(), "icon.png");
        assert_eq!(
            parse("assets/pieces/king.svg").unwrap().as_str(),
            "assets/pieces/king.svg"
        );
    }

    #[test]
    fn resolves_inside_the_package_root() {
        let root = Path::new("/var/lib/paperclip/apps/dev.calum.chess");
        assert_eq!(
            parse("bin/chess").unwrap().resolve_within(root),
            root.join("bin").join("chess")
        );
    }

    #[test]
    fn rejects_absolute_paths() {
        assert_eq!(parse("/bin/sh"), Err(PathError::Absolute));
        assert_eq!(parse("/etc/passwd"), Err(PathError::Absolute));
    }

    #[test]
    fn rejects_parent_escapes() {
        assert_eq!(parse("../../etc/passwd"), Err(PathError::ParentEscape));
        assert_eq!(
            parse("bin/../../../etc/shadow"),
            Err(PathError::ParentEscape)
        );
        assert_eq!(parse(".."), Err(PathError::ParentEscape));
    }

    #[test]
    fn rejects_home_expansion_and_windows_shapes() {
        assert_eq!(parse("~/.ssh/id_ed25519"), Err(PathError::HomeExpansion));
        assert_eq!(parse("bin\\chess.exe"), Err(PathError::NotPosix));
        assert_eq!(parse("C:/windows/system32"), Err(PathError::NotPosix));
    }

    #[test]
    fn rejects_empty_and_dot_components() {
        assert_eq!(parse(""), Err(PathError::Empty));
        assert_eq!(parse("bin//chess"), Err(PathError::EmptyComponent));
        assert_eq!(parse("./bin/chess"), Err(PathError::EmptyComponent));
        assert_eq!(parse("bin/chess/"), Err(PathError::EmptyComponent));
    }

    #[test]
    fn rejects_nul_and_overlong_paths() {
        assert_eq!(parse("bin/ch\0ess"), Err(PathError::Nul));
        assert!(matches!(
            parse(&"a".repeat(RelativePath::MAX_LEN + 1)),
            Err(PathError::TooLong { .. })
        ));
    }
}
