//! The install transaction: stage, verify, commit, activate, recover (§12).
//!
//! # The order, and why it is that order
//!
//! 1. **Validate the metadata.** The release descriptor is already signed by a
//!    trusted key before it reaches here — [`VerifiedRelease`] cannot be built
//!    any other way — and the capability policy that will apply is decided
//!    now, from host policy, not from anything in the package.
//! 2. **Download into bounded staging.** Never into the release directory. The
//!    bytes are hashed and counted in the same pass that writes them.
//! 3. **Verify size, digest, signature and structure** before anything is
//!    unpacked, then verify the unpacked manifest against the signed
//!    descriptor. A package whose `paper.toml` disagrees with what was signed
//!    is refused, because otherwise the signature covers the wrapper and not
//!    the contents.
//! 4. **Extract, executing nothing.** There is no install script hook.
//! 5. **Commit the complete directory durably** — marker written last, whole
//!    tree fsynced, then renamed.
//! 6. **Activate only when the app is stopped**, through a journalled
//!    selection change, with the app's lock held throughout.
//! 7. **Keep the previous release.** Rollback is a selection change, not a
//!    restore.
//!
//! # What an interruption leaves behind
//!
//! At every point, the invariant is the same: *the selected version is a
//! complete release directory, or there is no selected version.*
//!
//! - Killed during download or extraction → a staging directory and a
//!   `stage` journal entry. The prior release is still selected and still
//!   launchable. [`PackageManager::recover`] deletes both.
//! - Killed between commit and activation → the new release exists and is
//!   complete but unselected. Recovery finishes the activation, because the
//!   journal says that was the intent and the bytes are all there.
//! - Killed *during* activation, after `previous` and before `current` →
//!   recovery finishes it, for the same reason. Rerunning the final two
//!   writes is idempotent.
//! - Killed with a half-written release directory → impossible. A release
//!   directory is renamed into place already complete, or it does not exist.
//!
//! Extraction succeeding does not make a release known-good (§12). That is
//! what [`launch`](crate::launch) is for, and it is a separate question asked
//! after something has actually tried to run.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;

use crate::archive::{self, ArchiveError, ArchiveLimits};
use crate::capability::{Capability, GrantedCapabilities, InstallPolicy, InstalledApp};
use crate::digest::{Digest, MeasuredReader};
use crate::error::ManifestError;
use crate::id::AppId;
use crate::manifest::Manifest;
use crate::release::{Release, VerifiedRelease, VersionConflict};
use crate::store::{self, AppLock, Layout, Marker, RELEASE_MARKER, StoreError};

/// Whether an app is running, asked at the moment activation would happen.
///
/// §12: activation happens only when the app is stopped, so an update never
/// replaces the running version of an active app. The host knows this; this
/// crate does not, and inventing an answer here would be worse than asking.
pub trait ActivationGuard: fmt::Debug {
    /// Whether `app` is currently running.
    fn is_running(&self, app: &AppId) -> bool;
}

/// The answer on a machine with no supervisor — the Mac, and tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NothingIsRunning;

impl ActivationGuard for NothingIsRunning {
    fn is_running(&self, _app: &AppId) -> bool {
        false
    }
}

/// How far an install has got.
///
/// §6 requires the App Store to show download and install progress. Only the
/// download has a meaningful fraction; the rest are steps that either have
/// happened or have not, and reporting a made-up percentage for them would be
/// a progress bar that lies at exactly the moment someone is watching it
/// because something is slow.
/// Deliberately *not* `#[non_exhaustive]`, unlike the errors and the result
/// structs in this module. Every consumer of a step has to put a word on a
/// screen for it, and a catch-all arm is how a new step becomes a blank label
/// nobody notices. Adding a variant here should break the App Store's `match`
/// and make someone choose the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Bytes are arriving. `total` is the size the signed descriptor declares,
    /// so the fraction is trustworthy before a single byte has been read.
    Downloading {
        /// Bytes written to staging so far.
        done: u64,
        /// Bytes the signed release says there are.
        total: u64,
    },
    /// Checking size, digest and package structure.
    Verifying,
    /// Unpacking into staging.
    Extracting,
    /// Making the release durable and moving it into place.
    Committing,
    /// Changing which version is selected.
    Activating,
}

