//! What the platform update transaction decides, proved where there is no
//! systemd (§13).
//!
//! # What this file is evidence of, and what it is not
//!
//! It is evidence that the transaction is **right**: that a candidate which
//! stalls below `ready` is rolled back rather than committed, that an
//! interrupted update reverts rather than resumes, that a bundle signed for
//! the wrong domain is refused, that nothing retries twice.
//!
//! It is **not** evidence that any of it works on a tablet. Every session
//! operation here goes through a [`FakeSession`] that answers from a script.
//! A candidate that "stalls at `device-adapter`" stalls because this file said
//! so, not because a display failed to come up. Only
//! `tests/failure-harness`, run in a Linux VM with systemd, says the detection
//! is real — and only the device says the device is real. The project's
//! standing rule holds here as everywhere: passing local tests is not
//! qualification.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use paper_host::readiness::Rung;
use paper_packages::signing::{Domain, SecretKey, TrustedKeys};
use paper_packages::store::Layout;
use paper_protocol::ProtocolVersion;
use paper_testing::FakeClock;
use paper_updater::error::UpdateError;
use paper_updater::health::{Budget, Observation, SessionControl};
use paper_updater::journal::{Journal, Phase};
use paper_updater::layout::PlatformLayout;
use paper_updater::manifest::ComponentPolicy;
use paper_updater::remove::{AppData, plan};
use paper_updater::upgrade::{Outcome, Reconciled, Upgrade};
use paper_updater::{PlatformManifest, bundle};
use semver::Version;

// --- fakes ------------------------------------------------------------------
//
// `FakeClock` moved to `platform/testing` (WWW-46) — it was the shape every
// other crate's clock fake should have followed from the start. Everything
// below it is `SessionControl`, which stays here: it is a domain-specific
// composition of a unit, a wakelock and a readiness observation, not one of
// `platform/sys`'s four general effects.

/// How a release behaves when it is brought up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Climbs all the way.
    Ready,
    /// Climbs to `rung` and stops there, staying alive.
    StallsAt(Rung),
    /// Climbs to `rung`, then the process is gone.
    Dies(Rung),
}

/// A session that answers from a script.
#[derive(Debug)]
struct FakeSession {
    layout: PlatformLayout,
    behaviour: Mutex<HashMap<String, Behaviour>>,
    running: Mutex<Option<String>>,
    polls: Mutex<u32>,
    stock: Mutex<bool>,
    wakelock: Mutex<bool>,
    wakelock_stuck: Mutex<bool>,
    stand_down_fails: Mutex<bool>,
    /// What a release writes into the platform state directory when it comes
    /// up, keyed by version. This is what makes the snapshot test mean
    /// something: without it, "the state was restored" is indistinguishable
    /// from "nothing ever touched the state".
    state_writes: Mutex<HashMap<String, Vec<u8>>>,
    log: Mutex<Vec<String>>,
}

impl FakeSession {
    fn new(layout: &PlatformLayout) -> Self {
        Self {
            layout: layout.clone(),
            behaviour: Mutex::new(HashMap::new()),
            running: Mutex::new(None),
            polls: Mutex::new(0),
            stock: Mutex::new(true),
            wakelock: Mutex::new(false),
            wakelock_stuck: Mutex::new(false),
            stand_down_fails: Mutex::new(false),
            state_writes: Mutex::new(HashMap::new()),
            log: Mutex::new(Vec::new()),
        }
    }

    fn script(&self, version: &str, behaviour: Behaviour) {
        self.behaviour
            .lock()
            .expect("behaviour")
            .insert(version.to_owned(), behaviour);
    }

    fn writes_state(&self, version: &str, bytes: &[u8]) {
        self.state_writes
            .lock()
            .expect("state writes")
            .insert(version.to_owned(), bytes.to_vec());
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().expect("log").clone()
    }

    fn count(&self, what: &str) -> usize {
        self.log().iter().filter(|line| *line == what).count()
    }
}

impl SessionControl for FakeSession {
    fn stand_down(&self) -> Result<(), String> {
        self.log.lock().expect("log").push("stand-down".to_owned());
        if *self.stand_down_fails.lock().expect("stand-down") {
            return Err("stock refused to start".to_owned());
        }
        *self.running.lock().expect("running") = None;
        *self.polls.lock().expect("polls") = 0;
        *self.stock.lock().expect("stock") = true;
        Ok(())
    }

