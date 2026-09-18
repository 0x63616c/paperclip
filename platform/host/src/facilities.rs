//! What the machine underneath actually provides (§11).
//!
//! Every isolation directive Paperclip emits is chosen from a [`Facilities`]
//! value, never from a wish list. The type exists because the reMarkable Paper
//! Pro accepts several systemd directives that then do nothing — `MemoryMax=`
//! being the expensive one — and a silent no-op is worse than a missing
//! feature, because it reads like a protection in the unit file.
//!
//! Two constructors, and the difference between them is the whole point:
//!
//! * [`probe`](crate::probe::probe) reads the machine this process is running on.
//! * [`Facilities::paper_pro`] is a *recording* of what WWW-1 and WWW-11
//!   observed on the tablet. It is evidence, transcribed; it is not a probe,
//!   and [`FacilitySource`] keeps the two from being confused in a report.

use std::fmt;

/// Where a [`Facilities`] value came from.
///
/// Carried into every report so a reader can tell a live measurement from a
/// transcription of someone else's measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FacilitySource {
    /// Read from the running machine, named by `uname -srm` equivalent.
    Probed {
        /// Kernel release, as `/proc/sys/kernel/osrelease` reports it.
        kernel: String,
        /// Machine architecture, as the build target names it.
        arch: &'static str,
    },
    /// Transcribed from a device report rather than measured here.
    Recorded {
        /// The device the observations came from.
        device: &'static str,
        /// The issue that established them.
        issue: &'static str,
    },
}

impl fmt::Display for FacilitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FacilitySource::Probed { kernel, arch } => write!(f, "probed on {arch} {kernel}"),
            FacilitySource::Recorded { device, issue } => {
                write!(f, "recorded from {device} ({issue})")
            }
        }
    }
}

/// How the cgroup hierarchy is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupLayout {
    /// cgroup v2 only, mounted at `/sys/fs/cgroup`.
    Unified,
    /// systemd's `hybrid`: a v2 hierarchy for tracking, v1 for controllers.
    /// This is the Paper Pro.
    Hybrid,
    /// cgroup v1 only.
    Legacy,
    /// No cgroup filesystem found.
    None,
}

impl CgroupLayout {
    /// The name systemd prints for this layout in `systemctl --version`.
    pub fn name(self) -> &'static str {
        match self {
            CgroupLayout::Unified => "unified",
            CgroupLayout::Hybrid => "hybrid",
            CgroupLayout::Legacy => "legacy",
            CgroupLayout::None => "none",
        }
    }
}

/// Whether a cgroup controller can actually be used on our own units.
///
/// The middle variant is the one that matters. A controller compiled into the
/// kernel but not delegated to the cgroup our services land in will accept
/// `MemoryAccounting=yes` and then report `MemoryCurrent=[not set]`, and
/// `MemoryMax=` will bound nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Controller {
    /// Present and delegated: directives take effect.
    Delegated,
    /// Present in the kernel but not delegated here: directives are accepted
    /// and silently ineffective.
    NotDelegated,
    /// Not present at all.
    Absent,
}

impl Controller {
    /// Whether a limit written against this controller will be enforced.
    pub fn enforces(self) -> bool {
        matches!(self, Controller::Delegated)
    }
}

/// A sandboxing helper binary, looked up on `PATH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SandboxTool {
    /// `unshare(1)`.
    Unshare,
    /// `nsenter(1)`.
    Nsenter,
    /// `setpriv(1)`.
    Setpriv,
    /// `bwrap(1)`, bubblewrap.
    Bwrap,
    /// `chroot(1)`.
    Chroot,
}

impl SandboxTool {
    /// Everything looked for.
    pub const ALL: [SandboxTool; 5] = [
        SandboxTool::Unshare,
        SandboxTool::Nsenter,
        SandboxTool::Setpriv,
        SandboxTool::Bwrap,
        SandboxTool::Chroot,
    ];

    /// The binary name on `PATH`.
    pub fn binary(self) -> &'static str {
        match self {
            SandboxTool::Unshare => "unshare",
            SandboxTool::Nsenter => "nsenter",
            SandboxTool::Setpriv => "setpriv",
            SandboxTool::Bwrap => "bwrap",
            SandboxTool::Chroot => "chroot",
        }
    }
}