/// Something watching an install happen.
///
/// Reporting only; a [`Progress`] cannot cancel, fail or alter an install, and
/// nothing in the transaction waits on it. An App Store that stops drawing has
/// not stopped an install, and an install does not slow down because nobody is
/// looking.
pub trait Progress: fmt::Debug {
    /// Called as each step begins, and repeatedly while downloading.
    fn step(&self, step: Step);
}

/// The observer for callers that are not showing anyone anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unwatched;

impl Progress for Unwatched {
    fn step(&self, _step: Step) {}
}

/// A reader that reports how much has gone through it.
///
/// Wrapped *outside* the digest reader so what is reported is what was
/// accepted, not what a source offered.
#[derive(Debug)]
struct Watched<'a, R> {
    inner: R,
    progress: &'a dyn Progress,
    done: u64,
    total: u64,
    reported: u64,
}

impl<R: Read> Read for Watched<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.done += read as u64;
        // Report on every 64 KiB and at the end, not on every `read`: a
        // progress callback per 8 KiB chunk is a redraw storm on an e-ink
        // panel, which is the one display where that actually costs something.
        if read == 0 || self.done - self.reported >= 64 * 1024 {
            self.reported = self.done;
            self.progress.step(Step::Downloading {
                done: self.done,
                total: self.total,
            });
        }
        Ok(read)
    }
}

/// How an install should behave where §12 leaves a choice.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InstallOptions {
    /// Select the new version once it is committed.
    ///
    /// Selecting is not launching. Nothing in this crate starts a process, and
    /// §12 forbids a launch triggered by an install.
    pub activate: bool,
    /// Allow selecting a version older than the one selected now.
    ///
    /// Off by default: a downgrade is a decision, and `rollback` is the way to
    /// express it. An install path that silently accepts an older version is
    /// how a stale catalog walks a device backwards.
    pub allow_downgrade: bool,
    /// Install a package built against a protocol this platform cannot run.
    ///
    /// Off by default. On, it commits the release but refuses to activate it,
    /// so the App Store can hold a download for a platform update.
    pub allow_incompatible: bool,
    /// Delete releases that are neither selected nor the fallback.
    pub prune: bool,
    /// Bounds applied to the archive.
    pub limits: ArchiveLimits,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            activate: true,
            allow_downgrade: false,
            allow_incompatible: false,
            prune: true,
            limits: ArchiveLimits::DEFAULT,
        }
    }
}

/// What an install did.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Installed {
    /// Which app.
    pub app: AppId,
    /// The version now on disk.
    pub version: Version,
    /// What was selected before, if anything.
    pub previous: Option<Version>,
    /// Whether the new version is now selected.
    pub activated: bool,
    /// Whether the bytes were already on disk, so nothing was written.
    pub already_present: bool,
    /// What host policy grants this app. Computed, never read from a package.
    pub capabilities: GrantedCapabilities,
    /// Releases deleted by pruning.
    pub pruned: Vec<Version>,
}

/// What a rollback did.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RolledBack {
    /// Which app.
    pub app: AppId,
    /// What is selected now.
    pub version: Version,
    /// What was selected before.
    pub from: Version,
}

/// What recovery found and did.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Recovered {
    /// Activations that were interrupted and have now been completed.
    pub completed: Vec<(AppId, Version)>,
    /// Activations whose target was not usable, so the fallback was restored.
    pub reverted: Vec<(AppId, Option<Version>)>,
    /// Staging directories deleted.
    pub staging_removed: usize,
    /// Locks left behind by killed processes, cleared.
    pub locks_broken: usize,
    /// Incomplete release directories deleted.
    pub incomplete_removed: Vec<(AppId, String)>,
}

impl Recovered {
    /// Whether recovery had anything to do.
    pub fn is_clean(&self) -> bool {
        self.completed.is_empty()
            && self.reverted.is_empty()
            && self.staging_removed == 0
            && self.locks_broken == 0
            && self.incomplete_removed.is_empty()
    }
}

/// The platform facility that installs packages.
///
/// The *platform* owns this, and an app is a client of it. [`Self::host`] is
/// how the host builds one; [`Self::on_behalf_of`] is how an app asks for one,
/// and requires [`Capability::Packages`], which only host policy can grant. So
/// the App Store manages packages because it was granted that, not because it
/// is the App Store (§5, §12), and an ordinary app cannot obtain one at all.
#[derive(Debug)]
pub struct PackageManager {
    layout: Layout,
    policy: InstallPolicy,
}

impl PackageManager {
    /// The host's own package manager.
    pub fn host(layout: Layout, policy: InstallPolicy) -> Self {
        Self { layout, policy }
    }