    fn bring_up(&self) -> Result<(), String> {
        self.log.lock().expect("log").push("bring-up".to_owned());
        let selected = self
            .layout
            .selected()
            .expect("a readable selection")
            .map(|version| version.to_string());
        if let Some(version) = &selected
            && let Some(bytes) = self.state_writes.lock().expect("state writes").get(version)
        {
            let _ = std::fs::create_dir_all(self.layout.platform_state());
            let _ = std::fs::write(self.layout.platform_state().join("home.json"), bytes);
        }
        *self.running.lock().expect("running") = selected;
        *self.polls.lock().expect("polls") = 0;
        *self.stock.lock().expect("stock") = false;
        Ok(())
    }

    fn observe(&self) -> Observation {
        let running = self.running.lock().expect("running").clone();
        let Some(running) = running else {
            return Observation::absent();
        };
        let behaviour = self
            .behaviour
            .lock()
            .expect("behaviour")
            .get(&running)
            .copied()
            .unwrap_or(Behaviour::Ready);
        let mut polls = self.polls.lock().expect("polls");
        *polls += 1;
        let polled = *polls;
        drop(polls);

        match behaviour {
            Behaviour::Ready => Observation {
                alive: true,
                reached: Some(Rung::Ready),
                protocol: Some(ProtocolVersion::new(1, 0)),
                note: String::new(),
            },
            Behaviour::StallsAt(rung) => Observation {
                alive: true,
                reached: Some(rung),
                protocol: Some(ProtocolVersion::new(1, 0)),
                note: format!("scripted stall at {rung}"),
            },
            // Alive for one poll so the watch has seen it, gone afterwards.
            Behaviour::Dies(rung) if polled <= 1 => Observation {
                alive: true,
                reached: Some(rung),
                protocol: Some(ProtocolVersion::new(1, 0)),
                note: String::new(),
            },
            Behaviour::Dies(rung) => Observation {
                alive: false,
                reached: Some(rung),
                protocol: None,
                note: "scripted death".to_owned(),
            },
        }
    }

    fn stock_is_up(&self) -> bool {
        *self.stock.lock().expect("stock")
    }

    fn wakelock_held(&self) -> bool {
        *self.wakelock.lock().expect("wakelock")
    }

    fn release_wakelock(&self) {
        self.log
            .lock()
            .expect("log")
            .push("release-wakelock".to_owned());
        if !*self.wakelock_stuck.lock().expect("stuck") {
            *self.wakelock.lock().expect("wakelock") = false;
        }
    }
}

// --- fixture ----------------------------------------------------------------

/// A platform root, a key, and a way to make bundles for it.
struct Fixture {
    _temp: tempfile::TempDir,
    layout: PlatformLayout,
    keys: TrustedKeys,
    secret: SecretKey,
    bundles: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("a temp directory");
        let layout = PlatformLayout::new(temp.path().join("home/root/paperclip"));
        layout.ensure().expect("an established root");
        let secret = SecretKey::generate().expect("a key");
        let mut keys = TrustedKeys::none();
        keys.trust(secret.public_key());
        let bundles = temp.path().join("bundles");
        std::fs::create_dir_all(&bundles).expect("a bundle directory");
        Self {
            _temp: temp,
            layout,
            keys,
            secret,
            bundles,
        }
    }

    /// Builds a signed bundle for `version`.
    fn bundle(&self, version: &str) -> PathBuf {
        self.bundle_with(version, 1, 1, Domain::PLATFORM, |_| {})
    }

    /// Builds a bundle, with the state versions, the signing domain and a hook
    /// that may corrupt the staging tree before it is packed.
    fn bundle_with(
        &self,
        version: &str,
        state_version: u32,
        rollback_to_state: u32,
        domain: Domain,
        tamper: impl FnOnce(&Path),
    ) -> PathBuf {
        let source = self.bundles.join(format!("src-{version}-{state_version}"));
        let bin = source.join("bin");
        std::fs::create_dir_all(&bin).expect("a source tree");
        for name in [
            "paperclip-host",
            "paperclip-compositor",
            "home",
            "app-store",
            "settings",
        ] {
            std::fs::write(
                bin.join(name),
                elf(0xB7, format!("{name} {version}").as_bytes()),
            )
            .expect("a component");
        }
        tamper(&source);

        let manifest = bundle::describe(
            &source,
            &description(
                Version::parse(version).expect("a version"),
                state_version,
                rollback_to_state,
                &[],
            ),
        )
        .expect("a manifest");
        self.pack(&source, &manifest, domain, version, state_version)
    }

    fn pack(
        &self,
        source: &Path,
        manifest: &PlatformManifest,
        domain: Domain,
        version: &str,
        state_version: u32,
    ) -> PathBuf {
        let document = manifest.to_document();
        let signature = self.secret.sign(domain, document.as_bytes());
        let path = self
            .bundles
            .join(format!("paperclip-{version}-{state_version}.tar.gz"));
        let file = std::fs::File::create(&path).expect("a bundle file");
        bundle::build(source, manifest, document.as_bytes(), &signature, file)
            .expect("a built bundle");
        path
    }

    fn upgrade<'a>(&'a self, session: &'a FakeSession, clock: &'a FakeClock) -> Upgrade<'a> {
        Upgrade::new(&self.layout, session, clock, &self.keys)
            // Tests grade the transaction, not the binaries; the synthetic
            // components here are aarch64 headers with no code behind them.
            // `for_this_machine` is what the updater binary uses.
            .with_component_policy(ComponentPolicy::any_machine())
            .with_budget(Budget {
                deadline: Duration::from_secs(30),
                poll: Duration::from_millis(250),
            })
    }

    fn journal(&self) -> Journal {
        Journal::new(self.layout.journal_file())
    }
}

