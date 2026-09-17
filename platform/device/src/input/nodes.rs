//! Finding the pen and the touchscreen, without counting event nodes.
//!
//! `/dev/input/event2` is the pen and `event3` is the touchscreen *on this
//! image, today*. The Paper Pro's node ordering differs from earlier
//! reMarkables and rmweb's device profile is explicit that it must be resolved
//! by name; binding to a number is a bug waiting for a firmware update.
//!
//! Two signals are available and this module uses both.
//!
//! The **names** are now confirmed on this image: the pen is
//! `Elan marker input` and the touchscreen is `Elan touch input`. The
//! **advertised capability bits** say the same thing independently — a node
//! reporting `BTN_TOOL_PEN` is the pen whatever it calls itself.
//!
//! [`resolve`] requires them to agree. A name is the more specific signal and
//! wins when both are present, but a node whose name and capabilities
//! disagree is reported as such rather than silently resolved: on a device
//! where the whole input path is two files, picking the wrong one is worth an
//! error, and a firmware update that renames a node should be visible the
//! first time it happens rather than the first time a tap lands nowhere.
//!
//! `tools/device-probe/evname.c` prints the same mapping without needing a
//! Rust toolchain.

use std::fmt;
use std::path::{Path, PathBuf};

/// What a `/dev/input` node appears to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputRole {
    /// Reports `BTN_TOOL_PEN`.
    Pen,
    /// Reports multitouch positions.
    Touch,
    /// Reports `KEY_POWER`.
    PowerKey,
    /// Something else — a hall sensor, a lid switch, a keyboard folio.
    Other,
}

impl fmt::Display for InputRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Pen => "pen",
            Self::Touch => "touch",
            Self::PowerKey => "power-key",
            Self::Other => "other",
        };
        f.write_str(name)
    }
}

/// A capability bitmap as `EVIOCGBIT` returns it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    bits: Vec<u8>,
}

impl Capabilities {
    /// Wraps the bytes `EVIOCGBIT` wrote.
    pub fn from_bytes(bits: Vec<u8>) -> Self {
        Self { bits }
    }

    /// Whether `code` is advertised. Out-of-range codes are simply absent.
    pub fn has(&self, code: u16) -> bool {
        let byte = code as usize / 8;
        let bit = code as usize % 8;
        self.bits
            .get(byte)
            .is_some_and(|value| value >> bit & 1 == 1)
    }
}

const BTN_TOOL_PEN: u16 = 0x140;
const KEY_POWER: u16 = 0x74;
const ABS_MT_POSITION_X: u16 = 0x35;

/// Classifies a node from what it advertises.
///
/// Order matters: the pen also reports absolute positions, so the pen test
/// runs first. This is the same order `tools/device-probe/evname.c` uses, on
/// purpose — two implementations that disagree about which node is the pen
/// would be worse than either.
pub fn classify(keys: &Capabilities, abs: &Capabilities) -> InputRole {
    if keys.has(BTN_TOOL_PEN) {
        InputRole::Pen
    } else if abs.has(ABS_MT_POSITION_X) {
        InputRole::Touch
    } else if keys.has(KEY_POWER) {
        InputRole::PowerKey
    } else {
        InputRole::Other
    }
}

/// The pen's `EVIOCGNAME` on image `20260827113527`.
pub const PEN_NAME: &str = "Elan marker input";
/// The touchscreen's `EVIOCGNAME` on image `20260827113527`.
pub const TOUCH_NAME: &str = "Elan touch input";

/// The confirmed name for a role, if there is one.
pub const fn expected_name(role: InputRole) -> Option<&'static str> {
    match role {
        InputRole::Pen => Some(PEN_NAME),
        InputRole::Touch => Some(TOUCH_NAME),
        InputRole::PowerKey | InputRole::Other => None,
    }
}

/// One `/dev/input/event*` node and what it turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputNode {
    /// The device node.
    pub path: PathBuf,
    /// What `EVIOCGNAME` reported. Recorded for logs and overrides only.
    pub name: String,
    /// What its capability bits say it is.
    pub role: InputRole,
}