    /// A package manager for an app that has been granted package management.
    pub fn on_behalf_of(
        layout: Layout,
        policy: InstallPolicy,
        caller: &InstalledApp,
    ) -> Result<Self, InstallError> {
        if !caller.capabilities().holds(Capability::Packages) {
            return Err(InstallError::NotPermitted {
                app: caller.manifest().id().clone(),
            });
        }
        Ok(Self::host(layout, policy))
    }

    /// The layout it works in.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Installs `release` from `archive`.
    ///
    /// `archive` is a stream, not a path: the caller may be reading from a
    /// file, a catalog transport or a test, and none of them should have to
    /// materialise the whole package in memory to hand it over.
    pub fn install(
        &self,
        release: &VerifiedRelease,
        archive: impl Read,
        options: &InstallOptions,
        guard: &dyn ActivationGuard,
        progress: &dyn Progress,
    ) -> Result<Installed, InstallError> {
        let signed = release.release();
        let span = tracing::info_span!(
            "install",
            app = %signed.app(),
            version = %signed.version(),
        );
        let _entered = span.enter();
        self.install_inner(release, archive, options, guard, progress)
    }

    fn install_inner(
        &self,
        release: &VerifiedRelease,
        archive: impl Read,
        options: &InstallOptions,
        guard: &dyn ActivationGuard,
        progress: &dyn Progress,
    ) -> Result<Installed, InstallError> {
        self.layout.ensure()?;
        let signed = release.release();
        let app = signed.app().clone();
        let version = signed.version().clone();
        let _lock = AppLock::acquire(&self.layout, &app)?;

        let current = self.layout.current(&app)?;
        if !options.allow_downgrade
            && let Some(selected) = &current
            && selected > &version
        {
            return Err(InstallError::Downgrade {
                app,
                installed: selected.clone(),
                offered: version,
            });
        }

        let destination = self.layout.release_dir(&app, &version);
        // Already here, byte for byte: nothing to write, and the request is
        // satisfied. An identical republish is not an error; §12 only forbids
        // *different* bytes under a version that has been published.
        if let Some(marker) = existing_marker(&destination)? {
            if marker.digest != signed.digest() {
                return Err(InstallError::VersionExists(Box::new(VersionConflict {
                    app,
                    version,
                    recorded: marker.digest,
                    offered: signed.digest(),
                })));
            }
            let manifest = Manifest::read_package(&destination)
                .map_err(|source| InstallError::Manifest { source })?;
            return self.finish(Finish {
                app: &app,
                version: &version,
                manifest: &manifest,
                current,
                options,
                guard,
                progress,
                activate: options.activate,
                already_present: true,
            });
        }

        let transaction = Transaction::begin(&self.layout, &app, &version)?;
        let outcome = self.install_staged(
            &transaction,
            release,
            archive,
            options,
            guard,
            current,
            progress,
        );
        match outcome {
            Ok(installed) => {
                transaction.finish()?;
                Ok(installed)
            }
            // A clean failure tidies up after itself; a crash cannot, which is
            // what the journal entry left behind here is for. Either way the
            // previously selected release was never touched.
            Err(error) => {
                let _ = transaction.finish();
                Err(error)
            }
        }
    }

