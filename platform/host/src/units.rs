//! Generating the runtime systemd units (WWW-11, §11).
//!
//! Paperclip installs nothing on the root filesystem. Units are written into
//! `/run/systemd/system` by `paperctl session start`, and `/run` is a tmpfs —
//! so the next boot has never heard of Paperclip and comes up as stock. That
//! is a stronger guarantee than auto-start is worth, and it is also the §10
//! "Reboot" row: there is no `[Install]` section anywhere in this module, and
//! a test asserts it.
//!
//! The other rule here is that a directive is emitted only when
//! [`Facilities`] says it does something. `MemoryMax=` is the case that
//! earned the rule: the Paper Pro accepts it and enforces nothing, so a unit
//! carrying it would document a protection that does not exist. Where a
//! mechanism is missing the generator emits the closest thing that *is*
//! enforced and records the gap in the [`crate::report::IsolationReport`] —
//! never silently, and never by leaving the app unrestricted.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use paper_packages::{AppId, Capability, GrantedCapabilities};

use crate::facilities::{Controller, Facilities};

/// The slice every Paperclip process lands in, so a session has one cgroup
/// subtree to sweep.
pub const SLICE: &str = "paperclip.slice";

/// The target that owns a session; stopping it stops everything under it.
pub const SESSION_TARGET: &str = "paperclip-session.target";

/// The supervisor unit.
pub const HOST_UNIT: &str = "paperclip-host.service";

/// The independent recovery unit.
pub const RESTORE_UNIT: &str = "paperclip-restore-stock.service";

/// The templated foreground session unit.
pub const APP_UNIT: &str = "paperclip-app@.service";

/// Stock's unit. Named here so every reference to it in generated units goes
/// through one constant that is easy to grep for.
pub const XOCHITL_UNIT: &str = "xochitl.service";

/// How long the supervisor may go without petting systemd's watchdog.
///
/// **Derived, not chosen.** The longest thing the supervisor does
/// synchronously is a restore, and `platform/device` gives stock two guarded
/// starts with a [`RESTORE_TIMEOUT`](paper_device::stock::RESTORE_TIMEOUT)
/// wait each before it refuses. A watchdog shorter than that kills the
/// supervisor *while it is recovering* — systemd then fires `OnFailure=`, the
/// restore starts again from scratch, and the state machine never reaches
/// `Failed`, so nothing is recorded and the status file is left stale.
///
/// The VM harness found exactly that, with a 20 s watchdog: the supervisor
/// vanished mid-restore and the case saw `switching` forever.
///
/// The cost is real and is the right trade: a genuinely wedged supervisor now
/// takes this long to be noticed. During a restore the backstop is not the
/// watchdog anyway — it is `paperclip-restore-stock.service`, which systemd
/// runs in a different process.
pub const HOST_WATCHDOG: Duration =
    Duration::from_secs(paper_device::stock::RESTORE_TIMEOUT.as_secs() * 2 + 30);

/// Where a session's files live.
#[derive(Debug, Clone)]
pub struct SessionPaths {
    /// Everything Paperclip ships, off the root filesystem. `/home/root/paperclip`.
    pub root: PathBuf,
    /// Where units are written. `/run/systemd/system`.
    pub runtime_units: PathBuf,
    /// Per-session mutable state and diagnostics. Under `/run`, so a reboot
    /// clears it and a full `/home` cannot stop a recovery from recording why.
    pub state: PathBuf,
    /// Per-app persistent storage root.
    pub storage: PathBuf,
}

impl SessionPaths {
    /// The layout WWW-11 fixed for the device.
    pub fn device() -> Self {
        Self {
            root: PathBuf::from("/home/root/paperclip"),
            runtime_units: PathBuf::from("/run/systemd/system"),
            state: PathBuf::from("/run/paperclip"),
            storage: PathBuf::from("/home/root/paperclip/storage"),
        }
    }

    /// The same layout rebased under `prefix`, for a VM or a temp directory.
    pub fn under(prefix: &Path) -> Self {
        Self {
            root: prefix.join("home/root/paperclip"),
            runtime_units: prefix.join("run/systemd/system"),
            state: prefix.join("run/paperclip"),
            storage: prefix.join("home/root/paperclip/storage"),
        }
    }

