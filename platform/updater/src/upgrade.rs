//! The transaction (§13).
//!
//! ```text
//! PREPARE   stage and verify a complete release. Nothing selected changes.
//! ACTIVATE  return the display to stock, then swap `previous` and `current`.
//! VERIFY    bring the candidate up and grade it against the readiness ladder.
//! COMMIT    only if it reached `ready`.
//! ```
//!
//! Failure anywhere before `COMMIT` means rollback, once, and then stop.
//!
//! # The ordering, and what each step buys
//!
//! **Stage before touching anything.** A candidate that turns out to be
//! unsigned, truncated or built for the wrong machine is refused while the
//! running platform is still running and nothing has moved.
//!
//! **Stand down before activating.** The display goes back to stock first.
//! Swapping the binary under a session that still owns the panel is how a
//! device ends up with a blank screen and no supervisor. This is also the step
//! where the wakelock is checked: a wakelock still held after the old
//! supervisor has gone belongs to nobody, and activating over it would mean a
//! tablet that cannot sleep and nothing left that knows why.
//!
//! **Swap `previous` before `current`.** Interrupted between the two, `current`
//! still names the old release and `previous` names it too — harmless, and
//! reconcilable. The other order leaves a `current` with no recorded fallback.
//!
//! **Grade, then commit.** Not the other way round. Committing first and
//! rolling back on failure would mean the journal's terminal state was reached
//! while the outcome was still unknown.
//!
//! # What the updater will not do
//!
//! It will not retry. One climb for the candidate, one for the fallback. If
//! neither is healthy the device is left at stock with the journal saying so,
//! because a tablet sitting in stock with a written explanation is recoverable
//! over SSH and a tablet in a restart loop is not.
//!
//! It will not replace itself, `paperctl`, or the trusted keys. Those live
//! outside `releases/` and a platform bundle has no way to name them: every
//! component path is relative and checked, and the release directory is the
//! only thing extraction writes into.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use paper_packages::archive::{self, ArchiveLimits};
use paper_packages::signing::{Domain, TrustedKeys};
use paper_packages::store;
use semver::Version;

use crate::error::{Unhealthy, UpdateError};
use crate::health::{self, Budget, Clock, HealthReport, SessionControl};
use crate::journal::{Journal, MAX_ATTEMPTS, Phase, Record};
use crate::layout::PlatformLayout;
use crate::manifest::{self, ComponentPolicy, PlatformManifest, VerifiedPlatform};

/// How an upgrade ended.
///
/// Deliberately *not* `#[non_exhaustive]`, unlike the errors. Every variant is
/// a different thing to tell a person standing over a tablet, and a catch-all
/// arm is how a new outcome becomes a blank line nobody notices. Adding one
/// should break `paperctl`'s `match` and make someone choose the words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The candidate reached `ready` and was committed.
    Upgraded {
        /// What was selected before.
        from: Option<Version>,
        /// What is selected now.
        to: Version,
        /// How the candidate did.
        health: HealthReport,
    },
    /// The candidate failed and the previous release came back.
    RolledBack {
        /// The candidate.
        candidate: Version,
        /// What is selected now.
        restored: Version,
        /// Why the candidate was refused.
        failure: Unhealthy,
        /// How the restored release did on the way back.
        health: HealthReport,
    },
    /// Neither the candidate nor the fallback came back. The device is at
    /// stock, the journal says `failed`, and a person has to look.
    ///
    /// Deliberately not an `Err`. This is a completed transaction with a bad
    /// outcome, and the caller has a report to print rather than an error to
    /// propagate.
    Stranded {
        /// The candidate.
        candidate: Version,
        /// Why it was refused.
        failure: Unhealthy,
        /// Why the fallback was not brought back, if it was tried.
        fallback: Option<String>,
    },
}