    /// Everything between "staging exists" and "the new version is selected".
    #[expect(
        clippy::too_many_arguments,
        reason = "one call site; a struct to carry six borrowed values through \
                  a single call would be ceremony, not clarity"
    )]
    fn install_staged(
        &self,
        transaction: &Transaction,
        release: &VerifiedRelease,
        archive: impl Read,
        options: &InstallOptions,
        guard: &dyn ActivationGuard,
        current: Option<Version>,
        progress: &dyn Progress,
    ) -> Result<Installed, InstallError> {
        let signed = release.release();
        let app = signed.app().clone();
        let version = signed.version().clone();
        let staged = self.stage(transaction, signed, archive, options, progress)?;

        // The signed descriptor and the package's own manifest must agree.
        // Without this the signature covers a wrapper: anyone able to swap
        // archives inside a catalog could ship a different app under a name
        // the descriptor made trustworthy.
        check_agreement(signed, &staged.manifest)?;
        let runnable = staged.manifest.ensure_runnable();
        if !options.allow_incompatible {
            runnable.map_err(|source| InstallError::Manifest { source })?;
        }

        let marker = Marker {
            digest: signed.digest(),
            signer: release.signer(),
            installed: store::now(),
        };
        store::atomic_write(
            &staged.payload.join(RELEASE_MARKER),
            marker.to_document().as_bytes(),
        )?;
        progress.step(Step::Committing);
        store::commit_directory(&staged.payload, &self.layout.release_dir(&app, &version))?;

        self.finish(Finish {
            app: &app,
            version: &version,
            manifest: &staged.manifest,
            current,
            options,
            guard,
            progress,
            // A release built against a protocol this platform cannot run is
            // kept, and not selected. The App Store can hold it until the
            // platform catches up; selecting it would hand the host a binary
            // it cannot talk to.
            activate: options.activate && staged.manifest.ensure_runnable().is_ok(),
            already_present: false,
        })
    }

    /// Selects the fallback release, explicitly.
    ///
    /// Never automatic. §12 wants a failed release to be *recoverable from*,
    /// not silently replaced — an installer that rolls back on its own hides
    /// the failure that made it necessary.
    pub fn rollback(
        &self,
        app: &AppId,
        guard: &dyn ActivationGuard,
    ) -> Result<RolledBack, InstallError> {
        let _lock = AppLock::acquire(&self.layout, app)?;
        let from = self
            .layout
            .current(app)?
            .ok_or_else(|| InstallError::NothingSelected { app: app.clone() })?;
        let to = self
            .layout
            .previous(app)?
            .ok_or_else(|| InstallError::NoFallback { app: app.clone() })?;
        if guard.is_running(app) {
            return Err(InstallError::Running { app: app.clone() });
        }

        // The version rolled back *from* becomes the fallback, so a rollback
        // can itself be undone without going back to the catalog.
        let journal = Journal {
            app: app.clone(),
            version: to.clone(),
            previous: Some(from.clone()),
            staging: None,
            phase: Phase::Activate,
            started: store::now(),
        };
        let entry = journal.write(&self.layout)?;
        self.apply_selection(app, &to, Some(&from))?;
        entry.remove()?;

        Ok(RolledBack {
            app: app.clone(),
            version: to,
            from,
        })
    }

    /// Repairs whatever a crash or a power cut left behind.
    ///
    /// Safe to run at any time, including when nothing is wrong, and intended
    /// to run at host start before anything is launched. It is the other half
    /// of the journal: writing an intent down is only useful if something
    /// reads it back.
    pub fn recover(&self) -> Result<Recovered, InstallError> {
        self.layout.ensure()?;
        // First, and before anything tries to take one: a lock still on disk
        // was left by a process that was killed rather than one that is
        // working, because recovery runs before anything has been launched.
        // Leaving it would refuse every future install of that app, which is
        // precisely the "a crash invalidated a host-owned transaction" §12
        // rules out — and it would deadlock the loop below, which locks each
        // app it repairs.
        let mut report = Recovered {
            locks_broken: AppLock::break_all(&self.layout)?,
            ..Recovered::default()
        };

        for entry in JournalEntry::all(&self.layout)? {
            let journal = entry.read()?;
            let _lock = AppLock::acquire(&self.layout, &journal.app)?;

            if let Some(staging) = &journal.staging {
                store::remove_tree(staging)?;
                report.staging_removed += 1;
            }

            if journal.phase == Phase::Activate {
                let target = self.layout.release_dir(&journal.app, &journal.version);
                if existing_marker(&target)?.is_some() {
                    // The bytes are all there and the intent was recorded, so
                    // finishing is what the operator asked for. Re-running the
                    // two selection writes is idempotent.
                    self.apply_selection(
                        &journal.app,
                        &journal.version,
                        journal.previous.as_ref(),
                    )?;
                    report
                        .completed
                        .push((journal.app.clone(), journal.version.clone()));
                } else {
                    let fallback = self.usable_fallback(&journal)?;
                    self.restore_selection(&journal.app, fallback.as_ref())?;
                    report.reverted.push((journal.app.clone(), fallback));
                }
            }

            entry.remove()?;
        }

        report.staging_removed += self.sweep_staging()?;
        report.incomplete_removed = self.sweep_incomplete()?;
        Ok(report)
    }

    /// Every complete release of an app, oldest first.
    pub fn installed_versions(&self, app: &AppId) -> Result<Vec<Version>, InstallError> {
        Ok(self.layout.installed_versions(app)?)
    }

    /// What host policy grants an app, computed fresh from policy.
    ///
    /// Never stored and read back: [`GrantedCapabilities`] has no
    /// deserialisation path, and giving it one would be a way for a file on
    /// disk to become a grant (ADR-0003).
    pub fn capabilities(&self, manifest: &Manifest) -> GrantedCapabilities {
        self.policy.grant(manifest)
    }

    /// Downloads and unpacks into staging, verifying as it goes.
    fn stage(
        &self,
        transaction: &Transaction,
        release: &Release,
        archive: impl Read,
        options: &InstallOptions,
        progress: &dyn Progress,
    ) -> Result<Staged, InstallError> {
        let archive_path = transaction.dir.join("download.paperpkg");
        progress.step(Step::Downloading {
            done: 0,
            total: release.size(),
        });
        // The declared size *is* the limit. More bytes than the descriptor
        // promised is a failure, not something to notice afterwards.
        let watched = Watched {
            inner: archive,
            progress,
            done: 0,
            total: release.size(),
            reported: 0,
        };
        let mut reader = MeasuredReader::new(watched, release.size());
        let mut file = std::fs::File::create(&archive_path).map_err(|source| InstallError::Io {
            path: archive_path.clone(),
            source,
        })?;
        let copied = std::io::copy(&mut reader, &mut file).map_err(|source| InstallError::Io {
            path: archive_path.clone(),
            source,
        })?;
        drop(file);

        progress.step(Step::Verifying);
        if copied != release.size() {
            return Err(InstallError::Size {
                expected: release.size(),
                actual: copied,
            });
        }
        let digest = reader.digest();
        if digest != release.digest() {
            return Err(InstallError::Digest {
                expected: release.digest(),
                actual: digest,
            });
        }

        progress.step(Step::Extracting);
        let payload = transaction.dir.join("payload");
        std::fs::create_dir(&payload).map_err(|source| InstallError::Io {
            path: payload.clone(),
            source,
        })?;
        let opened = std::fs::File::open(&archive_path).map_err(|source| InstallError::Io {
            path: archive_path.clone(),
            source,
        })?;
        let extracted = archive::extract(opened, &payload, options.limits)?;

        Ok(Staged {
            payload,
            manifest: extracted.manifest,
        })
    }

    /// The shared tail of a fresh install and an already-present one.
    fn finish(&self, request: Finish<'_>) -> Result<Installed, InstallError> {
        let Finish {
            app,
            version,
            manifest,
            current,
            options,
            guard,
            progress,
            activate,
            already_present,
        } = request;

        let mut activated = current.as_ref() == Some(version) && activate;
        if activate && current.as_ref() != Some(version) {
            if guard.is_running(app) {
                return Err(InstallError::Running { app: app.clone() });
            }
            progress.step(Step::Activating);
            let journal = Journal {
                app: app.clone(),
                version: version.clone(),
                previous: current.clone(),
                staging: None,
                phase: Phase::Activate,
                started: store::now(),
            };
            let entry = journal.write(&self.layout)?;
            self.apply_selection(app, version, current.as_ref())?;
            entry.remove()?;
            activated = true;
        }

        let pruned = if options.prune {
            // `version` is in the keep set explicitly. An install that
            // deliberately did not activate — a download held for a platform
            // update, a staged rollout — has just written a release that is
            // neither current nor previous, and pruning by selection alone
            // would delete the thing the install was for.
            self.prune(app, version)?
        } else {
            Vec::new()
        };

        Ok(Installed {
            app: app.clone(),
            version: version.clone(),
            previous: current,
            activated,
            already_present,
            capabilities: self.policy.grant(manifest),
            pruned,
        })
    }

    /// Writes the fallback, then the selection. In that order, always.
    ///
    /// A crash between the two leaves `previous` pointing somewhere true and
    /// `current` still pointing at the old release — which is exactly the
    /// state recovery knows how to finish. The other order would leave
    /// `current` on the new release with no recorded way back.
    fn apply_selection(
        &self,
        app: &AppId,
        version: &Version,
        previous: Option<&Version>,
    ) -> Result<(), InstallError> {
        store::create_dir_if_missing(&self.layout.app_dir(app))?;
        match previous {
            Some(previous) if previous != version => {
                store::atomic_write(
                    &self.layout.previous_file(app),
                    format!("{previous}\n").as_bytes(),
                )?;
            }
            _ => {}
        }
        store::atomic_write(
            &self.layout.current_file(app),
            format!("{version}\n").as_bytes(),
        )?;
        store::sync_directory(&self.layout.app_dir(app))?;
        Ok(())
    }

    /// Points the selection back at a usable release, or removes it.
    ///
    /// Tolerates an app directory that is not there at all: a journal entry can
    /// outlive the app it names, and recovery refusing to run because of one
    /// stale file would be the wrong failure entirely.
    fn restore_selection(
        &self,
        app: &AppId,
        fallback: Option<&Version>,
    ) -> Result<(), InstallError> {
        if !self.layout.app_dir(app).exists() {
            return Ok(());
        }
        match fallback {
            Some(version) => {
                store::atomic_write(
                    &self.layout.current_file(app),
                    format!("{version}\n").as_bytes(),
                )?;
            }
            None => {
                // No complete release left. Removing the selection is honest;
                // leaving it pointing at debris would let something try to run
                // a directory that is not all there.
                match std::fs::remove_file(self.layout.current_file(app)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(InstallError::Io {
                            path: self.layout.current_file(app),
                            source,
                        });
                    }
                }
            }
        }
        store::sync_directory(&self.layout.app_dir(app))?;
        Ok(())
    }

    /// The best complete release to fall back to after a failed activation.
    fn usable_fallback(&self, journal: &Journal) -> Result<Option<Version>, InstallError> {
        if let Some(previous) = &journal.previous
            && existing_marker(&self.layout.release_dir(&journal.app, previous))?.is_some()
        {
            return Ok(Some(previous.clone()));
        }
        if let Some(current) = self.layout.current(&journal.app)? {
            return Ok(Some(current));
        }
        Ok(self
            .layout
            .installed_versions(&journal.app)?
            .into_iter()
            .next_back())
    }

    /// Deletes releases that are neither selected, the fallback, nor `keep`.
    fn prune(&self, app: &AppId, keep: &Version) -> Result<Vec<Version>, InstallError> {
        let keep: Vec<Version> = [
            self.layout.current(app)?,
            self.layout.previous(app)?,
            Some(keep.clone()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut pruned = Vec::new();
        for version in self.layout.installed_versions(app)? {
            if keep.contains(&version) {
                continue;
            }
            store::remove_tree(&self.layout.release_dir(app, &version))?;
            pruned.push(version);
        }
        Ok(pruned)
    }

    /// Deletes staging directories no journal entry claims.
    fn sweep_staging(&self) -> Result<usize, InstallError> {
        let staging = self.layout.staging_dir();
        let mut removed = 0;
        let entries = match std::fs::read_dir(&staging) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(source) => {
                return Err(InstallError::Io {
                    path: staging,
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| InstallError::Io {
                path: staging.clone(),
                source,
            })?;
            store::remove_tree(&entry.path())?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Deletes release directories with no completion marker.
    fn sweep_incomplete(&self) -> Result<Vec<(AppId, String)>, InstallError> {
        let mut removed = Vec::new();
        for app in self.layout.apps()? {
            let releases = self.layout.releases_dir(&app);
            let entries = match std::fs::read_dir(&releases) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(InstallError::Io {
                        path: releases,
                        source,
                    });
                }
            };
            for entry in entries {
                let entry = entry.map_err(|source| InstallError::Io {
                    path: releases.clone(),
                    source,
                })?;
                if !entry.path().is_dir() || existing_marker(&entry.path())?.is_some() {
                    continue;
                }
                store::remove_tree(&entry.path())?;
                removed.push((
                    app.clone(),
                    entry.file_name().to_string_lossy().into_owned(),
                ));
            }
        }
        Ok(removed)
    }
}

/// What [`PackageManager::finish`] needs, bundled so the signature stays
/// readable rather than becoming eight positional arguments.
#[derive(Debug)]
struct Finish<'a> {
    app: &'a AppId,
    version: &'a Version,
    manifest: &'a Manifest,
    current: Option<Version>,
    options: &'a InstallOptions,
    guard: &'a dyn ActivationGuard,
    progress: &'a dyn Progress,
    activate: bool,
    already_present: bool,
}

/// A staging directory, and the journal entry that claims it.
///
/// Deliberately without a `Drop` that cleans up: dropping is neither a commit
/// nor a rollback, and a destructor racing whatever went wrong is how a
/// cleanup path deletes something an error path still needed. Staging is
/// removed explicitly on the way out, or by `recover` after a crash.
#[derive(Debug)]
struct Transaction {
    dir: PathBuf,
    entry: JournalEntry,
}

impl Transaction {
    fn begin(layout: &Layout, app: &AppId, version: &Version) -> Result<Self, InstallError> {
        store::create_dir_if_missing(&layout.staging_dir())?;
        let dir = layout.staging_dir().join(format!(
            "{app}-{version}-{}-{}",
            std::process::id(),
            store::now()
        ));
        store::remove_tree(&dir)?;
        std::fs::create_dir(&dir).map_err(|source| InstallError::Io {
            path: dir.clone(),
            source,
        })?;
        let entry = Journal {
            app: app.clone(),
            version: version.clone(),
            previous: None,
            staging: Some(dir.clone()),
            phase: Phase::Stage,
            started: store::now(),
        }
        .write(layout)?;
        Ok(Self { dir, entry })
    }

    /// Removes the staging directory and the entry that claimed it.
    fn finish(&self) -> Result<(), InstallError> {
        store::remove_tree(&self.dir)?;
        self.entry.remove()
    }
}

/// What was staged, once it verified.
#[derive(Debug)]
struct Staged {
    payload: PathBuf,
    manifest: Manifest,
}

/// How far an install had got when it was written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Downloading and unpacking. Nothing outside staging has changed.
    Stage,
    /// About to change, or changing, the selection.
    Activate,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Stage => "stage",
            Phase::Activate => "activate",
        }
    }
}