    /// The `paperctl` binary, which is also the recovery entry point.
    ///
    /// **`bin/`, not `current/bin/`, and that is the whole point of it.**
    /// `paperctl` is both the recovery bootstrap and the platform updater —
    /// one binary, because both jobs have the same requirement and it is the
    /// requirement that decides the placement: the thing that performs the
    /// swap must not be one of the files being swapped. A second binary beside
    /// it would satisfy nothing this does not, and would be a second thing to
    /// keep in step.
    ///
    /// It is what
    /// [`RESTORE_UNIT`](crate::units::RESTORE_UNIT) runs when the supervisor
    /// has died, and what a person runs over SSH when the screen is wrong. A
    /// platform update replaces everything under `releases/`, and a recovery
    /// path that lived there would be a recovery path an update could break
    /// halfway through replacing it. Updating this binary is a separate
    /// operation with its own plan (§13).
    pub fn paperctl(&self) -> PathBuf {
        self.root.join("bin/paperctl")
    }

    /// The supervisor binary, resolved through the selected release.
    ///
    /// `current` is a symlink into `releases/<version>/`, so activating a
    /// staged platform release is a symlink swap and a restart rather than a
    /// file copy over a binary that something may still be executing (§13).
    pub fn host(&self) -> PathBuf {
        self.current().join("bin/paperclip-host")
    }

    /// Where platform releases are unpacked. `releases/<version>/`.
    pub fn releases(&self) -> PathBuf {
        self.root.join("releases")
    }

    /// The selected platform release. A symlink into [`Self::releases`].
    pub fn current(&self) -> PathBuf {
        self.root.join("current")
    }

    /// The release to fall back to. A symlink into [`Self::releases`].
    ///
    /// Named on disk rather than derived, so rollback is a symlink swap and
    /// the state is legible to someone with a serial cable and `ls -l`.
    pub fn previous(&self) -> PathBuf {
        self.root.join("previous")
    }

    /// The platform's own persistent state, and the update journal with it.
    ///
    /// Under [`Self::root`], not under [`Self::state`]: `state` is in `/run`
    /// and a reboot clears it, which is right for a session and wrong for a
    /// record of an update that a reboot interrupted.
    pub fn platform_state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// Where a session records its main-loop progress for the supervisor.
    pub fn progress(&self) -> PathBuf {
        self.state.join("progress")
    }

    /// Where diagnostics bundles are written.
    pub fn diagnostics(&self) -> PathBuf {
        self.state.join("diagnostics")
    }
}

/// Numeric limits for a foreground session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    /// Ceiling on address space, in bytes. This is the only memory bound that
    /// works on the Paper Pro; see [`crate::report`] for what it does and does
    /// not mean.
    pub address_space_bytes: u64,
    /// Ceiling on the number of tasks in the session.
    pub tasks: u32,
    /// CPU quota as a percentage of one core, or `None` for unbounded.
    pub cpu_percent: Option<u32>,
    /// Journal messages allowed per interval, to bound log growth.
    pub log_burst: u32,
    /// The interval `log_burst` applies to, in seconds.
    pub log_interval_secs: u32,
}

impl Default for SessionLimits {
    /// A single-user tablet with 2 GB of RAM running one foreground app.
    ///
    /// `address_space_bytes` is deliberately generous: it exists to turn a
    /// runaway allocation into a prompt, attributable failure rather than a
    /// system-wide OOM that takes stock down with it. It is not a fair-share
    /// mechanism, and §11 does not pretend it is one.
    fn default() -> Self {
        Self {
            address_space_bytes: 512 * 1024 * 1024,
            tasks: 64,
            cpu_percent: None,
            log_burst: 200,
            log_interval_secs: 30,
        }
    }
}