/// What reconciling an interrupted transaction did.
///
/// Not `#[non_exhaustive]`, for the reason [`Outcome`] is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    /// There was nothing in flight.
    Nothing,
    /// A staged candidate was discarded. Nothing had been selected.
    DiscardedStaging {
        /// The candidate that never activated.
        candidate: Version,
    },
    /// A selection that never committed was reverted.
    Reverted {
        /// The candidate that was selected but never graded healthy.
        candidate: Version,
        /// What is selected now. `None` when the interruption was during a
        /// first install and there was nothing to go back to.
        restored: Option<Version>,
        /// Whether a platform-state snapshot was restored with it.
        state_restored: bool,
    },
}

/// What `paperctl upgrade status` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The selected release.
    pub current: Option<Version>,
    /// The fallback.
    pub previous: Option<Version>,
    /// Every release on disk.
    pub installed: Vec<Version>,
    /// The last transaction, if there has been one.
    pub last: Option<Record>,
}

/// The transaction.
///
/// Holds no state of its own: everything it needs to survive a power cut is on
/// disk, and everything it needs to touch the machine is behind
/// [`SessionControl`].
#[derive(Debug)]
pub struct Upgrade<'a> {
    layout: &'a PlatformLayout,
    session: &'a dyn SessionControl,
    clock: &'a dyn Clock,
    keys: &'a TrustedKeys,
    budget: Budget,
    limits: ArchiveLimits,
    policy: ComponentPolicy,
}

impl<'a> Upgrade<'a> {
    /// A transaction against `layout`, controlling `session`.
    pub fn new(
        layout: &'a PlatformLayout,
        session: &'a dyn SessionControl,
        clock: &'a dyn Clock,
        keys: &'a TrustedKeys,
    ) -> Self {
        Self {
            layout,
            session,
            clock,
            keys,
            budget: Budget::default(),
            limits: ArchiveLimits::DEFAULT,
            policy: ComponentPolicy::for_this_machine(),
        }
    }