/// An intent, written down before it is acted on.
#[derive(Debug, Clone)]
struct Journal {
    app: AppId,
    version: Version,
    previous: Option<Version>,
    staging: Option<PathBuf>,
    phase: Phase,
    started: u64,
}

impl Journal {
    fn write(&self, layout: &Layout) -> Result<JournalEntry, InstallError> {
        store::create_dir_if_missing(&layout.journal_dir())?;
        let path = layout.journal_dir().join(format!(
            "{}-{}-{}.toml",
            self.app,
            self.version,
            self.phase.as_str()
        ));
        let mut document = String::from(
            "# An install intent, written before it was acted on. If this file is\n\
             # here, the operation it describes did not finish; `paperctl recover`\n\
             # is what reads it.\n",
        );
        document.push_str(&format!("app = \"{}\"\n", self.app));
        document.push_str(&format!("version = \"{}\"\n", self.version));
        if let Some(previous) = &self.previous {
            document.push_str(&format!("previous = \"{previous}\"\n"));
        }
        if let Some(staging) = &self.staging {
            document.push_str(&format!("staging = \"{}\"\n", staging.display()));
        }
        document.push_str(&format!("phase = \"{}\"\n", self.phase.as_str()));
        document.push_str(&format!("started = {}\n", self.started));

        store::atomic_write(&path, document.as_bytes())?;
        // The intent has to reach the disk before the thing it describes, or
        // recovery reads a journal that never mentioned the change it has to
        // repair. `atomic_write` fsyncs the file; this fsyncs its directory.
        store::sync_directory(&layout.journal_dir())?;
        Ok(JournalEntry { path })
    }
}