/// Picks the single node with `role`, if there is exactly one.
///
/// Two pens is as much a reason to stop as none: guessing which of them the
/// user is holding is not something this layer can do, and silently taking the
/// first would make the failure intermittent rather than visible.
pub fn sole(nodes: &[InputNode], role: InputRole) -> Option<&InputNode> {
    let mut matching = nodes.iter().filter(|node| node.role == role);
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

/// How a role was resolved to a node, or why it was not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// Name and capabilities agree. The ordinary case.
    Confirmed(&'a InputNode),
    /// The capabilities are right but the name is not the confirmed one — a
    /// firmware update, or a different device. Usable, but say so.
    NameChanged {
        /// The node the capability bits picked.
        node: &'a InputNode,
        /// The name this image was expected to report.
        expected: &'static str,
    },
    /// The confirmed name is present on a node whose capabilities say it is
    /// something else. Do not use it: one of the two signals is lying.
    Contradictory {
        /// The node carrying the expected name.
        node: &'a InputNode,
    },
    /// No node, or more than one, claims the role.
    Unresolved,
}

impl Resolution<'_> {
    /// The node to read from, if there is one that can be trusted.
    pub fn node(&self) -> Option<&InputNode> {
        match self {
            Self::Confirmed(node) | Self::NameChanged { node, .. } => Some(node),
            Self::Contradictory { .. } | Self::Unresolved => None,
        }
    }
}

/// Resolves `role` against both signals, reporting how it was decided.
pub fn resolve(nodes: &[InputNode], role: InputRole) -> Resolution<'_> {
    let expected = expected_name(role);

    // A node wearing the confirmed name but classified as something else is
    // the one case where proceeding would be worse than stopping.
    if let Some(expected) = expected
        && let Some(named) = nodes.iter().find(|node| node.name == expected)
        && named.role != role
    {
        return Resolution::Contradictory { node: named };
    }

    let Some(node) = sole(nodes, role) else {
        return Resolution::Unresolved;
    };
    match expected {
        Some(expected) if node.name != expected => Resolution::NameChanged { node, expected },
        _ => Resolution::Confirmed(node),
    }
}

#[cfg(target_os = "linux")]
pub use linux::enumerate;

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
// Justified per the note in the workspace manifest: `EVIOCGNAME` and
// `EVIOCGBIT` have no safe wrapper in `libc`, and pulling in an evdev crate to
// issue two ioctls would add a dependency to the one part of the tree that
// must stay auditable. Every `unsafe` block below is a single ioctl on a file
// descriptor this module owns, writing into a buffer this module sized.
mod linux {
    use std::ffi::CStr;
    use std::fs;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    use super::{Capabilities, InputNode, classify};
    use crate::error::DeviceError;

    const EV_KEY: u32 = 0x01;
    const EV_ABS: u32 = 0x03;
    const KEY_BYTES: usize = 96; // KEY_MAX 0x2FF, rounded up.
    const ABS_BYTES: usize = 8; // ABS_MAX 0x3F.
    const NAME_BYTES: usize = 256;

    // The `_IOC` encoding from `asm-generic/ioctl.h`: direction in the top two
    // bits, size, then the 'E' type and the number.
    const fn ioc_read(nr: u32, size: usize) -> libc::c_ulong {
        ((2 << 30) | ((size as u32) << 16) | (0x45 << 8) | nr) as libc::c_ulong
    }

    const fn eviocgname(size: usize) -> libc::c_ulong {
        ioc_read(0x06, size)
    }

    const fn eviocgbit(event: u32, size: usize) -> libc::c_ulong {
        ioc_read(0x20 + event, size)
    }

    fn open_node(path: &Path) -> Result<OwnedFd, DeviceError> {
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map(OwnedFd::from)
            .map_err(|source| DeviceError::io("open", path, source))
    }