    /// Overrides how long a candidate gets to climb.
    #[must_use]
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// Overrides what a component must be built for. Tests only; see
    /// [`ComponentPolicy::machine`].
    #[must_use]
    pub fn with_component_policy(mut self, policy: ComponentPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The journal.
    pub fn journal(&self) -> Journal {
        Journal::new(self.layout.journal_file())
    }

    /// What is selected, what is on disk, and what the last transaction did.
    ///
    /// # Errors
    ///
    /// An unreadable selection, release directory or journal.
    pub fn status(&self) -> Result<Status, UpdateError> {
        Maintenance::new(self.layout).status()
    }

    /// Runs a complete upgrade from a platform bundle.
    ///
    /// # Errors
    ///
    /// Anything that stops the transaction *before* the candidate is graded: a
    /// bad bundle, an in-flight transaction that has not been reconciled, a
    /// session that will not stand down. A candidate that is graded and found
    /// unhealthy is not an error — see [`Outcome::RolledBack`].
    pub fn run(&self, bundle: &Path) -> Result<Outcome, UpdateError> {
        let span = tracing::info_span!("upgrade", bundle = %bundle.display());
        let _entered = span.enter();
        self.run_inner(bundle)
    }

    fn run_inner(&self, bundle: &Path) -> Result<Outcome, UpdateError> {
        self.layout.ensure()?;
        self.refuse_if_in_flight()?;

        let from = self.layout.selected()?;
        let previous_before = self.layout.fallback()?;
        let journal = self.journal();

        // PREPARE. The record is written before the staging directory exists,
        // so an interruption at any point after this leaves something to
        // reconcile rather than an orphan directory nobody will ever look at.
        let staging = self.layout.staging_dir().join(format!("{}", store::now()));
        // `0.0.0` is a placeholder: the version is inside the bundle, and the
        // record has to be durable *before* the bundle is opened. Reconcile
        // handles it — nothing is named `releases/0.0.0`, so the only work it
        // implies is deleting the staging directory, which is the right work.
        let mut record = Record::opening(from.clone(), Version::new(0, 0, 0));
        record.previous_before = previous_before;
        record.staged = Some(staging.clone());
        record.note = "staging a bundle; its version is not known yet".to_owned();
        journal.record(&mut record)?;

        let staged = match self.prepare(bundle, &staging) {
            Ok(staged) => staged,
            Err(error) => {
                let _ = store::remove_tree(&staging);
                record.staged = None;
                let _ = journal.advance(&mut record, Phase::Failed, error.to_string());
                return Err(error);
            }
        };
        let candidate = staged.manifest().version().clone();
        record.to = candidate.clone();
        journal.record(&mut record)?;

        if from.as_ref() == Some(&candidate) {
            let _ = store::remove_tree(&staging);
            record.staged = None;
            let _ = journal.advance(&mut record, Phase::Failed, "already selected");
            return Err(UpdateError::AlreadySelected { version: candidate });
        }

        // Commit the release directory before anything is selected. A release
        // directory is renamed into place already complete, or it does not
        // exist; there is no half-written one to activate.
        let release = self.layout.release_dir(&candidate);
        if release.exists() {
            store::remove_tree(&release)?;
        }
        if let Some(parent) = release.parent() {
            store::create_dir_if_missing(parent)?;
        }
        store::commit_directory(&staging, &release)?;
        record.staged = None;
        journal.record(&mut record)?;

        // The state-migration gate, before anything is activated. Refusing
        // here costs a failed command; discovering it during a rollback costs
        // a tablet.
        let snapshot = self.snapshot_for_rollback(from.as_ref(), staged.manifest())?;
        record.snapshot = snapshot.clone();
        journal.record(&mut record)?;

        // ACTIVATE.
        journal.advance(&mut record, Phase::Activate, "standing down to stock")?;
        self.stand_down()?;
        if let Some(from) = &from {
            self.layout.select(&self.layout.previous(), from)?;
        } else {
            self.layout.deselect(&self.layout.previous())?;
        }
        self.layout.select(&self.layout.current(), &candidate)?;

        // VERIFY.
        record.attempts += 1;
        journal.advance(&mut record, Phase::Verify, "grading the candidate")?;
        let health = self.bring_up_and_grade()?;
        record.reached = Some(health.reached.to_string());
        journal.record(&mut record)?;

        match health.failure() {
            None => {
                journal.advance(&mut record, Phase::Commit, health.summary())?;
                self.prune(&candidate, from.as_ref())?;
                Ok(Outcome::Upgraded {
                    from,
                    to: candidate,
                    health,
                })
            }
            Some(failure) => {
                let outcome = self.roll_back(&mut record, &journal, &candidate, &failure)?;
                Ok(outcome)
            }
        }
    }

    /// Rolls back to `previous` on request, with the same grading and the same
    /// single attempt.
    ///
    /// # Errors
    ///
    /// No fallback recorded, an in-flight transaction, or a session that will
    /// not stand down.
    pub fn rollback(&self) -> Result<Outcome, UpdateError> {
        let span = tracing::info_span!("upgrade", rollback = true);
        let _entered = span.enter();
        self.rollback_inner()
    }

    fn rollback_inner(&self) -> Result<Outcome, UpdateError> {
        self.layout.ensure()?;
        self.refuse_if_in_flight()?;

        let current = self.layout.selected()?;
        let Some(fallback) = self.layout.fallback()? else {
            return Err(UpdateError::Manifest {
                reason: "no previous release is recorded, so there is nothing to roll back to"
                    .to_owned(),
            });
        };
        let candidate = current.clone().unwrap_or_else(|| fallback.clone());
        let journal = self.journal();
        let mut record = Record::opening(current, fallback.clone());
        // A deliberate rollback consumes the fallback: after it, the release
        // being left behind is the one to come back to.
        record.previous_before = record.from.clone();
        record.note = "rollback requested".to_owned();
        journal.advance(&mut record, Phase::Activate, "rollback requested")?;

        let failure = Unhealthy {
            reached: paper_host::readiness::Rung::Ready,
            stalled_at: paper_host::readiness::Rung::Ready,
            note: "rollback requested by hand".to_owned(),
            died: false,
        };
        self.roll_back(&mut record, &journal, &candidate, &failure)
    }

    /// Finishes or undoes whatever an interrupted transaction left behind.
    ///
    /// Safe to run at any time, including when nothing was interrupted, which
    /// is what lets it be the first thing every other command does.
    ///
    /// # Errors
    ///
    /// An unreadable journal, or a filesystem that will not let the revert
    /// happen.
    pub fn reconcile(&self) -> Result<Reconciled, UpdateError> {
        Maintenance::new(self.layout).reconcile()
    }
}

/// The half of the transaction that needs no running session.
///
/// Separate from [`Upgrade`] because `status` and `reconcile` are answerable
/// from the disk alone, and a caller that had to construct a
/// [`SessionControl`] to ask "what happened here?" would be a caller that
/// cannot ask it from a Mac, or from a recovery shell, or on the way up before
/// anything has been started. Reconcile in particular has to run *before* the
/// first takeover after an interrupted update, when there is nothing to
/// control yet.
#[derive(Debug)]
pub struct Maintenance<'a> {
    layout: &'a PlatformLayout,
}