/// The description every bundle in this file is built from, varying only in
/// the things a test is actually about.
fn description<'a>(
    version: Version,
    state_version: u32,
    rollback_to_state: u32,
    extras: &'a [&'a str],
) -> bundle::Description<'a> {
    bundle::Description {
        version,
        protocol: ProtocolVersion::new(1, 0),
        published: 1_758_000_000,
        state_version,
        rollback_to_state,
        notes: String::new(),
        extras,
    }
}

/// A synthetic ELF64 little-endian executable header for `machine`, with
/// `filler` behind it so two components differ in their digests.
fn elf(machine: u16, filler: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(filler);
    bytes
}

/// Installs `version` as the selected release, the way a first install would.
fn install_first(fixture: &Fixture, session: &FakeSession, version: &str) {
    let clock = FakeClock::new();
    session.script(version, Behaviour::Ready);
    let bundle = fixture.bundle(version);
    let outcome = fixture
        .upgrade(session, &clock)
        .run(&bundle)
        .expect("the first install to succeed");
    assert!(matches!(outcome, Outcome::Upgraded { .. }), "{outcome:?}");
}

// --- the happy path ---------------------------------------------------------

#[test]
fn a_healthy_candidate_is_committed_and_the_old_release_becomes_the_fallback() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    session.script("0.4.0", Behaviour::Ready);
    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("an upgrade");

    match outcome {
        Outcome::Upgraded { from, to, health } => {
            assert_eq!(from, Some(Version::new(0, 3, 1)));
            assert_eq!(to, Version::new(0, 4, 0));
            assert!(health.healthy);
            assert_eq!(health.reached, Rung::Ready);
        }
        other => panic!("expected an upgrade, got {other:?}"),
    }

    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 4, 0))
    );
    assert_eq!(
        fixture.layout.fallback().unwrap(),
        Some(Version::new(0, 3, 1))
    );
    assert_eq!(
        fixture.journal().read().unwrap().map(|record| record.phase),
        Some(Phase::Commit)
    );
}

#[test]
fn the_committed_release_is_executable() {
    // Found by the VM harness, which is the only place it could be: a Mac test
    // that never execs anything cannot notice that the whole platform landed
    // on disk mode 0644. `systemd` noticed, with `203/EXEC`.
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    let release = fixture.layout.release_dir(&Version::new(0, 3, 1));
    for name in [
        "paperclip-host",
        "paperclip-compositor",
        "home",
        "app-store",
        "settings",
    ] {
        let path = release.join("bin").join(name);
        let mode = std::fs::metadata(&path)
            .expect("a committed component")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "{} is mode {mode:o}; systemd cannot exec it",
            path.display()
        );
    }
}

#[test]
fn the_display_goes_back_to_stock_before_the_selection_moves() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");
    session.script("0.4.0", Behaviour::Ready);

    let clock = FakeClock::new();
    fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("an upgrade");

    // Every `bring-up` is preceded by a `stand-down`. §13's sequence is
    // stand down, swap, bring up — never swap under a live session.
    let log = session.log();
    let first_bring_up = log.iter().position(|line| line == "bring-up");
    let first_stand_down = log.iter().position(|line| line == "stand-down");
    assert!(
        first_stand_down < first_bring_up,
        "stood up before standing down: {log:?}"
    );
}

#[test]
fn only_the_selected_release_and_the_fallback_are_kept() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.1.0");
    for version in ["0.2.0", "0.3.0"] {
        session.script(version, Behaviour::Ready);
        let clock = FakeClock::new();
        fixture
            .upgrade(&session, &clock)
            .run(&fixture.bundle(version))
            .expect("an upgrade");
    }

    assert_eq!(
        fixture.layout.installed().unwrap(),
        vec![Version::new(0, 2, 0), Version::new(0, 3, 0)],
        "0.1.0 should have been pruned; keeping every release ever installed fills /home"
    );
}

// --- rollback ---------------------------------------------------------------