/// The facilities a host can build a session out of.
///
/// Construct with [`probe`](crate::probe::probe) or take one of the recorded constants.
/// Every field is an observation, not a preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facilities {
    /// Where these observations came from.
    pub source: FacilitySource,
    /// systemd's major version, if it could be read.
    pub systemd_version: Option<u32>,
    /// How the cgroup tree is mounted.
    pub cgroup: CgroupLayout,
    /// Whether memory limits written on our units are enforced.
    pub memory: Controller,
    /// Whether task-count limits written on our units are enforced.
    pub pids: Controller,
    /// Whether `cgroup.freeze` exists on our own cgroup.
    pub cgroup_freeze: bool,
    /// Whether `cgroup.kill` exists on our own cgroup.
    pub cgroup_kill: bool,
    /// Whether systemd was built with seccomp support, so `SystemCallFilter=`
    /// and `RestrictAddressFamilies=` do anything.
    pub systemd_seccomp: bool,
    /// Whether systemd was built with the BPF framework, which is what
    /// `IPAddressDeny=` is implemented on top of.
    ///
    /// `None` means nobody has established it on this platform yet. That is
    /// not the same as `Some(false)`, and the generator and the report treat
    /// it differently: an unknown is reported as unproven, never as absent and
    /// never as present.
    pub systemd_bpf: Option<bool>,
    /// Which Linux security modules are active, as `/sys/kernel/security/lsm`
    /// lists them. `capability` alone means no confinement is available.
    pub lsms: Vec<String>,
    /// Which sandbox helpers exist on `PATH`.
    pub sandbox_tools: Vec<SandboxTool>,
}

impl Facilities {
    /// The reMarkable Paper Pro, as WWW-1 and WWW-11 measured it.
    ///
    /// Transcribed so that unit generation and the isolation report can be
    /// exercised against device reality from any machine. Running the tests
    /// against this value proves the *decisions* are right for the tablet; it
    /// proves nothing about enforcement, which only the device can show.
    pub fn paper_pro() -> Self {
        Self {
            source: FacilitySource::Recorded {
                device: "reMarkable Paper Pro",
                issue: "WWW-1 / WWW-11",
            },
            // Version not recorded in the gate report; left unknown rather
            // than guessed, and the generator must cope with `None`.
            systemd_version: None,
            cgroup: CgroupLayout::Hybrid,
            // Accepted, and silently ineffective. This single field is why
            // `MemoryMax=` never reaches a generated unit.
            memory: Controller::NotDelegated,
            pids: Controller::Delegated,
            cgroup_freeze: false,
            cgroup_kill: false,
            // systemd is built `-SECCOMP` on this image.
            systemd_seccomp: false,
            // Not established by WWW-1 or WWW-11. Left unknown rather than
            // assumed in either direction; the first device run that reads
            // `systemctl --version` closes it.
            systemd_bpf: None,
            lsms: vec!["capability".to_owned()],
            sandbox_tools: vec![SandboxTool::Chroot],
        }
    }

    /// Whether a security module that can actually confine a service is
    /// active.
    ///
    /// Deliberately narrow. `/sys/kernel/security/lsm` on a modern kernel
    /// lists `capability,landlock,yama,bpf` on a machine with no mandatory
    /// access control whatsoever: `capability` is a stub, `yama` only scopes
    /// ptrace, and `landlock` and `bpf` are opt-in — they confine a process
    /// that asks to be confined, and Paperclip does not ask. Counting those as
    /// confinement is how a report ends up claiming a backstop that is not
    /// there.
    pub fn has_lsm_confinement(&self) -> bool {
        const CONFINING: [&str; 4] = ["selinux", "apparmor", "smack", "tomoyo"];
        self.lsms
            .iter()
            .any(|lsm| CONFINING.contains(&lsm.as_str()))
    }

    /// Whether `tool` is available here.
    pub fn has_tool(&self, tool: SandboxTool) -> bool {
        self.sandbox_tools.contains(&tool)
    }

    /// How a hung session's process tree has to be emptied.
    ///
    /// §10 asks for the "complete cgroup/process tree" to be contained and
    /// terminated. On a kernel with `cgroup.kill` that is one write. Without
    /// it — and the Paper Pro has neither `cgroup.kill` nor the freezer — the
    /// only honest answer is to read the membership repeatedly and signal
    /// what is there, accepting the race and re-reading until it is empty.
    pub fn tree_termination(&self) -> TreeTermination {
        if self.cgroup_kill {
            TreeTermination::CgroupKill
        } else if self.pids.enforces() {
            TreeTermination::WalkCgroupProcs
        } else {
            TreeTermination::WalkProcTable
        }
    }
}

/// How the supervisor empties a session's process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeTermination {
    /// One write to `cgroup.kill`. Atomic, no race, not available on the
    /// Paper Pro.
    CgroupKill,
    /// Re-read `cgroup.procs` and signal every member until it stays empty.
    WalkCgroupProcs,
    /// No usable cgroup: walk `/proc` by parent pid. Last resort, and the
    /// only mode that can genuinely miss a reparented process.
    WalkProcTable,
}

impl TreeTermination {
    /// Whether this mode can guarantee it saw every member.
    ///
    /// `WalkProcTable` cannot: a process that reparents to pid 1 between two
    /// scans is indistinguishable from one that was never ours.
    pub fn is_complete(self) -> bool {
        !matches!(self, TreeTermination::WalkProcTable)
    }

    /// A short phrase for the report.
    pub fn description(self) -> &'static str {
        match self {
            TreeTermination::CgroupKill => "single write to cgroup.kill",
            TreeTermination::WalkCgroupProcs => "repeated cgroup.procs sweep until empty",
            TreeTermination::WalkProcTable => "/proc parent-pid walk (incomplete by construction)",
        }
    }
}