impl<'a> Maintenance<'a> {
    /// Maintenance against `layout`.
    pub fn new(layout: &'a PlatformLayout) -> Self {
        Self { layout }
    }

    /// The journal.
    pub fn journal(&self) -> Journal {
        Journal::new(self.layout.journal_file())
    }

    /// What is selected, what is on disk, and what the last transaction did.
    ///
    /// # Errors
    ///
    /// An unreadable selection, release directory or journal.
    pub fn status(&self) -> Result<Status, UpdateError> {
        Ok(Status {
            current: self.layout.selected()?,
            previous: self.layout.fallback()?,
            installed: self.layout.installed()?,
            last: self.journal().read()?,
        })
    }

    /// Finishes or undoes whatever an interrupted transaction left behind.
    ///
    /// # Errors
    ///
    /// An unreadable journal, or a filesystem that will not let the revert
    /// happen.
    pub fn reconcile(&self) -> Result<Reconciled, UpdateError> {
        let journal = self.journal();
        let Some(mut record) = journal.read()? else {
            return Ok(Reconciled::Nothing);
        };
        if record.phase.is_terminal() {
            return Ok(Reconciled::Nothing);
        }

        match record.phase {
            Phase::Prepare => {
                if let Some(staged) = record.staged.clone() {
                    let _ = store::remove_tree(&staged);
                }
                // The release directory may have been committed just before
                // the interruption. Remove it unless something points at it —
                // an uncommitted release is not a release anyone chose.
                let candidate = record.to.clone();
                let selected = self.layout.selected()?;
                let fallback = self.layout.fallback()?;
                if selected.as_ref() != Some(&candidate) && fallback.as_ref() != Some(&candidate) {
                    let _ = store::remove_tree(&self.layout.release_dir(&candidate));
                }
                record.staged = None;
                journal.advance(
                    &mut record,
                    Phase::Failed,
                    "interrupted while staging; the candidate was discarded",
                )?;
                Ok(Reconciled::DiscardedStaging { candidate })
            }
            Phase::Activate | Phase::Verify => {
                // It never committed, so it does not get to stay selected —
                // whatever `current` happens to point at right now. Resuming
                // would mean starting a candidate that has never been shown to
                // be healthy, unattended, on a device that just came back from
                // an interruption.
                let candidate = record.to.clone();
                let restored = record.from.clone();
                match &restored {
                    Some(version) => self.layout.select(&self.layout.current(), version)?,
                    None => self.layout.deselect(&self.layout.current())?,
                }
                restore_fallback_link(self.layout, &record)?;
                let state_restored = self.restore_snapshot(&record)?;
                let why = if record.attempts >= MAX_ATTEMPTS {
                    "interrupted after the candidate's one attempt; reverted rather than retried"
                } else {
                    "interrupted before the candidate committed; reverted"
                };
                journal.advance(&mut record, Phase::RolledBack, why)?;
                Ok(Reconciled::Reverted {
                    candidate,
                    restored,
                    state_restored,
                })
            }
            Phase::Commit | Phase::RolledBack | Phase::Failed => Ok(Reconciled::Nothing),
        }
    }