#[test]
fn a_candidate_that_stalls_below_ready_is_rolled_back() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    session.script("0.4.0", Behaviour::StallsAt(Rung::DeviceAdapter));
    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("a completed transaction");

    match outcome {
        Outcome::RolledBack {
            candidate,
            restored,
            failure,
            health,
        } => {
            assert_eq!(candidate, Version::new(0, 4, 0));
            assert_eq!(restored, Version::new(0, 3, 1));
            assert_eq!(failure.reached, Rung::DeviceAdapter);
            assert_eq!(
                failure.stalled_at,
                Rung::Home,
                "the report must name the rung it never reached, not just `not ready`"
            );
            assert!(health.healthy, "the restored release came back");
        }
        other => panic!("expected a rollback, got {other:?}"),
    }

    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
    assert_eq!(
        fixture.journal().read().unwrap().map(|record| record.phase),
        Some(Phase::RolledBack)
    );
}

#[test]
fn a_candidate_that_starts_and_then_exits_is_rolled_back() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    session.script("0.4.0", Behaviour::Dies(Rung::Control));
    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("a completed transaction");

    match outcome {
        Outcome::RolledBack { failure, .. } => {
            assert!(failure.died, "the report must say the supervisor exited");
            assert_eq!(failure.reached, Rung::Control);
        }
        other => panic!("expected a rollback, got {other:?}"),
    }
}

#[test]
fn a_release_that_starts_but_never_climbs_is_not_treated_as_healthy() {
    // The whole point of §13's "process startup alone is not health evidence".
    // This candidate is alive for the entire deadline and never crashes.
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    session.script("0.4.0", Behaviour::StallsAt(Rung::Process));
    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("a completed transaction");

    assert!(matches!(outcome, Outcome::RolledBack { .. }), "{outcome:?}");
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
}

#[test]
fn when_the_fallback_also_fails_the_device_is_left_at_stock_and_nothing_retries() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    // The old release stops working too — a shared file it depended on is
    // gone, say. Both are now unhealthy.
    session.script("0.3.1", Behaviour::StallsAt(Rung::Protocol));
    session.script("0.4.0", Behaviour::StallsAt(Rung::Control));
    let before = session.count("bring-up");

    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("a completed transaction");

    match outcome {
        Outcome::Stranded {
            candidate,
            fallback,
            ..
        } => {
            assert_eq!(candidate, Version::new(0, 4, 0));
            assert!(
                fallback.is_some(),
                "the report must say the fallback was tried"
            );
        }
        other => panic!("expected the device to be left at stock, got {other:?}"),
    }

    assert_eq!(
        session.count("bring-up") - before,
        2,
        "one attempt for the candidate and one for the fallback; §13 forbids the loop"
    );
    assert!(session.stock_is_up(), "the device must be left at stock");
    assert_eq!(
        fixture.journal().read().unwrap().map(|record| record.phase),
        Some(Phase::Failed)
    );
}

#[test]
fn a_first_release_that_fails_leaves_nothing_selected() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    session.script("0.1.0", Behaviour::StallsAt(Rung::Control));

    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.1.0"))
        .expect("a completed transaction");

    assert!(
        matches!(outcome, Outcome::Stranded { fallback: None, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        fixture.layout.selected().unwrap(),
        None,
        "a `current` pointing at a release that does not work would fail the same way next time"
    );
    assert!(session.stock_is_up());
}

// --- refusals, before anything moves ----------------------------------------

#[test]
fn a_bundle_signed_by_a_stranger_is_refused_and_nothing_is_selected() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    let stranger = SecretKey::generate().expect("a key");
    let source = fixture.bundles.join("src-stranger");
    std::fs::create_dir_all(source.join("bin")).expect("a source tree");
    for name in [
        "paperclip-host",
        "paperclip-compositor",
        "home",
        "app-store",
        "settings",
    ] {
        std::fs::write(source.join("bin").join(name), elf(0xB7, name.as_bytes()))
            .expect("a component");
    }
    let manifest = bundle::describe(&source, &description(Version::new(0, 4, 0), 1, 1, &[]))
        .expect("a manifest");
    let document = manifest.to_document();
    let signature = stranger.sign(Domain::PLATFORM, document.as_bytes());
    let path = fixture.bundles.join("stranger.tar.gz");
    bundle::build(
        &source,
        &manifest,
        document.as_bytes(),
        &signature,
        std::fs::File::create(&path).expect("a file"),
    )
    .expect("a bundle");

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&path)
        .expect_err("a bundle signed by an untrusted key must be refused");
    assert!(matches!(error, UpdateError::Signature(_)), "{error:?}");
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
    assert_eq!(
        session.count("stand-down"),
        1,
        "only the first install stood down"
    );
}