    fn read_name(fd: &OwnedFd) -> String {
        let mut buffer = [0u8; NAME_BYTES];
        // SAFETY: `buffer` is `NAME_BYTES` long and that same length is encoded
        // into the request, so the kernel cannot write past it. `fd` is a live
        // descriptor owned by the caller for the duration of the call.
        let written = unsafe {
            libc::ioctl(
                fd.as_raw_fd(),
                eviocgname(NAME_BYTES) as _,
                buffer.as_mut_ptr(),
            )
        };
        if written <= 0 {
            return "<unnamed>".to_owned();
        }
        CStr::from_bytes_until_nul(&buffer)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "<unnamed>".to_owned())
    }

    fn read_bits(fd: &OwnedFd, event: u32, size: usize) -> Capabilities {
        let mut buffer = vec![0u8; size];
        // SAFETY: same contract as `read_name` — the buffer is `size` bytes and
        // `size` is what the request tells the kernel it may write. A failed
        // ioctl leaves the zeroed buffer, which reads as "advertises nothing".
        let written = unsafe {
            libc::ioctl(
                fd.as_raw_fd(),
                eviocgbit(event, size) as _,
                buffer.as_mut_ptr(),
            )
        };
        if written <= 0 {
            buffer.fill(0);
        }
        Capabilities::from_bytes(buffer)
    }

    /// Lists every `/dev/input/event*` node with its name and role.
    ///
    /// A node that cannot be opened is skipped rather than failing the whole
    /// enumeration: `/dev/input` holds nodes this session has no business
    /// reading, and one `EACCES` must not hide the touchscreen.
    ///
    /// The returned list is sorted by path, so a report of it is stable
    /// between runs.
    pub fn enumerate() -> Result<Vec<InputNode>, DeviceError> {
        let dir = Path::new("/dev/input");
        let entries = fs::read_dir(dir).map_err(|source| DeviceError::io("list", dir, source))?;

        let mut nodes = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
            {
                continue;
            }
            let Ok(fd) = open_node(&path) else {
                continue;
            };
            let name = read_name(&fd);
            let keys = read_bits(&fd, EV_KEY, KEY_BYTES);
            let abs = read_bits(&fd, EV_ABS, ABS_BYTES);
            nodes.push(InputNode {
                path,
                name,
                role: classify(&keys, &abs),
            });
        }
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(nodes)
    }
}

/// Lists input nodes. Always empty off Linux — there are none to list.
#[cfg(not(target_os = "linux"))]
pub fn enumerate() -> Result<Vec<InputNode>, crate::error::DeviceError> {
    Ok(Vec::new())
}

/// The conventional node paths on the image WWW-1 surveyed.
///
/// Recorded as *evidence*, not as an interface. Nothing in this crate reads
/// them; they exist so a device report can say "resolution agreed with what
/// WWW-20 saw" or, more usefully, "it did not".
pub const OBSERVED_PEN_NODE: &str = "/dev/input/event2";
/// See [`OBSERVED_PEN_NODE`].
pub const OBSERVED_TOUCH_NODE: &str = "/dev/input/event3";

/// Whether a path is one of the nodes WWW-20 happened to see.
pub fn is_observed_node(path: &Path) -> bool {
    path == Path::new(OBSERVED_PEN_NODE) || path == Path::new(OBSERVED_TOUCH_NODE)
}

#[cfg(test)]
mod tests {
    use super::{
        Capabilities, InputNode, InputRole, PEN_NAME, Resolution, TOUCH_NAME, classify,
        is_observed_node, resolve, sole,
    };
    use std::path::{Path, PathBuf};

    fn bits(codes: &[u16], size: usize) -> Capabilities {
        let mut bytes = vec![0u8; size];
        for code in codes {
            bytes[*code as usize / 8] |= 1 << (*code % 8);
        }
        Capabilities::from_bytes(bytes)
    }

    fn node(path: &str, name: &str, role: InputRole) -> InputNode {
        InputNode {
            path: PathBuf::from(path),
            name: name.to_owned(),
            role,
        }
    }

    #[test]
    fn capability_lookup_finds_set_bits_and_nothing_else() {
        let caps = bits(&[0x140, 0x74], 96);
        assert!(caps.has(0x140));
        assert!(caps.has(0x74));
        assert!(!caps.has(0x141));
        // Past the end of the bitmap is absent, not a panic.
        assert!(!caps.has(0xFFFF));
        assert!(!Capabilities::default().has(0));
    }

    #[test]
    fn the_pen_wins_over_touch_because_it_reports_positions_too() {
        let keys = bits(&[0x140, 0x141, 0x14A], 96);
        let abs = bits(&[0x00, 0x01, 0x18, 0x35], 8);
        assert_eq!(classify(&keys, &abs), InputRole::Pen);
    }

    #[test]
    fn a_multitouch_node_without_a_pen_button_is_the_touchscreen() {
        let keys = bits(&[0x14A], 96);
        let abs = bits(&[0x2F, 0x35, 0x36, 0x39], 8);
        assert_eq!(classify(&keys, &abs), InputRole::Touch);
    }

