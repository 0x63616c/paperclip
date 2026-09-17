//! What the isolation actually enforces (§11, deliverable 4).
//!
//! §11 chose crash recovery over hostile-app isolation, and this module is
//! where that choice stops being a sentence in a specification and becomes
//! something a reader can check. Every mechanism gets one of three verdicts,
//! and the interesting one is [`Verdict::Ineffective`]: present in the unit
//! vocabulary, accepted without complaint, and enforcing nothing. A report
//! that only had "on" and "off" could not describe the Paper Pro's memory
//! controller at all, which is precisely how that class of mistake survives.

use std::fmt;

use crate::facilities::{Controller, Facilities};

/// A protection a reader might assume Paperclip has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Mechanism {
    /// Sessions run as an unprivileged user rather than root.
    UserSeparation,
    /// The filesystem the session can write.
    WritableStorage,
    /// Device nodes the session can open.
    DeviceAccess,
    /// A bound on memory.
    MemoryBound,
    /// A bound on the number of tasks.
    TaskBound,
    /// A bound on CPU.
    CpuBound,
    /// A bound on log and output growth.
    LogBound,
    /// Syscall filtering.
    SyscallFilter,
    /// Mandatory access control.
    MandatoryAccessControl,
    /// Emptying a session's whole process tree, including children that
    /// outlived their parent.
    TreeContainment,
    /// Network reach.
    NetworkReach,
    /// Privilege escalation via setuid binaries.
    PrivilegeEscalation,
}

impl Mechanism {
    /// Every mechanism the report covers.
    pub const ALL: [Mechanism; 12] = [
        Mechanism::UserSeparation,
        Mechanism::WritableStorage,
        Mechanism::DeviceAccess,
        Mechanism::MemoryBound,
        Mechanism::TaskBound,
        Mechanism::CpuBound,
        Mechanism::LogBound,
        Mechanism::SyscallFilter,
        Mechanism::MandatoryAccessControl,
        Mechanism::TreeContainment,
        Mechanism::NetworkReach,
        Mechanism::PrivilegeEscalation,
    ];

    /// The heading used in the report.
    pub fn title(self) -> &'static str {
        match self {
            Mechanism::UserSeparation => "User separation",
            Mechanism::WritableStorage => "Writable storage",
            Mechanism::DeviceAccess => "Device access",
            Mechanism::MemoryBound => "Memory bound",
            Mechanism::TaskBound => "Task-count bound",
            Mechanism::CpuBound => "CPU bound",
            Mechanism::LogBound => "Log and output growth",
            Mechanism::SyscallFilter => "Syscall filtering",
            Mechanism::MandatoryAccessControl => "Mandatory access control",
            Mechanism::TreeContainment => "Process-tree containment",
            Mechanism::NetworkReach => "Network reach",
            Mechanism::PrivilegeEscalation => "Privilege escalation",
        }
    }
}

/// How well a mechanism holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Enforced by the kernel or by systemd, and provable.
    Enforced,
    /// Partly enforced: the bound exists but does not mean what its name
    /// suggests.
    Partial,
    /// The directive exists and does nothing here. The dangerous case.
    Ineffective,
    /// Not available at all, and not claimed.
    Absent,
}

impl Verdict {
    /// A short marker for the table.
    pub fn marker(&self) -> &'static str {
        match self {
            Verdict::Enforced => "enforced",
            Verdict::Partial => "partial",
            Verdict::Ineffective => "ineffective",
            Verdict::Absent => "absent",
        }
    }

    /// Whether a claim of protection is honest for this verdict.
    pub fn may_be_claimed(&self) -> bool {
        matches!(self, Verdict::Enforced)
    }
}

/// One row of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Which protection.
    pub mechanism: Mechanism,
    /// How well it holds.
    pub verdict: Verdict,
    /// What is actually in force.
    pub enforced_by: String,
    /// What a reader might otherwise assume, and should not.
    pub caveat: String,
}