#[test]
fn a_document_signed_as_an_app_release_does_not_verify_as_a_platform() {
    // §13: routine app installation can never replace the host. One half of
    // making that structural rather than conventional is the domain
    // separator — a signature made over an app release does not verify here,
    // even from a trusted key, even over identical bytes.
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    let bundle = fixture.bundle_with("0.4.0", 1, 1, Domain::RELEASE, |_| {});

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&bundle)
        .expect_err("the app-release domain must not verify as a platform manifest");
    assert!(matches!(error, UpdateError::Signature(_)), "{error:?}");
}

#[test]
fn a_component_that_does_not_match_the_signed_manifest_is_refused() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    // Build the manifest from honest bytes, then swap one component for
    // something else before packing. The signature is valid; the payload is
    // not what it covers.
    let source = fixture.bundles.join("src-swapped");
    std::fs::create_dir_all(source.join("bin")).expect("a source tree");
    for name in [
        "paperclip-host",
        "paperclip-compositor",
        "home",
        "app-store",
        "settings",
    ] {
        std::fs::write(source.join("bin").join(name), elf(0xB7, name.as_bytes()))
            .expect("a component");
    }
    let manifest = bundle::describe(&source, &description(Version::new(0, 4, 0), 1, 1, &[]))
        .expect("a manifest");
    std::fs::write(
        source.join("bin/paperclip-host"),
        elf(0xB7, b"a different supervisor entirely"),
    )
    .expect("the swap");

    let document = manifest.to_document();
    let signature = fixture.secret.sign(Domain::PLATFORM, document.as_bytes());
    let path = fixture.bundles.join("swapped.tar.gz");
    bundle::build(
        &source,
        &manifest,
        document.as_bytes(),
        &signature,
        std::fs::File::create(&path).expect("a file"),
    )
    .expect("a bundle");

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&path)
        .expect_err("a payload that disagrees with the signed manifest must be refused");
    assert!(
        matches!(&error, UpdateError::Component { name, .. } if name == "paperclip-host"),
        "{error:?}"
    );
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
}

#[test]
fn a_file_in_the_bundle_that_the_manifest_does_not_name_is_refused() {
    // The signature covers the manifest, and the manifest covers the files it
    // lists. A file it does not list is a file nobody signed for. The attack
    // is a bundle whose *contents* carry one more thing than the document that
    // was signed.
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    let source = fixture.bundles.join("src-stowaway");
    std::fs::create_dir_all(source.join("bin")).expect("a source tree");
    for name in [
        "paperclip-host",
        "paperclip-compositor",
        "home",
        "app-store",
        "settings",
    ] {
        std::fs::write(source.join("bin").join(name), elf(0xB7, name.as_bytes()))
            .expect("a component");
    }
    std::fs::write(source.join("bin/extra"), elf(0xB7, b"unlisted")).expect("a stowaway");

    let describe = |extras: &[&str]| {
        bundle::describe(&source, &description(Version::new(0, 4, 0), 1, 1, extras))
            .expect("a manifest")
    };
    // Signed without the stowaway; packed with it.
    let signed = describe(&[]);
    let packed = describe(&["bin/extra"]);
    let document = signed.to_document();
    let signature = fixture.secret.sign(Domain::PLATFORM, document.as_bytes());
    let path = fixture.bundles.join("stowaway.tar.gz");
    bundle::build(
        &source,
        &packed,
        document.as_bytes(),
        &signature,
        std::fs::File::create(&path).expect("a file"),
    )
    .expect("a bundle");

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&path)
        .expect_err("a file the signed manifest does not name must be refused");
    assert!(
        matches!(&error, UpdateError::Component { name, .. } if name == "bin/extra"),
        "{error:?}"
    );
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
}

#[test]
fn a_manifest_missing_a_required_component_is_refused() {
    let source = tempfile::tempdir().expect("a temp directory");
    std::fs::create_dir_all(source.path().join("bin")).expect("a source tree");
    for name in ["paperclip-host", "home", "app-store"] {
        std::fs::write(
            source.path().join("bin").join(name),
            elf(0xB7, name.as_bytes()),
        )
        .expect("a component");
    }
    let error = bundle::describe(
        source.path(),
        &description(Version::new(0, 4, 0), 1, 1, &[]),
    )
    .expect_err("a platform release missing a required component is not a platform release");
    assert!(matches!(error, UpdateError::Component { .. }), "{error:?}");
}

#[test]
fn a_manifest_that_cannot_read_its_own_writes_is_refused() {
    let error = PlatformManifest::new(
        Version::new(0, 4, 0),
        ProtocolVersion::new(1, 0),
        1,
        1,
        2,
        "",
        Vec::new(),
    )
    .expect_err("rollback_to_state above state_version is nonsense");
    assert!(matches!(error, UpdateError::Manifest { .. }), "{error:?}");
}