/// Everything needed to generate a session's units.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// Where things live.
    pub paths: SessionPaths,
    /// The unprivileged user sessions run as. On the device this is the
    /// vendor's own `xochitl` uid, which is already in `input`, `video` and
    /// `render` — Paperclip adds no user and no group.
    pub user: String,
    /// Device nodes a foreground session may open.
    pub device_allow: Vec<String>,
    /// Where the advisory display-ownership locks live — `/tmp` on the
    /// device, because that is where Xochitl keeps them.
    ///
    /// It has to be writable by the session, and `ProtectSystem=strict` makes
    /// the whole hierarchy read-only including `/tmp`. The VM harness found
    /// this the hard way: without the `ReadWritePaths=` below, a session
    /// starts, reports ready, and can never register itself as the display
    /// owner — so the switch correctly never completes and every session
    /// times out into recovery.
    pub display_locks: PathBuf,
    /// Which unit counts as stock.
    ///
    /// Configurable for exactly one reason: the VM harness has to exercise the
    /// generated units verbatim, and it cannot use the real `xochitl.service`.
    /// Everything else about the units is identical, so the harness is testing
    /// the text that ships rather than a second copy of it.
    pub stock_unit: String,
    /// Numeric limits.
    pub limits: SessionLimits,
}

impl SessionSpec {
    /// The device configuration.
    pub fn device() -> Self {
        Self {
            paths: SessionPaths::device(),
            user: "xochitl".to_owned(),
            display_locks: PathBuf::from("/tmp"),
            stock_unit: XOCHITL_UNIT.to_owned(),
            device_allow: vec![
                "/dev/dri/card0 rw".to_owned(),
                "char-input r".to_owned(),
                "/dev/null rw".to_owned(),
                "/dev/zero rw".to_owned(),
                "/dev/urandom r".to_owned(),
            ],
            limits: SessionLimits::default(),
        }
    }
}

/// What an app is allowed to touch, derived from its granted capabilities.
///
/// Derived, never declared: the input is a [`GrantedCapabilities`], which a
/// package cannot manufacture.
#[derive(Debug, Clone)]
pub struct SessionGrants {
    /// The app.
    pub app: AppId,
    /// Paths the session may write. Empty unless [`Capability::Storage`] was
    /// granted.
    pub writable: Vec<PathBuf>,
    /// Whether the session may reach the network.
    pub network: bool,
}

impl SessionGrants {
    /// Turns a host grant decision into filesystem and network reach.
    pub fn derive(app: &AppId, granted: &GrantedCapabilities, paths: &SessionPaths) -> Self {
        let mut writable = Vec::new();
        if granted.holds(Capability::Storage) {
            writable.push(paths.storage.join(app.to_string()));
        }
        Self {
            app: app.clone(),
            writable,
            network: granted.holds(Capability::Network),
        }
    }
}

/// One generated unit file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFile {
    /// File name, as it lands in `/run/systemd/system`.
    pub name: String,
    /// The full contents.
    pub contents: String,
}

/// The complete set of units a session needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSet {
    /// In the order they should be written.
    pub files: Vec<UnitFile>,
}

impl UnitSet {
    /// Looks a unit up by name.
    pub fn get(&self, name: &str) -> Option<&UnitFile> {
        self.files.iter().find(|file| file.name == name)
    }

    /// Generates the units for `spec` against what `facilities` can enforce.
    pub fn plan(spec: &SessionSpec, grants: &SessionGrants, facilities: &Facilities) -> Self {
        Self {
            files: vec![
                slice(),
                session_target(&spec.stock_unit),
                host_service(spec),
                restore_service(spec),
                app_service(spec, grants, facilities),
            ],
        }
    }
}

const HEADER: &str = "# Generated by paperctl into /run/systemd/system.\n\
                      # Runtime-only: /run is a tmpfs, so a reboot removes this\n\
                      # and the device comes back as stock (WWW-11).\n\
                      # Do not copy into /etc or /usr — Paperclip installs nothing\n\
                      # on the root filesystem, which is replaced wholesale by the\n\
                      # A/B updater.\n";

fn slice() -> UnitFile {
    UnitFile {
        name: "paperclip.slice".to_owned(),
        contents: format!(
            "{HEADER}\n\
             [Unit]\n\
             Description=Paperclip\n\
             Before=slices.target\n"
        ),
    }
}