/// The full §11 report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationReport {
    /// Where the facilities came from, printed at the top so a recorded
    /// profile can never be read as a live probe.
    pub source: String,
    /// One row per mechanism.
    pub findings: Vec<Finding>,
}

impl IsolationReport {
    /// Derives the report from what the machine provides.
    pub fn derive(facilities: &Facilities) -> Self {
        let memory = memory_finding(facilities);
        let tasks = task_finding(facilities);
        let tree = tree_finding(facilities);
        let syscalls = syscall_finding(facilities);
        let mac = mac_finding(facilities);
        Self {
            source: facilities.source.to_string(),
            findings: vec![
                Finding {
                    mechanism: Mechanism::UserSeparation,
                    verdict: Verdict::Enforced,
                    enforced_by: "User= on the session unit; the vendor's own unprivileged uid"
                        .to_owned(),
                    caveat: "Same uid as stock Xochitl. A session can reach anything that \
                             uid can reach, including Xochitl's own files. This is separation \
                             from root, not from the user's data."
                        .to_owned(),
                },
                Finding {
                    mechanism: Mechanism::WritableStorage,
                    verdict: Verdict::Partial,
                    enforced_by:
                        "ProtectSystem=strict, ReadOnlyPaths=, ReadWritePaths= from granted \
                         capabilities, InaccessiblePaths=/data"
                            .to_owned(),
                    caveat: "/tmp is deliberately shared: display ownership is registered in \
                             /tmp/epframebuffer.lock and /tmp/epd.lock, and a session with \
                             PrivateTmp= cannot join that registry. A session can therefore \
                             write /tmp."
                        .to_owned(),
                },
                Finding {
                    mechanism: Mechanism::DeviceAccess,
                    verdict: Verdict::Enforced,
                    enforced_by: "DevicePolicy=closed with an explicit DeviceAllow= list"
                        .to_owned(),
                    caveat: "The allow list includes the DRM node and input devices, because a \
                             foreground session needs both. Any session that can draw can also \
                             read every pen and touch event."
                        .to_owned(),
                },
                memory,
                tasks,
                Finding {
                    mechanism: Mechanism::CpuBound,
                    verdict: if facilities.cgroup == crate::facilities::CgroupLayout::None {
                        Verdict::Absent
                    } else {
                        Verdict::Partial
                    },
                    enforced_by: "CPUQuota=, when configured".to_owned(),
                    caveat: "Left unset by default. A busy loop in a session will not lock the \
                             device out of recovery — the supervisor's deadlines do not depend \
                             on the session yielding — but it will make the panel feel dead."
                        .to_owned(),
                },
                Finding {
                    mechanism: Mechanism::LogBound,
                    verdict: Verdict::Enforced,
                    enforced_by: "LogRateLimitIntervalSec= and LogRateLimitBurst= on the session"
                        .to_owned(),
                    caveat: "Bounds the journal. A session that writes its own file inside its \
                             granted storage is bounded by the filesystem, not by this."
                        .to_owned(),
                },
                syscalls,
                mac,
                tree,
                network_finding(facilities),
                Finding {
                    mechanism: Mechanism::PrivilegeEscalation,
                    verdict: Verdict::Enforced,
                    enforced_by: "NoNewPrivileges=yes, empty CapabilityBoundingSet=, \
                                  RestrictSUIDSGID=yes"
                        .to_owned(),
                    caveat: "Kernel vulnerabilities are out of scope; nothing here is a \
                             sandbox against a hostile binary."
                        .to_owned(),
                },
            ],
        }
    }

    /// The row for `mechanism`.
    pub fn finding(&self, mechanism: Mechanism) -> Option<&Finding> {
        self.findings
            .iter()
            .find(|finding| finding.mechanism == mechanism)
    }