#[test]
fn installing_the_version_that_is_already_selected_is_refused() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.3.1"))
        .expect_err("re-selecting the running release is not an upgrade");
    assert!(
        matches!(error, UpdateError::AlreadySelected { .. }),
        "{error:?}"
    );
}

#[test]
fn a_wakelock_that_cannot_be_released_stops_the_upgrade_before_the_swap() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    *session.wakelock.lock().expect("wakelock") = true;
    *session.wakelock_stuck.lock().expect("stuck") = true;
    session.script("0.4.0", Behaviour::Ready);

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect_err("a wakelock held by a process that is gone must stop the upgrade");
    assert!(matches!(error, UpdateError::WakelockStuck), "{error:?}");
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1)),
        "nothing may have been swapped"
    );
}

#[test]
fn a_released_wakelock_lets_the_upgrade_continue() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    *session.wakelock.lock().expect("wakelock") = true;
    session.script("0.4.0", Behaviour::Ready);

    let clock = FakeClock::new();
    fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("an upgrade");
    assert!(session.count("release-wakelock") >= 1);
    assert!(!session.wakelock_held());
}

#[test]
fn a_session_that_will_not_stand_down_stops_the_upgrade_before_the_swap() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");
    *session.stand_down_fails.lock().expect("stand-down") = true;
    session.script("0.4.0", Behaviour::Ready);

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect_err(
            "swapping under a session that still owns the panel is the one thing not allowed",
        );
    assert!(matches!(error, UpdateError::StandDown { .. }), "{error:?}");
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
}

// --- interruption -----------------------------------------------------------

#[test]
fn reconcile_does_nothing_when_nothing_was_interrupted() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    let clock = FakeClock::new();
    assert_eq!(
        fixture.upgrade(&session, &clock).reconcile().unwrap(),
        Reconciled::Nothing
    );
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
}

#[test]
fn an_interruption_between_activate_and_commit_reverts_rather_than_resumes() {
    // The power-cut case. The journal says `verify`; `current` has already
    // moved. §13's answer is that an update which never committed does not get
    // to stay selected, because nothing has ever shown the candidate to be
    // healthy.
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");
    session.script("0.4.0", Behaviour::Ready);

    // Stage the candidate for real, then forge the interruption: `current`
    // moved, the journal is at `verify`, nothing committed.
    let clock = FakeClock::new();
    let upgrade = fixture.upgrade(&session, &clock);
    let staged = fixture.bundle("0.4.0");
    upgrade.run(&staged).expect("an upgrade");
    fixture
        .layout
        .select(&fixture.layout.current(), &Version::new(0, 4, 0))
        .expect("a selection");
    let mut record = fixture.journal().read().unwrap().expect("a record");
    record.phase = Phase::Verify;
    record.attempts = 1;
    fixture
        .journal()
        .record(&mut record)
        .expect("a journal write");

    let reconciled = upgrade.reconcile().expect("a reconcile");
    assert_eq!(
        reconciled,
        Reconciled::Reverted {
            candidate: Version::new(0, 4, 0),
            restored: Some(Version::new(0, 3, 1)),
            state_restored: false,
        }
    );
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
    assert_eq!(
        fixture.journal().read().unwrap().map(|record| record.phase),
        Some(Phase::RolledBack)
    );
}

#[test]
fn an_interruption_while_staging_discards_the_candidate() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    // Forge a `prepare` record naming a staged candidate that was committed to
    // `releases/` but never selected.
    let candidate = Version::new(0, 4, 0);
    let release = fixture.layout.release_dir(&candidate);
    std::fs::create_dir_all(release.join("bin")).expect("a release directory");
    let staging = fixture.layout.staging_dir().join("interrupted");
    std::fs::create_dir_all(&staging).expect("a staging directory");
    let mut record = paper_updater::Record::opening(Some(Version::new(0, 3, 1)), candidate.clone());
    record.staged = Some(staging.clone());
    fixture
        .journal()
        .record(&mut record)
        .expect("a journal write");

    let clock = FakeClock::new();
    let reconciled = fixture
        .upgrade(&session, &clock)
        .reconcile()
        .expect("a reconcile");
    assert_eq!(reconciled, Reconciled::DiscardedStaging { candidate });
    assert!(!staging.exists(), "the staging directory must be gone");
    assert!(!release.exists(), "an uncommitted release is not a release");
    assert_eq!(
        fixture.layout.selected().unwrap(),
        Some(Version::new(0, 3, 1))
    );
}

