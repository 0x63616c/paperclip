//! Returning the tablet to stock and taking Paperclip off it (§14).
//!
//! # The requirement, stated exactly
//!
//! §14: a documented way to return to stock and remove Paperclip **without
//! unintentionally deleting notebooks or app data**. Two different promises,
//! and this module keeps them differently.
//!
//! *Notebooks* are not ours. They live in Xochitl's own directory and nothing
//! here can name them: [`plan`] only ever produces paths under the two roots
//! Paperclip owns, and [`Removal::execute`] refuses a root without the
//! [`PLATFORM_MARKER`](crate::layout::PLATFORM_MARKER) file in it. Removing
//! Paperclip cannot reach a notebook, and that is a property of the code
//! rather than of the operator being careful.
//!
//! *App data* is ours, and is the thing someone might actually want to keep
//! across a reinstall — a half-finished game of Chess. So it is a decision
//! [`plan`] takes explicitly, and the default is to keep it.
//!
//! # A plan, then an execution
//!
//! Nothing here removes anything until a caller has been handed the list and
//! asked for it. The device half of Paperclip is nine directories with similar
//! names, several of which are one typo from something that takes an afternoon
//! to rebuild; printing the list first costs a line of output and is the
//! difference between a removal and an incident.
//!
//! # What this does not do
//!
//! It does not touch `/run/systemd/system`. Units are runtime-only (WWW-11),
//! so they are gone at the next boot whatever happens here, and a removal that
//! stopped units would be a removal that could take the display down while
//! somebody was using it. Returning to stock is
//! [`SessionControl::stand_down`](crate::health::SessionControl::stand_down),
//! and it happens before this, on purpose and as its own step.

use std::fs;
use std::path::{Path, PathBuf};

use paper_packages::store::{self, Layout};

use crate::error::UpdateError;
use crate::layout::{PLATFORM_MARKER, PlatformLayout};

/// What a removal would delete, and what it would leave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// Paths that would be deleted, in order.
    pub removes: Vec<PathBuf>,
    /// Paths that would deliberately be left, with why.
    pub keeps: Vec<(PathBuf, &'static str)>,
    platform_root: PathBuf,
}

impl Removal {
    /// The plan as lines a person can read before agreeing to it.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for path in &self.removes {
            out.push_str(&format!("remove  {}\n", path.display()));
        }
        for (path, why) in &self.keeps {
            out.push_str(&format!("keep    {}  ({why})\n", path.display()));
        }
        out
    }

    /// Carries the plan out.
    ///
    /// # Errors
    ///
    /// [`UpdateError::NotAPaperclipRoot`] if the platform root has no marker
    /// file — a root this code did not create is a root it will not delete
    /// from — or any filesystem failure.
    pub fn execute(&self) -> Result<Removed, UpdateError> {
        if !self.platform_root.join(PLATFORM_MARKER).is_file() {
            return Err(UpdateError::NotAPaperclipRoot {
                path: self.platform_root.clone(),
            });
        }
        let mut removed = Vec::new();
        for path in &self.removes {
            if !path.exists() {
                continue;
            }
            if path.is_dir() {
                store::remove_tree(path)?;
            } else {
                fs::remove_file(path).map_err(|error| UpdateError::io(path, error))?;
            }
            removed.push(path.clone());
        }
        Ok(Removed {
            removed,
            kept: self.keeps.iter().map(|(path, _)| path.clone()).collect(),
        })
    }
}

/// What a removal actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    /// Paths deleted.
    pub removed: Vec<PathBuf>,
    /// Paths left alone.
    pub kept: Vec<PathBuf>,
}

/// What to do with the things an app wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppData {
    /// Leave it. The default, and what §14 means by "without unintentionally
    /// deleting app data".
    Keep,
    /// Delete it too. Only ever on an explicit request.
    Remove,
}

/// Works out what removing Paperclip would delete.
///
/// The marker file goes last in the list, so an interrupted removal still
/// looks like a Paperclip root to the next attempt and can be finished rather
/// than refused.
pub fn plan(platform: &PlatformLayout, store: &Layout, data: AppData) -> Removal {
    let mut removes = vec![
        platform.releases_dir(),
        platform.current(),
        platform.previous(),
        platform.staging_dir(),
        platform.state_dir(),
        platform.keys_dir(),
        platform.bin_dir(),
    ];
    let mut keeps: Vec<(PathBuf, &'static str)> = Vec::new();

    let apps = store.root().join("apps");
    let staging = store.root().join("staging");
    let state = store.root().join("state");
    removes.push(apps);
    removes.push(staging);
    removes.push(state);

    let app_data = store.root().join("data");
    let shared = store.root().join("shared");
    match data {
        AppData::Keep => {
            keeps.push((app_data, "app data — §14: not deleted unintentionally"));
            keeps.push((shared, "explicitly shared files between apps"));
        }
        AppData::Remove => {
            removes.push(app_data);
            removes.push(shared);
        }
    }
    keeps.push((
        PathBuf::from("/home/root/.local/share/remarkable"),
        "notebooks — Xochitl's, never Paperclip's to delete",
    ));

    removes.push(platform.root().join(PLATFORM_MARKER));

    Removal {
        removes,
        keeps,
        platform_root: platform.root().to_path_buf(),
    }
}

/// Whether `path` is inside `root`. Used by the tests that pin the promise
/// that nothing outside the two roots is ever named.
pub fn is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}