    /// Mechanisms that must not be described as protections.
    pub fn unclaimable(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| !finding.verdict.may_be_claimed())
    }

    /// The report as Markdown, for `docs/isolation.md` and for issue comments.
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "Facilities {}.\n", self.source);
        let _ = writeln!(out, "| Protection | Verdict | In force | Do not assume |");
        let _ = writeln!(out, "|---|---|---|---|");
        for finding in &self.findings {
            let _ = writeln!(
                out,
                "| {} | **{}** | {} | {} |",
                finding.mechanism.title(),
                finding.verdict.marker(),
                finding.enforced_by,
                finding.caveat
            );
        }
        out
    }
}

impl fmt::Display for IsolationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "facilities {}", self.source)?;
        for finding in &self.findings {
            writeln!(
                f,
                "{:<26} {:<12} {}",
                finding.mechanism.title(),
                finding.verdict.marker(),
                finding.enforced_by
            )?;
        }
        Ok(())
    }
}

fn memory_finding(facilities: &Facilities) -> Finding {
    match facilities.memory {
        Controller::Delegated => Finding {
            mechanism: Mechanism::MemoryBound,
            verdict: Verdict::Enforced,
            enforced_by: "MemoryMax= on the session cgroup, plus LimitAS=".to_owned(),
            caveat: "Exceeding it kills the session, not the allocation.".to_owned(),
        },
        Controller::NotDelegated => Finding {
            mechanism: Mechanism::MemoryBound,
            verdict: Verdict::Partial,
            enforced_by: "LimitAS= only — an rlimit, checked per process at allocation time"
                .to_owned(),
            caveat: "MemoryMax= is NOT in force and is not written into the unit. The memory \
                     controller is not delegated to our cgroup, so systemd accepts \
                     MemoryAccounting=yes and reports MemoryCurrent=[not set]. Memory \
                     exhaustion is bounded per process by rlimit and system-wide by the OOM \
                     killer; it is not bounded per session."
                .to_owned(),
        },
        Controller::Absent => Finding {
            mechanism: Mechanism::MemoryBound,
            verdict: Verdict::Partial,
            enforced_by: "LimitAS= only".to_owned(),
            caveat: "No memory controller at all. Same rlimit-and-OOM-killer position as \
                     above, with no cgroup accounting even for observation."
                .to_owned(),
        },
    }
}

fn task_finding(facilities: &Facilities) -> Finding {
    if facilities.pids.enforces() {
        Finding {
            mechanism: Mechanism::TaskBound,
            verdict: Verdict::Enforced,
            enforced_by: "TasksMax= on the session cgroup".to_owned(),
            caveat: "Counts tasks, not memory or descriptors.".to_owned(),
        }
    } else {
        Finding {
            mechanism: Mechanism::TaskBound,
            verdict: Verdict::Partial,
            enforced_by: "LimitNPROC= only — per uid, not per session".to_owned(),
            caveat: "The pids controller is not delegated, so the bound is shared with every \
                     other process running as the same user, stock Xochitl included."
                .to_owned(),
        }
    }
}

fn tree_finding(facilities: &Facilities) -> Finding {
    let termination = facilities.tree_termination();
    Finding {
        mechanism: Mechanism::TreeContainment,
        verdict: if termination.is_complete() {
            Verdict::Enforced
        } else {
            Verdict::Partial
        },
        enforced_by: format!(
            "KillMode=control-group, and a supervisor sweep: {}",
            termination.description()
        ),
        caveat: if facilities.cgroup_freeze || facilities.cgroup_kill {
            "The tree can be frozen or killed atomically, so nothing can fork faster than it \
             is signalled."
                .to_owned()
        } else {
            "Neither cgroup.freeze nor cgroup.kill exists here, so the sweep cannot be atomic: \
             a process that forks between two reads of cgroup.procs is caught on the next \
             pass, not the current one. The sweep repeats until the set is empty, which \
             terminates against anything that is not deliberately fork-bombing."
                .to_owned()
        },
    }
}