#[test]
fn a_second_upgrade_over_an_unreconciled_one_is_refused() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    let mut record =
        paper_updater::Record::opening(Some(Version::new(0, 3, 1)), Version::new(0, 4, 0));
    record.phase = Phase::Verify;
    fixture
        .journal()
        .record(&mut record)
        .expect("a journal write");

    let clock = FakeClock::new();
    let error = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.5.0"))
        .expect_err("starting a second transaction over an in-flight one must be refused");
    assert!(matches!(error, UpdateError::InFlight { .. }), "{error:?}");
}

// --- state migration --------------------------------------------------------

#[test]
fn a_rollback_over_state_the_older_release_cannot_read_restores_a_snapshot() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");

    // What 0.3.1 wrote.
    let state = fixture.layout.platform_state();
    std::fs::write(state.join("home.json"), b"v1 state").expect("some state");

    // 0.4.0 writes state version 2, and says nothing below 2 can read it. It
    // gets far enough to rewrite the state before it stalls, which is exactly
    // the situation a swap-the-executables rollback would leave behind.
    session.script("0.4.0", Behaviour::StallsAt(Rung::Home));
    session.writes_state("0.4.0", b"v2 state 0.3.1 cannot parse");
    let bundle = fixture.bundle_with("0.4.0", 2, 2, Domain::PLATFORM, |_| {});

    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&bundle)
        .expect("a completed transaction");
    assert!(matches!(outcome, Outcome::RolledBack { .. }), "{outcome:?}");

    // The candidate had a chance to write state 2 over it; the snapshot is
    // what makes the rollback a rollback rather than a swap over bytes 0.3.1
    // cannot parse.
    assert!(
        fixture.layout.snapshot_dir(&Version::new(0, 3, 1)).is_dir(),
        "a snapshot should have been taken before activating"
    );
    assert_eq!(
        std::fs::read(state.join("home.json")).unwrap(),
        b"v1 state",
        "the restored state must be what the restored release wrote"
    );
}

#[test]
fn no_snapshot_is_taken_when_the_older_release_can_read_the_newer_ones_writes() {
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");
    session.script("0.4.0", Behaviour::Ready);

    // state_version 2, but readable back to 1 — a backward-compatible change.
    let bundle = fixture.bundle_with("0.4.0", 2, 1, Domain::PLATFORM, |_| {});
    let clock = FakeClock::new();
    fixture
        .upgrade(&session, &clock)
        .run(&bundle)
        .expect("an upgrade");

    assert!(
        !fixture.layout.snapshot_dir(&Version::new(0, 3, 1)).exists(),
        "copying state nobody needs copied is a cost with no buyer"
    );
}

#[test]
fn a_release_with_an_unreadable_manifest_can_still_be_upgraded_away_from() {
    // Found by the VM harness. Refusing here would mean a release with a
    // damaged manifest could not be upgraded *away from*, which is exactly
    // when an upgrade is most wanted. The safe answer is to stop reasoning
    // about whether rollback is state-safe and take the snapshot anyway.
    let fixture = Fixture::new();
    let session = FakeSession::new(&fixture.layout);
    install_first(&fixture, &session, "0.3.1");
    std::fs::remove_file(
        fixture
            .layout
            .release_dir(&Version::new(0, 3, 1))
            .join("platform.toml.sig"),
    )
    .expect("the signature to remove");

    session.script("0.4.0", Behaviour::Ready);
    let clock = FakeClock::new();
    let outcome = fixture
        .upgrade(&session, &clock)
        .run(&fixture.bundle("0.4.0"))
        .expect("an upgrade away from a damaged release");
    assert!(matches!(outcome, Outcome::Upgraded { .. }), "{outcome:?}");
    assert!(
        fixture.layout.snapshot_dir(&Version::new(0, 3, 1)).is_dir(),
        "with no readable state version, the snapshot is the only safe assumption"
    );
}

// --- containment ------------------------------------------------------------

#[test]
fn the_platform_root_and_the_app_store_root_must_not_overlap() {
    let platform = PlatformLayout::new("/home/root/paperclip");
    assert!(
        platform
            .separate_from(&Layout::new("/home/root/.local/share/paperclip"))
            .is_ok(),
        "the device's two roots are disjoint"
    );
    assert!(
        platform
            .separate_from(&Layout::new("/home/root/paperclip/apps"))
            .is_err(),
        "an app store inside the platform root would let an app install reach the host"
    );
    assert!(
        platform.separate_from(&Layout::new("/home/root")).is_err(),
        "a platform root inside the app store root is the same hole the other way up"
    );
}