fn session_target(stock: &str) -> UnitFile {
    UnitFile {
        name: SESSION_TARGET.to_owned(),
        contents: format!(
            "{HEADER}\n\
             [Unit]\n\
             Description=Paperclip foreground session\n\
             # Stock must be down before anything here starts, and the ordering\n\
             # is one-directional: we never express a dependency that could make\n\
             # systemd *stop* {stock} as a side effect except this Conflicts=,\n\
             # which is a clean stop. Nothing anywhere can make it fail.\n\
             Conflicts={stock}\n\
             After={stock}\n\
             # No [Install]. This target is started by name, never enabled.\n"
        ),
    }
}

fn host_service(spec: &SessionSpec) -> UnitFile {
    let paths = &spec.paths;
    let contents = format!(
        "{HEADER}\n\
         [Unit]\n\
         Description=Paperclip host supervisor\n\
         # Deliberately NOT PartOf={SESSION_TARGET}. The supervisor outlives the\n\
         # sessions it supervises: it has to still be there after a session has\n\
         # been torn down, or its record of repeated failures dies with the\n\
         # failure and \"stop retrying\" becomes \"retry forever, one process at\n\
         # a time\".\n\
         # The recovery path does not run inside this process, is not a signal\n\
         # handler, and is not a destructor. If the supervisor dies in any way\n\
         # at all — including SIGKILL, which no handler survives — systemd\n\
         # starts the restore unit.\n\
         OnFailure={RESTORE_UNIT}\n\
         \n\
         [Service]\n\
         Type=notify\n\
         NotifyAccess=main\n\
         ExecStart={host} run --state {state} --config {state}/config\n\
         # The supervisor decides what restarts, so systemd must not. A\n\
         # `Restart=` here would be the restart loop §10 forbids.\n\
         Restart=no\n\
         # A watchdog on the supervisor's own main loop — petted from that\n\
         # loop and nowhere else, so a supervisor that has stopped supervising\n\
         # is noticed rather than reported healthy by a spare thread. Missing\n\
         # it is a failure, which triggers OnFailure above.\n\
         #\n\
         # Longer than a whole restore takes, deliberately: see HOST_WATCHDOG.\n\
         WatchdogSec={watchdog}s\n\
         Slice={SLICE}\n\
         KillMode=control-group\n\
         TimeoutStopSec=10s\n\
         # No [Install]: never enabled, so a reboot comes up stock.\n",
        host = paths.host().display(),
        state = paths.state.display(),
        watchdog = HOST_WATCHDOG.as_secs(),
    );
    UnitFile {
        name: HOST_UNIT.to_owned(),
        contents,
    }
}

fn restore_service(spec: &SessionSpec) -> UnitFile {
    let paths = &spec.paths;
    let contents = format!(
        "{HEADER}\n\
         [Unit]\n\
         Description=Paperclip: return the display to stock\n\
         # Deliberately NOT PartOf= or BoundBy= the session target. This unit\n\
         # has to be able to run when everything else is gone.\n\
         DefaultDependencies=no\n\
         After={SESSION_TARGET}\n\
         Conflicts={SESSION_TARGET}\n\
         # Two attempts inside ten minutes, then stop. {XOCHITL_UNIT} carries\n\
         # StartLimitBurst=4/600s and an OnFailure= that lands on\n\
         # emergency.target, whose companion unit does not exist on this image;\n\
         # an enthusiastic retry loop here would put the tablet on a serial\n\
         # console it has no screen for.\n\
         StartLimitIntervalSec=600\n\
         StartLimitBurst=2\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=no\n\
         # `paperctl stock` talks to no host, no Home and no socket. It reads\n\
         # the device and systemd directly, which is why it still works when\n\
         # the thing that was supposed to call it is the thing that died.\n\
         ExecStart={paperctl} stock --from-unit --state {state}\n\
         Restart=no\n\
         # Longer than two guarded starts plus their waits. A timeout here\n\
         # would kill the restore that is still working.\n\
         TimeoutStartSec={timeout}s\n\
         # Failure here is the \"Xochitl fails to start\" row: it must stay\n\
         # visible in `systemctl status` rather than being retried into\n\
         # silence.\n\
         SuccessExitStatus=0\n\
         # No [Install].\n",
        paperctl = paths.paperctl().display(),
        state = paths.state.display(),
        timeout = HOST_WATCHDOG.as_secs(),
    );
    UnitFile {
        name: RESTORE_UNIT.to_owned(),
        contents,
    }
}