fn syscall_finding(facilities: &Facilities) -> Finding {
    if facilities.systemd_seccomp {
        Finding {
            mechanism: Mechanism::SyscallFilter,
            verdict: Verdict::Enforced,
            enforced_by: "SystemCallFilter= applied by systemd before exec".to_owned(),
            caveat: "An allow-list of @system-service; it is not a proof of safety.".to_owned(),
        }
    } else {
        Finding {
            mechanism: Mechanism::SyscallFilter,
            verdict: Verdict::Ineffective,
            enforced_by: "nothing at the unit level".to_owned(),
            caveat: "systemd is built without seccomp, so SystemCallFilter= and every related \
                     directive are parsed and ignored. Paperclip does not write them, because \
                     a unit file that lists them reads like a protection. An app may install \
                     its own filter after exec, which protects against its own later bugs and \
                     not against the app itself."
                .to_owned(),
        }
    }
}

fn network_finding(facilities: &Facilities) -> Finding {
    match (facilities.systemd_bpf, facilities.systemd_seccomp) {
        (Some(true), _) => Finding {
            mechanism: Mechanism::NetworkReach,
            verdict: Verdict::Enforced,
            enforced_by: "IPAddressDeny=any, unless the network capability was granted".to_owned(),
            caveat: "AF_UNIX stays open so the session can talk to the host.".to_owned(),
        },
        (Some(false), true) => Finding {
            mechanism: Mechanism::NetworkReach,
            verdict: Verdict::Partial,
            enforced_by: "RestrictAddressFamilies=AF_UNIX only".to_owned(),
            caveat: "No BPF framework, so IPAddressDeny= is not written. A session cannot \
                     create an IP socket, but nothing filters traffic for one it inherits."
                .to_owned(),
        },
        (Some(false), false) => Finding {
            mechanism: Mechanism::NetworkReach,
            verdict: Verdict::Ineffective,
            enforced_by: "nothing at the unit level".to_owned(),
            caveat: "Neither mechanism exists here: IPAddressDeny= needs BPF and \
                     RestrictAddressFamilies= needs seccomp. An app that was NOT granted the \
                     network capability can still open a socket. The capability is a record \
                     of a decision, not an enforced boundary, on this platform."
                .to_owned(),
        },
        (None, seccomp) => Finding {
            mechanism: Mechanism::NetworkReach,
            verdict: Verdict::Ineffective,
            enforced_by: if seccomp {
                "RestrictAddressFamilies=AF_UNIX only".to_owned()
            } else {
                "nothing at the unit level".to_owned()
            },
            caveat: "Whether this systemd has the BPF framework has not been established, so \
                     IPAddressDeny= is not written and no network boundary is claimed. One \
                     line of `systemctl --version` on the device closes this."
                .to_owned(),
        },
    }
}

fn mac_finding(facilities: &Facilities) -> Finding {
    if facilities.has_lsm_confinement() {
        Finding {
            mechanism: Mechanism::MandatoryAccessControl,
            verdict: Verdict::Partial,
            enforced_by: format!("active LSMs: {}", facilities.lsms.join(",")),
            caveat: "A confining LSM is present, but no Paperclip policy is loaded for it. \
                     Sessions get whatever the platform's own policy says about an \
                     unconfined service, which may be nothing."
                .to_owned(),
        }
    } else {
        Finding {
            mechanism: Mechanism::MandatoryAccessControl,
            verdict: Verdict::Absent,
            enforced_by: "nothing".to_owned(),
            caveat: "No SELinux, AppArmor, Smack or TOMOYO. `landlock` and `bpf` may appear \
                     in the LSM list and are not counted: they confine a process that asks to \
                     be confined, and Paperclip does not ask. So every restriction above is \
                     discretionary with no mandatory backstop, and §11's refusal to claim \
                     hostile-code isolation rests on this row."
                .to_owned(),
        }
    }
}