/// One journal file on disk.
#[derive(Debug, Clone)]
struct JournalEntry {
    path: PathBuf,
}

impl JournalEntry {
    fn all(layout: &Layout) -> Result<Vec<Self>, InstallError> {
        let dir = layout.journal_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(InstallError::Io { path: dir, source }),
        };
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| InstallError::Io {
                path: dir.clone(),
                source,
            })?;
            if entry.path().extension().is_some_and(|e| e == "toml") {
                found.push(Self { path: entry.path() });
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    fn read(&self) -> Result<Journal, InstallError> {
        let text = std::fs::read_to_string(&self.path).map_err(|source| InstallError::Io {
            path: self.path.clone(),
            source,
        })?;
        let raw: RawJournal = toml::from_str(&text).map_err(|_| InstallError::CorruptJournal {
            path: self.path.clone(),
        })?;
        let corrupt = || InstallError::CorruptJournal {
            path: self.path.clone(),
        };
        Ok(Journal {
            app: raw.app.parse().map_err(|_| corrupt())?,
            version: Version::parse(&raw.version).map_err(|_| corrupt())?,
            previous: raw
                .previous
                .map(|v| Version::parse(&v))
                .transpose()
                .map_err(|_| corrupt())?,
            staging: raw.staging.map(PathBuf::from),
            phase: match raw.phase.as_str() {
                "stage" => Phase::Stage,
                "activate" => Phase::Activate,
                _ => return Err(corrupt()),
            },
            started: raw.started,
        })
    }

    fn remove(&self) -> Result<(), InstallError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(InstallError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        }
        if let Some(parent) = self.path.parent() {
            store::sync_directory(parent)?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJournal {
    app: String,
    version: String,
    #[serde(default)]
    previous: Option<String>,
    #[serde(default)]
    staging: Option<String>,
    phase: String,
    started: u64,
}

/// The marker of a release directory, if it is there and complete.
fn existing_marker(release: &Path) -> Result<Option<Marker>, InstallError> {
    Ok(store::read_marker(release)?)
}

/// Refuses a package whose manifest does not say what the descriptor says.
fn check_agreement(release: &Release, manifest: &Manifest) -> Result<(), InstallError> {
    if manifest.id() != release.app() {
        return Err(InstallError::Disagreement {
            field: "app id",
            signed: release.app().to_string(),
            package: manifest.id().to_string(),
        });
    }
    if manifest.version() != release.version() {
        return Err(InstallError::Disagreement {
            field: "version",
            signed: release.version().to_string(),
            package: manifest.version().to_string(),
        });
    }
    if manifest.protocol() != release.protocol() {
        return Err(InstallError::Disagreement {
            field: "protocol",
            signed: release.protocol().to_string(),
            package: manifest.protocol().to_string(),
        });
    }
    Ok(())
}

/// Why an install, rollback or recovery failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallError {
    /// An app without [`Capability::Packages`] asked to manage packages.
    #[error(
        "`{app}` has not been granted package management. Only an app the host \
         policy grants `packages` can install, update or roll back software."
    )]
    NotPermitted {
        /// Which app asked.
        app: AppId,
    },

    /// The downloaded archive was not the declared length.
    #[error("the download is {actual} bytes; the signed release says {expected}")]
    Size {
        /// What was signed.
        expected: u64,
        /// What arrived.
        actual: u64,
    },

    /// The downloaded archive did not hash to the signed digest.
    #[error("the download does not match the signed digest ({expected}, got {actual})")]
    Digest {
        /// What was signed.
        expected: Digest,
        /// What arrived.
        actual: Digest,
    },

    /// The package's own manifest contradicts the signed descriptor.
    #[error("the signed release says {field} `{signed}`, the package says `{package}`")]
    Disagreement {
        /// Which field.
        field: &'static str,
        /// The signed value.
        signed: String,
        /// The package's value.
        package: String,
    },

    /// This version is installed already, with different bytes.
    #[error(
        "`{}` {} is already installed with different bytes ({}, offered {}). \
         A published version is immutable; publish a new version.",
        .0.app, .0.version, .0.recorded, .0.offered
    )]
    VersionExists(Box<VersionConflict>),

    /// The offered version is older than the selected one.
    #[error(
        "`{app}` {installed} is installed and {offered} is older. Downgrading \
         is `paperctl rollback`, not an install."
    )]
    Downgrade {
        /// Which app.
        app: AppId,
        /// What is selected.
        installed: Version,
        /// What was offered.
        offered: Version,
    },

    /// Activation was asked for while the app was running.
    #[error("`{app}` is running; stop it before changing which version is selected")]
    Running {
        /// Which app.
        app: AppId,
    },

    /// A rollback with nothing selected.
    #[error("`{app}` has no selected version to roll back from")]
    NothingSelected {
        /// Which app.
        app: AppId,
    },

    /// A rollback with no usable previous release.
    #[error("`{app}` has no previous release to roll back to")]
    NoFallback {
        /// Which app.
        app: AppId,
    },

    /// A journal file the installer wrote is unreadable.
    #[error("{path} is not a readable install journal")]
    CorruptJournal {
        /// Which file.
        path: PathBuf,
    },

    /// A release marker is unreadable.
    #[error("{path} is not a readable release marker")]
    CorruptMarker {
        /// Which file.
        path: PathBuf,
    },

    /// The archive could not be opened safely.
    #[error("the package could not be unpacked")]
    Archive(#[from] ArchiveError),

    /// The package's manifest is not usable here.
    #[error("the package's manifest is not usable")]
    Manifest {
        /// Why.
        #[source]
        source: ManifestError,
    },

    /// The store could not be read or written.
    #[error("the package store could not be updated")]
    Store(#[from] StoreError),

    /// A filesystem operation failed.
    #[error("cannot access {path}")]
    Io {
        /// Which path.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}