    /// Puts a snapshot back, if the transaction took one.
    fn restore_snapshot(&self, record: &Record) -> Result<bool, UpdateError> {
        restore_snapshot(self.layout, record)
    }
}

impl Upgrade<'_> {
    // --- steps ------------------------------------------------------------

    /// Unpacks and verifies a bundle into `staging`.
    fn prepare(&self, bundle: &Path, staging: &Path) -> Result<VerifiedPlatform, UpdateError> {
        store::create_dir_if_missing(&self.layout.staging_dir())?;
        store::create_dir_if_missing(staging)?;

        let file = fs::File::open(bundle).map_err(|source| UpdateError::io(bundle, source))?;
        // The same hostile-archive protections the app installer gets: two
        // size bounds, an entry ceiling, no symlinks, no device nodes, no
        // absolute paths, no `..`. One extractor, not two.
        let tree = archive::extract_tree(file, staging, self.limits)?;

        let verified = manifest::read_release(staging, self.keys)?;
        verified.manifest().verify_payload(staging, self.policy)?;

        // Every file that came out of the bundle is named by the signed
        // manifest. Without this the signature would cover the files it lists
        // and say nothing about the ones it does not, and "a complete release,
        // verified" (§13) would mean "the parts of it somebody chose to
        // mention". An unlisted file in a release directory is inert today;
        // it is one `ExecStart=` away from not being.
        for name in &tree.names {
            let name = name.as_str();
            if name == manifest::MANIFEST_FILE_NAME || name == manifest::SIGNATURE_FILE_NAME {
                continue;
            }
            if verified
                .manifest()
                .components()
                .iter()
                .all(|component| component.path() != name)
            {
                return Err(UpdateError::Component {
                    name: name.to_owned(),
                    reason: "in the bundle, but not in the signed manifest".to_owned(),
                });
            }
        }
        // Extraction deliberately does not preserve the archive's permission
        // bits — a mode an attacker controls is not a mode — so nothing that
        // came out of the bundle is executable yet. The app installer sets the
        // bit on the one entrypoint its manifest names; a platform release has
        // several, and they are exactly the components under `bin/`.
        //
        // After verification, never before: a file whose digest has not been
        // checked must not be made executable, even for the moment between two
        // statements.
        for component in verified.manifest().components() {
            if !component.path().starts_with("bin/") {
                continue;
            }
            let path = staging.join(component.path());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .map_err(|error| UpdateError::io(&path, error))?;
        }
        Ok(verified)
    }

    /// Takes the display back to stock and makes sure it got there.
    fn stand_down(&self) -> Result<(), UpdateError> {
        self.session
            .stand_down()
            .map_err(|reason| UpdateError::StandDown { reason })?;
        if !self.session.stock_is_up() {
            return Err(UpdateError::StandDown {
                reason: "stock is not running afterwards".to_owned(),
            });
        }
        // A wakelock still held once the session is down belongs to a process
        // that no longer exists. Release it by name, and refuse to go on if
        // that did not work: a tablet that cannot sleep is a tablet with a
        // flat battery by morning, and this is the last point at which
        // refusing costs nothing.
        if self.session.wakelock_held() {
            self.session.release_wakelock();
            if self.session.wakelock_held() {
                return Err(UpdateError::WakelockStuck);
            }
        }
        Ok(())
    }

    /// Starts whatever `current` points at and grades it.
    fn bring_up_and_grade(&self) -> Result<HealthReport, UpdateError> {
        self.session
            .bring_up()
            .map_err(|reason| UpdateError::BringUp { reason })?;
        Ok(health::watch(self.session, self.clock, self.budget))
    }

    /// The one rollback. Called at most once per transaction.
    fn roll_back(
        &self,
        record: &mut Record,
        journal: &Journal,
        candidate: &Version,
        failure: &Unhealthy,
    ) -> Result<Outcome, UpdateError> {
        journal.advance(record, Phase::Activate, format!("rolling back: {failure}"))?;

        if let Err(error) = self.stand_down() {
            journal.advance(record, Phase::Failed, format!("rollback: {error}"))?;
            return Ok(Outcome::Stranded {
                candidate: candidate.clone(),
                failure: failure.clone(),
                fallback: Some(error.to_string()),
            });
        }

        let Some(fallback) = record.from.clone() else {
            // A first install that failed. There is nothing to go back to, and
            // leaving `current` pointing at a release that does not work would
            // make the next boot's takeover fail the same way.
            self.layout.deselect(&self.layout.current())?;
            journal.advance(
                record,
                Phase::Failed,
                "the first platform release failed; nothing is selected and the device is at stock",
            )?;
            return Ok(Outcome::Stranded {
                candidate: candidate.clone(),
                failure: failure.clone(),
                fallback: None,
            });
        };

        self.layout.select(&self.layout.current(), &fallback)?;
        restore_fallback_link(self.layout, record)?;
        self.restore_snapshot(record)?;

        record.attempts += 1;
        journal.advance(record, Phase::Verify, "grading the restored release")?;
        let health = match self.bring_up_and_grade() {
            Ok(health) => health,
            Err(error) => {
                journal.advance(record, Phase::Failed, format!("rollback: {error}"))?;
                return Ok(Outcome::Stranded {
                    candidate: candidate.clone(),
                    failure: failure.clone(),
                    fallback: Some(error.to_string()),
                });
            }
        };
        record.reached = Some(health.reached.to_string());

        if health.failure().is_some() {
            // Both are unhealthy. Stop. §13 forbids the unbounded retry, and
            // this is exactly where one would be written: the fallback is the
            // known-good release and it did not come back, so trying a third
            // thing means guessing. Leave the device at stock, which is
            // reachable over SSH, and say so in the journal.
            let _ = self.session.stand_down();
            journal.advance(
                record,
                Phase::Failed,
                format!(
                    "the previous release also failed to come up: {}",
                    health.summary()
                ),
            )?;
            return Ok(Outcome::Stranded {
                candidate: candidate.clone(),
                failure: failure.clone(),
                fallback: Some(health.summary()),
            });
        }

        journal.advance(
            record,
            Phase::RolledBack,
            format!("{candidate} refused ({failure}); {fallback} is back"),
        )?;
        Ok(Outcome::RolledBack {
            candidate: candidate.clone(),
            restored: fallback,
            failure: failure.clone(),
            health,
        })
    }

    // --- state ------------------------------------------------------------

    /// Snapshots the platform's persistent state when rollback would otherwise
    /// be a swap over state the older release cannot read.
    ///
    /// Returns the snapshot path when one was needed and taken, `None` when
    /// none was needed.
    fn snapshot_for_rollback(
        &self,
        from: Option<&Version>,
        candidate: &PlatformManifest,
    ) -> Result<Option<PathBuf>, UpdateError> {
        let Some(from) = from else {
            return Ok(None);
        };
        // An outgoing release whose manifest cannot be read is not a reason to
        // refuse the upgrade — it is a reason to stop reasoning about whether
        // rollback is safe and just take the snapshot. Failing here would mean
        // a release with a damaged manifest could not be upgraded *away from*,
        // which is exactly when an upgrade is most wanted. The VM harness
        // found this: a baseline placed by hand, with no manifest, blocked
        // every upgrade case.
        let reads = match manifest::read_release(&self.layout.release_dir(from), self.keys) {
            Ok(outgoing) => {
                let reads = outgoing.manifest().state_version();
                if candidate.readable_by_state_version(reads) {
                    return Ok(None);
                }
                reads
            }
            Err(_) => 0,
        };

        let source = self.layout.platform_state();
        let destination = self.layout.snapshot_dir(from);
        let take = || -> Result<(), UpdateError> {
            if destination.exists() {
                store::remove_tree(&destination)?;
            }
            if let Some(parent) = destination.parent() {
                store::create_dir_if_missing(parent)?;
            }
            let staging = destination.with_extension("taking");
            if staging.exists() {
                store::remove_tree(&staging)?;
            }
            copy_tree(&source, &staging)?;
            store::commit_directory(&staging, &destination)?;
            Ok(())
        };
        match take() {
            Ok(()) => Ok(Some(destination)),
            Err(error) => Err(UpdateError::StateNotRollbackSafe {
                from: from.clone(),
                to: candidate.version().clone(),
                writes: candidate.rollback_to_state(),
                reads,
                reason: error.to_string(),
            }),
        }
    }

    /// Puts a snapshot back, if the transaction took one.
    fn restore_snapshot(&self, record: &Record) -> Result<bool, UpdateError> {
        restore_snapshot(self.layout, record)
    }

    // --- housekeeping -----------------------------------------------------

    /// Refuses to start a second transaction over an unreconciled first one.
    fn refuse_if_in_flight(&self) -> Result<(), UpdateError> {
        if let Some(record) = self.journal().read()?
            && record.is_in_flight()
        {
            return Err(UpdateError::InFlight {
                phase: record.phase.label(),
                version: record.to,
            });
        }
        Ok(())
    }

    /// Keeps the selected release and the fallback, and removes the rest.
    ///
    /// Two releases, not one: keeping only `current` would make rollback a
    /// download. Keeping every release ever installed would fill `/home` on a
    /// device whose whole update story depends on `/home` having room.
    fn prune(&self, current: &Version, previous: Option<&Version>) -> Result<(), UpdateError> {
        for version in self.layout.installed()? {
            if &version == current || Some(&version) == previous {
                continue;
            }
            store::remove_tree(&self.layout.release_dir(&version))?;
        }
        Ok(())
    }
}

