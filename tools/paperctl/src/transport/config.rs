//! The one file the Mac-side transport writes: a pinned device and the
//! last-good host, per the resolution order in WWW-33.
//!
//! `~/.config/paperctl/config.toml` (or `$PAPERCTL_CONFIG_DIR/config.toml`,
//! or `$XDG_CONFIG_HOME/paperctl/config.toml`), documented in
//! `docs/development.md`. TOML, not because the format matters, but because
//! `toml` is already a workspace dependency and a person can read it.

use std::path::{Path, PathBuf};

use paper_packages::store::{StoreError, atomic_write};

/// The config file's contents. `#[serde(default)]` on every field: a file
/// with only `pinned =` in it, written by an older `paperctl`, is still
/// readable rather than a parse error.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct ConfigFile {
    #[serde(default)]
    pinned: Option<String>,
    #[serde(default)]
    cache: Option<String>,
}

/// The config, loaded from (or defaulted for) one path.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    path: PathBuf,
    file: ConfigFile,
}

/// Why the config could not be read, written or trusted.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// A pin or cache write was asked for a host that is not one.
    #[error("`{host}` is not a usable device host: {reason}")]
    InvalidHost {
        /// What was typed.
        host: String,
        /// Why it was refused.
        reason: &'static str,
    },
    /// The file exists but could not be read.
    #[error("cannot read {path}")]
    Read {
        /// Which file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The file exists but is not valid TOML, or not the shape expected.
    #[error("{path} is not a valid paperctl config file")]
    Parse {
        /// Which file.
        path: PathBuf,
        /// Why.
        #[source]
        source: toml::de::Error,
    },
    /// The file could not be encoded. Only reachable if a future field holds
    /// something TOML cannot represent; every field here is a plain string.
    #[error("cannot encode the paperctl config")]
    Encode(#[from] toml::ser::Error),
    /// The file could not be written.
    #[error("cannot write the paperctl config")]
    Write(#[from] StoreError),
}

/// Where the config lives, absent an override. Exposed so `paperctl devices
/// --help` can print it without duplicating the resolution logic in a doc
/// comment that can drift from the code.
pub(crate) fn default_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("PAPERCTL_CONFIG_DIR") {
        return PathBuf::from(dir).join("config.toml");
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("paperctl").join("config.toml");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("paperctl").join("config.toml")
}

/// Refuses a host with no format that `ssh` could plausibly do anything with.
/// Not a reachability check — a pin is allowed for a tablet that is asleep
/// right now, which is the whole point of pinning one.
fn validate_host(host: &str) -> Result<(), ConfigError> {
    if host.is_empty() {
        return Err(ConfigError::InvalidHost {
            host: host.to_owned(),
            reason: "empty",
        });
    }
    if host.trim() != host {
        return Err(ConfigError::InvalidHost {
            host: host.to_owned(),
            reason: "has leading or trailing whitespace",
        });
    }
    if host.chars().any(char::is_control) {
        return Err(ConfigError::InvalidHost {
            host: host.to_owned(),
            reason: "contains a control character",
        });
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_' | '@');
    if !host.chars().all(allowed) {
        return Err(ConfigError::InvalidHost {
            host: host.to_owned(),
            reason: "contains a character that is not valid in a hostname, \
                      address or `user@host` ssh destination",
        });
    }
    if host.len() > 255 {
        return Err(ConfigError::InvalidHost {
            host: host.to_owned(),
            reason: "longer than 255 characters",
        });
    }
    Ok(())
}

impl Config {
    /// Loads the config at `path`, or an empty one if nothing is there yet —
    /// "not set up" and "set up with nothing pinned" are the same state.
    pub(crate) fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                file: ConfigFile::default(),
            });
        }
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let file = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    /// The pinned device, if one is set.
    pub(crate) fn pinned(&self) -> Option<&str> {
        self.file.pinned.as_deref()
    }

    /// The last device a resolution succeeded against.
    pub(crate) fn cached(&self) -> Option<&str> {
        self.file.cache.as_deref()
    }

    /// Pins `host`, skipping discovery for every resolution from now on.
    ///
    /// Validates before touching the file at all: a rejected pin must leave
    /// the config exactly as it was.
    pub(crate) fn pin(&mut self, host: &str) -> Result<(), ConfigError> {
        validate_host(host)?;
        self.file.pinned = Some(host.to_owned());
        self.save()
    }

    /// Clears the pin. Auto-discovery resumes on the next resolution.
    pub(crate) fn unpin(&mut self) -> Result<(), ConfigError> {
        self.file.pinned = None;
        self.save()
    }

    /// Records the host the most recent successful resolution used, so a
    /// tablet with no USB link and no mDNS response can still be found next
    /// time — the "cached last-good host" resolution source.
    // Mac-only: `transport::remote` is the only caller and it is
    // `#[cfg(not(target_os = "linux"))]`.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn remember(&mut self, host: &str) -> Result<(), ConfigError> {
        if self.file.cache.as_deref() == Some(host) {
            return Ok(());
        }
        self.file.cache = Some(host.to_owned());
        self.save()
    }

    fn save(&self) -> Result<(), ConfigError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Read {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let document = toml::to_string_pretty(&self.file)?;
        atomic_write(&self.path, document.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        (dir, path)
    }

    #[test]
    fn round_trips_a_pin() {
        let (_dir, path) = temp_config();

        let mut config = Config::load(&path).expect("load");
        assert_eq!(config.pinned(), None);
        config.pin("10.0.0.12").expect("pin");

        let reloaded = Config::load(&path).expect("reload");
        assert_eq!(reloaded.pinned(), Some("10.0.0.12"));
    }

    #[test]
    fn round_trips_an_unpin() {
        let (_dir, path) = temp_config();

        let mut config = Config::load(&path).expect("load");
        config.pin("remarkable-wifi").expect("pin");
        config.unpin().expect("unpin");

        let reloaded = Config::load(&path).expect("reload");
        assert_eq!(reloaded.pinned(), None);
    }

    #[test]
    fn rejects_an_empty_host_without_writing() {
        let (_dir, path) = temp_config();
        std::fs::write(&path, "pinned = \"already-here\"\n").expect("seed");
        let before = std::fs::read(&path).expect("read before");

        let mut config = Config::load(&path).expect("load");
        let result = config.pin("");
        assert!(result.is_err());

        let after = std::fs::read(&path).expect("read after");
        assert_eq!(before, after, "a rejected pin must not touch the file");
    }

    #[test]
    fn rejects_a_host_with_shell_metacharacters() {
        let (_dir, path) = temp_config();
        let mut config = Config::load(&path).expect("load");
        assert!(config.pin("tablet; rm -rf /").is_err());
        assert!(config.pin("tablet\nEXTRA").is_err());
        assert!(!path.exists(), "a rejected pin must not create the file");
    }
}