fn app_service(spec: &SessionSpec, grants: &SessionGrants, facilities: &Facilities) -> UnitFile {
    let paths = &spec.paths;
    let limits = spec.limits;
    let mut body = String::new();

    let _ = write!(
        body,
        "{HEADER}\n\
         [Unit]\n\
         Description=Paperclip foreground session %i\n\
         PartOf={SESSION_TARGET}\n\
         After={HOST_UNIT}\n\
         # An app crash must return the panel even if the supervisor is not\n\
         # there to notice. Belt and braces, by design: §10 forbids leaning on\n\
         # the crashed process to clean up after itself.\n\
         OnFailure={RESTORE_UNIT}\n\
         \n\
         [Service]\n\
         Type=notify\n\
         NotifyAccess=main\n\
         ExecStart={root}/apps/%i/bin/%i\n\
         # systemd never restarts a foreground session. The supervisor owns\n\
         # that decision and stops making it after a budget (§10, repeated\n\
         # failures).\n\
         Restart=no\n\
         Slice={SLICE}\n\
         KillMode=control-group\n\
         KillSignal=SIGTERM\n\
         TimeoutStopSec=5s\n\
         FinalKillSignal=SIGKILL\n\
         WorkingDirectory={root}\n\
         \n\
         # --- what this machine can actually enforce ---\n\
         User={user}\n\
         NoNewPrivileges=yes\n\
         CapabilityBoundingSet=\n\
         AmbientCapabilities=\n\
         ProtectSystem=strict\n\
         ProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\n\
         ProtectControlGroups=yes\n\
         RestrictSUIDSGID=yes\n\
         RemoveIPC=yes\n\
         LockPersonality=yes\n\
         DevicePolicy=closed\n",
        root = paths.root.display(),
        user = spec.user,
    );

    for allow in &spec.device_allow {
        let _ = writeln!(body, "DeviceAllow={allow}");
    }

    // /data is the device's identity state. Nothing Paperclip runs has any
    // business there, and the rule is cheap to make structural.
    let _ = writeln!(body, "InaccessiblePaths=-/data");

    if grants.writable.is_empty() {
        let _ = writeln!(
            body,
            "# No storage capability granted: the session gets no writable path at all."
        );
    }
    for path in &grants.writable {
        let _ = writeln!(body, "ReadWritePaths={}", path.display());
    }
    let _ = writeln!(body, "ReadOnlyPaths={}", paths.root.display());
    // Not a capability, and not optional: this is the channel the session
    // publishes main-loop progress on, and ProtectSystem=strict makes /run
    // read-only without it. A session that cannot write its witness looks
    // exactly like a session that has hung.
    let _ = writeln!(body, "ReadWritePaths={}", paths.progress().display());
    // The display-ownership registry. A foreground session must be able to
    // write it or it can never take the panel — and `ProtectSystem=strict`
    // covers /tmp too. This widens the session's reach to all of /tmp, which
    // is a real cost and is reported as such rather than hidden: see the
    // `Writable storage` row of the isolation report.
    let _ = writeln!(body, "ReadWritePaths={}", spec.display_locks.display());

    if grants.network {
        let _ = writeln!(body, "# Network granted by host policy.");
    } else {
        // Denying the network needs a mechanism, and both candidates can be
        // missing. `IPAddressDeny=` is BPF; `RestrictAddressFamilies=` is
        // seccomp. Writing either where it is not compiled in would produce a
        // unit that reads like a firewall and is not one.
        match facilities.systemd_bpf {
            Some(true) => {
                let _ = writeln!(body, "IPAddressDeny=any");
            }
            Some(false) => {
                let _ = writeln!(
                    body,
                    "# IPAddressDeny= omitted: systemd is built without the BPF framework here."
                );
            }
            None => {
                let _ = writeln!(
                    body,
                    "# IPAddressDeny= omitted: nobody has established whether systemd on this\n\
                     # platform has the BPF framework. An unproven mechanism is not written."
                );
            }
        }
        if facilities.systemd_seccomp {
            let _ = writeln!(body, "RestrictAddressFamilies=AF_UNIX");
        } else {
            let _ = writeln!(
                body,
                "# RestrictAddressFamilies= omitted: it is implemented with seccomp, which\n\
                 # this systemd was not built with. See docs/isolation.md — on such a\n\
                 # platform an ungranted app can still open a socket, and §11 says so\n\
                 # rather than implying a boundary that is not there."
            );
        }
    }

    // /tmp stays shared, and that is a decision rather than an oversight:
    // display ownership is registered in /tmp/epframebuffer.lock and
    // /tmp/epd.lock, and a session with PrivateTmp= cannot see the registry
    // it has to join. §11 reports this as an unisolated path instead of
    // pretending otherwise.
    let _ = writeln!(
        body,
        "PrivateTmp=no\n\
         # ^ deliberate: /tmp/epd.lock and /tmp/epframebuffer.lock are the\n\
         #   display-ownership registry and must be the real ones."
    );

    // Memory. The one that has to be got right.
    match facilities.memory {
        Controller::Delegated => {
            let _ = writeln!(
                body,
                "MemoryAccounting=yes\nMemoryMax={}",
                limits.address_space_bytes
            );
        }
        Controller::NotDelegated | Controller::Absent => {
            let _ = writeln!(
                body,
                "# MemoryMax= omitted: the memory controller is {state} here, so it\n\
                 # would be accepted and enforce nothing. LimitAS= is a real rlimit\n\
                 # and is checked at allocation time — a weaker, per-process bound.\n\
                 # See docs/isolation.md before claiming this is a memory limit.",
                state = match facilities.memory {
                    Controller::NotDelegated => "not delegated to our cgroup",
                    _ => "absent",
                }
            );
        }
    }
    let _ = writeln!(body, "LimitAS={}", limits.address_space_bytes);

    match facilities.pids {
        Controller::Delegated => {
            let _ = writeln!(body, "TasksAccounting=yes\nTasksMax={}", limits.tasks);
        }
        Controller::NotDelegated | Controller::Absent => {
            let _ = writeln!(
                body,
                "# TasksMax= omitted: the pids controller is not delegated here.\n\
                 LimitNPROC={}",
                limits.tasks
            );
        }
    }

    if let Some(percent) = limits.cpu_percent {
        let _ = writeln!(body, "CPUQuota={percent}%");
    }

    let _ = writeln!(
        body,
        "LogRateLimitIntervalSec={}s\nLogRateLimitBurst={}",
        limits.log_interval_secs, limits.log_burst
    );

    if facilities.systemd_seccomp {
        let _ = writeln!(
            body,
            "SystemCallArchitectures=native\n\
             SystemCallFilter=@system-service\n\
             SystemCallFilter=~@privileged @resources @obsolete"
        );
    } else {
        let _ = writeln!(
            body,
            "# SystemCallFilter= omitted: systemd is built without seccomp here,\n\
             # so every syscall directive would be accepted and ignored. The app\n\
             # applies its own filter after exec, which is a weaker guarantee\n\
             # because it runs as the app, not before it."
        );
    }

    if !facilities.has_lsm_confinement() {
        let _ = writeln!(
            body,
            "# No LSM is active ({lsms}), so there is no mandatory access control\n\
             # backstop behind any of the above.",
            lsms = if facilities.lsms.is_empty() {
                "none reported".to_owned()
            } else {
                facilities.lsms.join(",")
            }
        );
    }

    let _ = writeln!(body, "# No [Install].");

    UnitFile {
        name: APP_UNIT.to_owned(),
        contents: body,
    }
}