/// Puts `previous` back to what it pointed at before the transaction.
///
/// Without this a rolled-back transaction leaves `previous` naming the release
/// that is now `current`, and the next rollback is a no-op that stands the
/// session down and back up for nothing.
fn restore_fallback_link(layout: &PlatformLayout, record: &Record) -> Result<(), UpdateError> {
    match &record.previous_before {
        Some(version) if layout.release_dir(version).is_dir() => {
            layout.select(&layout.previous(), version)
        }
        // Either there was no fallback before, or the release it named has
        // since been pruned. Naming nothing is honest; naming a directory that
        // is not there is not.
        _ => layout.deselect(&layout.previous()),
    }
}

/// Puts a platform-state snapshot back over the live state, durably.
///
/// Shared by [`Upgrade`] and [`Maintenance`]: the same restore has to happen
/// whether the rollback was decided by a failed health check or by a reboot
/// that reconcile found afterwards.
fn restore_snapshot(layout: &PlatformLayout, record: &Record) -> Result<bool, UpdateError> {
    let Some(snapshot) = &record.snapshot else {
        return Ok(false);
    };
    if !snapshot.is_dir() {
        return Ok(false);
    }
    let live = layout.platform_state();
    let staging = live.with_extension("restoring");
    if staging.exists() {
        store::remove_tree(&staging)?;
    }
    copy_tree(snapshot, &staging)?;
    if live.exists() {
        store::remove_tree(&live)?;
    }
    store::commit_directory(&staging, &live)?;
    Ok(true)
}

/// Copies a directory tree, creating `destination`.
///
/// Regular files and directories only. A snapshot of platform state that
/// carried a symlink out of the tree would restore something the updater never
/// captured.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), UpdateError> {
    fs::create_dir_all(destination).map_err(|error| UpdateError::io(destination, error))?;
    if !source.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(source).map_err(|error| UpdateError::io(source, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| UpdateError::io(source, error))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| UpdateError::io(&from, error))?;
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to).map_err(|error| UpdateError::io(&from, error))?;
        }
    }
    Ok(())
}

/// The domain a platform manifest is signed under, re-exported so a caller
/// building one does not have to reach into `paper_packages` to find it.
pub const PLATFORM_DOMAIN: Domain = Domain::PLATFORM;