#[test]
fn no_app_id_names_a_path_inside_the_platform_root() {
    // The structural half of "routine app installation can never replace the
    // host". Not a rule the installer follows — a place it cannot reach.
    let platform = PlatformLayout::new("/home/root/paperclip");
    let store = Layout::new(paper_packages::store::DEVICE_ROOT);
    platform.separate_from(&store).expect("disjoint roots");

    for id in [
        "dev.calum.chess",
        "paperclip-host",
        "home",
        "current",
        "bin",
    ] {
        let Ok(app) = id.parse::<paper_packages::AppId>() else {
            continue;
        };
        let release = store.release_dir(&app, &Version::new(1, 0, 0));
        assert!(
            !release.starts_with(platform.root()),
            "installing `{id}` would write into the platform root at {}",
            release.display()
        );
    }
}

// --- removal ----------------------------------------------------------------

#[test]
fn removal_keeps_app_data_and_never_names_a_notebook() {
    let platform = PlatformLayout::new("/home/root/paperclip");
    let store = Layout::new("/home/root/.local/share/paperclip");
    let removal = plan(&platform, &store, AppData::Keep);

    for path in &removal.removes {
        assert!(
            path.starts_with(platform.root()) || path.starts_with(store.root()),
            "{} is outside both Paperclip roots",
            path.display()
        );
        assert!(
            !path.starts_with("/home/root/.local/share/remarkable"),
            "{} is Xochitl's",
            path.display()
        );
    }
    assert!(
        removal.keeps.iter().any(|(path, _)| path.ends_with("data")),
        "app data is kept unless removal is asked for it explicitly"
    );
}

#[test]
fn removal_can_be_asked_for_app_data_explicitly() {
    let platform = PlatformLayout::new("/home/root/paperclip");
    let store = Layout::new("/home/root/.local/share/paperclip");
    let removal = plan(&platform, &store, AppData::Remove);
    assert!(removal.removes.iter().any(|path| path.ends_with("data")));
}

#[test]
fn removal_refuses_a_directory_that_is_not_a_paperclip_root() {
    let temp = tempfile::tempdir().expect("a temp directory");
    let platform = PlatformLayout::new(temp.path().join("not-paperclip"));
    std::fs::create_dir_all(platform.root()).expect("a directory");
    let keepsake = platform.root().join("something-precious");
    std::fs::write(&keepsake, b"do not delete me").expect("a file");

    let store = Layout::new(temp.path().join("store"));
    let error = plan(&platform, &store, AppData::Keep)
        .execute()
        .expect_err("a root without the marker must be refused");
    assert!(
        matches!(error, UpdateError::NotAPaperclipRoot { .. }),
        "{error:?}"
    );
    assert!(keepsake.exists());
}

#[test]
fn removal_deletes_the_platform_tree_and_leaves_app_data() {
    let temp = tempfile::tempdir().expect("a temp directory");
    let platform = PlatformLayout::new(temp.path().join("paperclip"));
    platform.ensure().expect("an established root");
    std::fs::create_dir_all(platform.release_dir(&Version::new(0, 3, 1))).expect("a release");

    let store = Layout::new(temp.path().join("store"));
    std::fs::create_dir_all(store.root().join("data/dev.calum.chess")).expect("app data");
    let save = store.root().join("data/dev.calum.chess/game.json");
    std::fs::write(&save, b"a half-finished game").expect("a save");

    plan(&platform, &store, AppData::Keep)
        .execute()
        .expect("a removal");

    assert!(!platform.releases_dir().exists());
    assert!(!platform.root().join(".paperclip-platform").exists());
    assert!(
        save.exists(),
        "§14: app data is not deleted unintentionally"
    );
}

// --- the journal ------------------------------------------------------------

#[test]
fn an_unreadable_journal_is_an_error_rather_than_an_assumption_that_nothing_happened() {
    let temp = tempfile::tempdir().expect("a temp directory");
    let path = temp.path().join("update.json");
    let mut file = std::fs::File::create(&path).expect("a file");
    file.write_all(b"{ this is not a record").expect("bytes");
    drop(file);

    let error = Journal::new(&path)
        .read()
        .expect_err("guessing is worst exactly here");
    assert!(
        matches!(error, UpdateError::CorruptJournal { .. }),
        "{error:?}"
    );
}

#[test]
fn the_climb_deadline_is_under_the_supervisors_watchdog() {
    // A deadline at or above `HOST_WATCHDOG` means systemd kills a stalled
    // candidate — for not petting a watchdog it never reached the main loop to
    // pet — at the same moment the updater is grading it. The rollback happens
    // either way; the *reason* is what gets lost, and the reason is the useful
    // half of the report.
    assert!(
        paper_updater::health::CLIMB_DEADLINE < paper_host::units::HOST_WATCHDOG,
        "the updater must decide, not systemd: {:?} vs {:?}",
        paper_updater::health::CLIMB_DEADLINE,
        paper_host::units::HOST_WATCHDOG
    );
}