    #[test]
    fn the_power_key_and_everything_else_are_distinguished() {
        assert_eq!(
            classify(&bits(&[0x74], 96), &Capabilities::default()),
            InputRole::PowerKey
        );
        assert_eq!(
            classify(&Capabilities::default(), &Capabilities::default()),
            InputRole::Other
        );
    }

    #[test]
    fn resolution_needs_exactly_one_candidate() {
        let nodes = vec![
            node("/dev/input/event2", "pen", InputRole::Pen),
            node("/dev/input/event3", "touch", InputRole::Touch),
            node("/dev/input/event0", "power", InputRole::PowerKey),
        ];
        assert_eq!(
            sole(&nodes, InputRole::Pen).map(|n| n.path.as_path()),
            Some(Path::new("/dev/input/event2"))
        );

        let ambiguous = vec![
            node("/dev/input/event2", "pen", InputRole::Pen),
            node("/dev/input/event9", "other pen", InputRole::Pen),
        ];
        assert!(sole(&ambiguous, InputRole::Pen).is_none());
        assert!(sole(&[], InputRole::Touch).is_none());
    }

    #[test]
    fn resolution_confirms_when_the_name_and_the_capabilities_agree() {
        let nodes = vec![
            node("/dev/input/event2", PEN_NAME, InputRole::Pen),
            node("/dev/input/event3", TOUCH_NAME, InputRole::Touch),
        ];
        assert_eq!(
            resolve(&nodes, InputRole::Pen),
            Resolution::Confirmed(&nodes[0])
        );
        assert_eq!(
            resolve(&nodes, InputRole::Touch),
            Resolution::Confirmed(&nodes[1])
        );
    }

    #[test]
    fn a_renamed_node_is_usable_but_reported() {
        // A firmware update renames the pen. Its capabilities still identify
        // it, so work continues — loudly.
        let nodes = vec![node("/dev/input/event2", "Elan pen v2", InputRole::Pen)];
        let resolution = resolve(&nodes, InputRole::Pen);
        assert_eq!(
            resolution,
            Resolution::NameChanged {
                node: &nodes[0],
                expected: PEN_NAME
            }
        );
        assert_eq!(resolution.node(), Some(&nodes[0]));
    }

    #[test]
    fn the_right_name_on_the_wrong_device_is_refused_rather_than_used() {
        // Both signals cannot be trusted at once, so neither is.
        let nodes = vec![
            node("/dev/input/event2", PEN_NAME, InputRole::Touch),
            node("/dev/input/event3", TOUCH_NAME, InputRole::Touch),
        ];
        let resolution = resolve(&nodes, InputRole::Pen);
        assert_eq!(resolution, Resolution::Contradictory { node: &nodes[0] });
        assert_eq!(resolution.node(), None);
    }

    #[test]
    fn nothing_and_too_much_both_resolve_to_nothing() {
        assert_eq!(resolve(&[], InputRole::Pen), Resolution::Unresolved);
        let two = vec![
            node("/dev/input/event2", PEN_NAME, InputRole::Pen),
            node("/dev/input/event9", "another pen", InputRole::Pen),
        ];
        assert_eq!(resolve(&two, InputRole::Pen), Resolution::Unresolved);
        assert_eq!(resolve(&two, InputRole::Pen).node(), None);
    }

    #[test]
    fn a_role_with_no_confirmed_name_resolves_on_capability_alone() {
        let nodes = vec![node(
            "/dev/input/event0",
            "snvs-powerkey",
            InputRole::PowerKey,
        )];
        assert_eq!(
            resolve(&nodes, InputRole::PowerKey),
            Resolution::Confirmed(&nodes[0])
        );
    }

    #[test]
    fn roles_render_the_same_words_the_c_probe_prints() {
        assert_eq!(InputRole::Pen.to_string(), "pen");
        assert_eq!(InputRole::Touch.to_string(), "touch");
        assert_eq!(InputRole::PowerKey.to_string(), "power-key");
        assert_eq!(InputRole::Other.to_string(), "other");
    }

    #[test]
    fn the_observed_nodes_are_recorded_but_decide_nothing() {
        assert!(is_observed_node(Path::new("/dev/input/event2")));
        assert!(!is_observed_node(Path::new("/dev/input/event7")));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn enumeration_off_linux_finds_nothing_rather_than_failing() {
        assert!(super::enumerate().expect("succeeds").is_empty());
    }
}
