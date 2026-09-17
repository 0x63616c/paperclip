//! Reading [`Facilities`] off the machine this process is running on.
//!
//! Only Linux can answer these questions, and the answer for every other
//! platform is an error rather than a plausible-looking default. A macOS
//! build that returned "seccomp: unavailable" would be indistinguishable from
//! a device that told us so.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::facilities::{CgroupLayout, Controller, Facilities, FacilitySource, SandboxTool};

/// Why a probe could not produce a [`Facilities`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProbeError {
    /// The probe was run somewhere it cannot mean anything.
    #[error("isolation facilities can only be probed on Linux; this is {platform}")]
    UnsupportedPlatform {
        /// The platform the binary was built for.
        platform: &'static str,
    },

    /// A file the probe depends on could not be read.
    #[error("cannot read {path} while probing isolation facilities")]
    Read {
        /// Which file.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

/// The directories a probe reads, so tests can point it at a fixture tree.
///
/// A probe with `root` set to something other than `/` is a *simulation* and
/// says so in its [`FacilitySource`]; nothing in this crate lets a simulated
/// probe be reported as a real one.
#[derive(Debug, Clone)]
pub struct ProbeRoots {
    /// Stands in for `/`.
    pub root: PathBuf,
}

impl Default for ProbeRoots {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }
}

/// Reads the facilities of the running machine.
///
/// # Errors
///
/// Returns [`ProbeError::UnsupportedPlatform`] anywhere but Linux.
pub fn probe() -> Result<Facilities, ProbeError> {
    if cfg!(not(target_os = "linux")) {
        return Err(ProbeError::UnsupportedPlatform {
            platform: std::env::consts::OS,
        });
    }
    let roots = ProbeRoots::default();
    let kernel = read_trimmed(&roots.root.join("proc/sys/kernel/osrelease")).unwrap_or_default();
    Ok(Facilities {
        source: FacilitySource::Probed {
            kernel,
            arch: std::env::consts::ARCH,
        },
        systemd_version: systemd_version(),
        cgroup: cgroup_layout(&roots),
        memory: controller(&roots, "memory"),
        pids: controller(&roots, "pids"),
        cgroup_freeze: own_cgroup_file(&roots, "cgroup.freeze").is_some(),
        cgroup_kill: own_cgroup_file(&roots, "cgroup.kill").is_some(),
        systemd_seccomp: systemd_features().is_some_and(|f| f.contains("+SECCOMP")),
        systemd_bpf: systemd_features().map(|f| f.contains("+BPF_FRAMEWORK")),
        lsms: read_trimmed(&roots.root.join("sys/kernel/security/lsm"))
            .map(|raw| raw.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
        sandbox_tools: SandboxTool::ALL
            .into_iter()
            .filter(|tool| on_path(tool.binary()))
            .collect(),
    })
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|raw| raw.trim().to_owned())
}

/// systemd's layout, decided from what is actually mounted.
///
/// Read from `/proc/mounts` rather than `systemctl --version` because a
/// container can have systemd installed and no hierarchy of its own.
fn cgroup_layout(roots: &ProbeRoots) -> CgroupLayout {
    let Some(mounts) = read_trimmed(&roots.root.join("proc/mounts")) else {
        return CgroupLayout::None;
    };
    let mut v1 = false;
    let mut v2 = false;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_source), Some(point), Some(kind)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !point.starts_with("/sys/fs/cgroup") {
            continue;
        }
        match kind {
            "cgroup2" => v2 = true,
            "cgroup" => v1 = true,
            _ => {}
        }
    }
    match (v2, v1) {
        (true, true) => CgroupLayout::Hybrid,
        (true, false) => CgroupLayout::Unified,
        (false, true) => CgroupLayout::Legacy,
        (false, false) => CgroupLayout::None,
    }
}

/// Whether `name` is usable on the cgroup our own process sits in.
///
/// Delegation is a property of *our* cgroup, not of the kernel: a controller
/// can be compiled in, mounted, and still absent from the `cgroup.controllers`
/// of the directory our units land in. That is exactly the Paper Pro's memory
/// controller, and asking the kernel instead of asking our own cgroup is how
/// that gets missed.
fn controller(roots: &ProbeRoots, name: &str) -> Controller {
    let delegated = own_cgroup_file(roots, "cgroup.controllers")
        .and_then(|path| read_trimmed(&path))
        .is_some_and(|list| list.split_whitespace().any(|entry| entry == name));
    if delegated {
        return Controller::Delegated;
    }
    let present = read_trimmed(&roots.root.join("proc/cgroups"))
        .is_some_and(|raw| raw.lines().any(|line| line.starts_with(name)))
        || roots.root.join("sys/fs/cgroup").join(name).is_dir();
    if present {
        Controller::NotDelegated
    } else {
        Controller::Absent
    }
}

/// Resolves `name` inside the cgroup v2 directory this process belongs to.
fn own_cgroup_file(roots: &ProbeRoots, name: &str) -> Option<PathBuf> {
    let own = read_trimmed(&roots.root.join("proc/self/cgroup"))?;
    // The unified entry is the one with an empty controller list: `0::/path`.
    let relative = own
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim_start_matches('/')
        .to_owned();
    let path = roots.root.join("sys/fs/cgroup").join(relative).join(name);
    path.exists().then_some(path)
}

fn systemd_features() -> Option<String> {
    let output = std::process::Command::new("systemctl")
        .arg("--version")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn systemd_version() -> Option<u32> {
    systemd_features()?
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn on_path(binary: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(binary).is_file())
}

/// A probe result together with the machine it describes, for printing.
#[derive(Debug)]
pub struct ProbeSummary<'a> {
    /// What was found.
    pub facilities: &'a Facilities,
}

impl fmt::Display for ProbeSummary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let facilities = self.facilities;
        writeln!(f, "source        {}", facilities.source)?;
        writeln!(
            f,
            "systemd       {}",
            facilities
                .systemd_version
                .map_or_else(|| "unknown".to_owned(), |v| v.to_string())
        )?;
        writeln!(f, "cgroup        {}", facilities.cgroup.name())?;
        writeln!(f, "memory ctrl   {:?}", facilities.memory)?;
        writeln!(f, "pids ctrl     {:?}", facilities.pids)?;
        writeln!(f, "cgroup.freeze {}", facilities.cgroup_freeze)?;
        writeln!(f, "cgroup.kill   {}", facilities.cgroup_kill)?;
        writeln!(f, "seccomp       {}", facilities.systemd_seccomp)?;
        writeln!(
            f,
            "bpf           {}",
            facilities
                .systemd_bpf
                .map_or_else(|| "unknown".to_owned(), |has| has.to_string())
        )?;
        writeln!(f, "lsm           {}", facilities.lsms.join(","))?;
        write!(
            f,
            "tree kill     {}",
            facilities.tree_termination().description()
        )
    }
}
