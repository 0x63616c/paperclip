//! The resolution order WWW-33 fixes: `--device`, `PAPERCTL_DEVICE`, a pin,
//! USB ethernet, mDNS, then the cached last-good host.
//!
//! Explicit sources (`--device`, the environment variable, a pin) are taken
//! on trust and never probed here — a pin is allowed to name a tablet that is
//! asleep right now, which is the whole point of pinning one. Auto-discovery
//! sources (USB, mDNS, cache) are probed for reachability in order, and the
//! first one that answers wins.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// The static address the tablet's USB ethernet gadget answers on.
pub(crate) const USB_HOST: &str = "10.11.99.1";

/// The mDNS service Paperclip is discoverable under — proposed, not yet
/// validated on hardware: nothing in this repository advertises it from the
/// device side yet, so this source is expected to find nothing until a later
/// stage adds that. Recorded here rather than guessed at again later.
const MDNS_SERVICE: &str = "_paperctl._tcp.local.";

/// SSH's own port, and what reachability is measured against — a device
/// transport that cannot reach the SSH port cannot do anything else either.
const SSH_PORT: u16 = 22;

/// How long one reachability probe may spend in total, retries included —
/// not a single connection attempt. WWW-35, on hardware: a 2s bound (this
/// module's original value, chosen for a 15s "fails fast" budget) reported
/// a merely *sleeping* tablet as gone. The shipped IW612 Wi-Fi driver wakes
/// on an inbound TCP SYN and never on ICMP, but coming out of deep sleep and
/// actually accepting the connection takes real time that a single short
/// `connect_timeout` never gave it. [`SystemProber`] spends this budget as
/// several short attempts with backoff rather than one long call — a SYN
/// sent while still asleep gets no answer at all, so only a *later* attempt
/// has any chance of landing after the wake.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The bound `doctor` and `deploy` use for their own resolution, instead of
/// [`PROBE_TIMEOUT`].
///
/// `open`, `stock` and friends need that 30s budget to wake a sleeping
/// tablet's Wi-Fi (WWW-35). `doctor` and `deploy` cannot spend it and still
/// meet their own 15s "no device reachable" failure bound (WWW-34 acceptance
/// criterion 8), so they accept the cheaper, honest trade instead: a
/// sleeping tablet reads as unreachable rather than waiting to find out. A
/// person who wants a diagnostic against a sleeping tablet wakes it first.
// Mac-only: `deploy`, `doctor` and `remote` are the only callers and all
// three are `#[cfg(not(target_os = "linux"))]` (main.rs, transport/mod.rs), so
// on a device build this is dead rather than merely unused. Gated for the same
// reason `remote` and `runlog` are, rather than left to look reachable.
#[cfg(not(target_os = "linux"))]
pub(crate) const QUICK_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// The bound `devices` uses to *enumerate*, instead of [`PROBE_TIMEOUT`].
///
/// [`discover_all`] probes each candidate in turn, so a per-candidate budget
/// is paid once per candidate rather than once per run. At [`PROBE_TIMEOUT`]
/// that made `paperctl devices` take 33s on a Mac with no tablet plugged in —
/// 30s of backoff against an absent USB host, then the mDNS browse — printing
/// nothing until it finished, which reads as a hang rather than as a slow
/// answer.
///
/// Listing is not reaching: nothing is about to be done to the tablet, so the
/// wake budget buys nothing here. This is the same trade
/// [`QUICK_PROBE_TIMEOUT`] makes for `doctor` and `deploy`, for the same
/// reason. `devices --wake` spends [`PROBE_TIMEOUT`] instead, for the one case
/// where the question really is "is there a *sleeping* tablet out there".
///
/// Ungated, unlike [`QUICK_PROBE_TIMEOUT`]: `devices` is built on the device
/// too, so a Mac-only constant would not compile there.
pub(crate) const LIST_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// How long any single connection attempt inside [`PROBE_TIMEOUT`]'s budget
/// gets. Short and repeated, not long and singular — see [`PROBE_TIMEOUT`].
const CONNECT_ATTEMPT: Duration = Duration::from_secs(2);

/// How long the mDNS browse listens before giving up.
pub(crate) const MDNS_TIMEOUT: Duration = Duration::from_secs(3);

/// Where a resolved (or listed) host came from, in resolution-order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DeviceSource {
    #[cfg(not(target_os = "linux"))]
    Flag,
    Env,
    Pinned,
    Usb,
    Mdns,
    Cache,
}

impl std::fmt::Display for DeviceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(not(target_os = "linux"))]
            Self::Flag => "--device flag",
            Self::Env => "PAPERCTL_DEVICE",
            Self::Pinned => "pinned config",
            Self::Usb => "USB ethernet",
            Self::Mdns => "mDNS",
            Self::Cache => "cached last-good host",
        })
    }
}

/// A candidate host, as reported by `paperctl devices`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Discovered {
    pub(crate) host: String,
    pub(crate) source: DeviceSource,
    pub(crate) reachable: bool,
    pub(crate) pinned: bool,
}

/// Probes reachability and enumerates discoverable hosts. A trait so
/// `resolve` and `discover_all` are testable without a network — see the
/// `FakeProber` in this module's tests.
pub(crate) trait Prober {
    fn reachable(&self, host: &str, timeout: Duration) -> bool;
    fn mdns_candidates(&self, timeout: Duration) -> Vec<String>;
}

/// The real prober: a TCP connect to port 22, and an actual mDNS browse.
///
/// Never ICMP. WWW-35 confirmed the shipped IW612 driver's wake filter
/// answers inbound TCP and ignores ICMP entirely, so a `ping`-based probe
/// would never wake a sleeping tablet and would always read as unreachable.
pub(crate) struct SystemProber;

impl Prober for SystemProber {
    fn reachable(&self, host: &str, budget: Duration) -> bool {
        let Some(target) = resolvable_host(host) else {
            return false;
        };
        let Ok(addrs) = (target.as_str(), SSH_PORT).to_socket_addrs() else {
            return false;
        };
        probe_with_backoff(&addrs.collect::<Vec<_>>(), budget)
    }

    fn mdns_candidates(&self, timeout: Duration) -> Vec<String> {
        mdns_browse(timeout)
    }
}

/// `host` with any `user@` stripped. The probe opens a TCP socket, which has
/// no notion of who will log in — only [`super::remote::ssh_target`]'s side
/// cares. `to_socket_addrs` treats the whole string as a hostname, so
/// `root@192.168.0.180` fails to parse and reads as unreachable.
fn bare_host(host: &str) -> &str {
    host.rsplit_once('@').map_or(host, |(_, host)| host)
}

/// The name this machine can actually resolve for `host`, or `None`.
///
/// `--device` documents itself as taking a host "as `ssh` would take it", and
/// the pin, the cache and `PAPERCLIP_DEVICE` all accept the same thing — but
/// the probe resolves the string itself rather than handing it to `ssh`, so
/// two of the forms ssh accepts never worked:
///
/// - `root@host`, which `to_socket_addrs` cannot parse; and
/// - a `~/.ssh/config` alias such as `remarkable-wifi`, which only `ssh`
///   knows and the system resolver has never heard of.
///
/// Both failed in tens of milliseconds with `reachable=false` — indis-
/// tinguishable, to a reader, from a tablet that is switched off. Diagnosed
/// 2026-09-18 against a tablet that was demonstrably up and answering `ssh`.
///
/// So: strip the login, then, if the name still does not resolve, ask `ssh`
/// what it would use. `ssh -G` prints the fully-expanded config for a target
/// without connecting, so this costs a local process and no network. When no
/// `Host` block matches, `ssh -G` echoes the name back unchanged — that is
/// not a translation, and the name did not resolve a moment ago, so it is
/// rejected rather than retried.
fn resolvable_host(host: &str) -> Option<String> {
    let bare = bare_host(host);
    if (bare, SSH_PORT).to_socket_addrs().is_ok() {
        return Some(bare.to_owned());
    }
    ssh_config_hostname(bare)
}

/// The `hostname` `ssh -G <host>` reports, when it differs from `host`.
fn ssh_config_hostname(host: &str) -> Option<String> {
    let output = std::process::Command::new("ssh")
        .arg("-G")
        .arg(host)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map(str::trim)
        .filter(|resolved| !resolved.is_empty() && !resolved.eq_ignore_ascii_case(host))
        .map(str::to_owned)
}

/// Retries a TCP connect to any of `addrs` with backoff across `budget`,
/// rather than one long `connect_timeout` — see [`PROBE_TIMEOUT`] for why.
/// Factored out of [`SystemProber::reachable`] so the retry shape is
/// testable against a local listener without needing port 22.
fn probe_with_backoff(addrs: &[SocketAddr], budget: Duration) -> bool {
    if addrs.is_empty() {
        return false;
    }
    let deadline = Instant::now() + budget;
    let mut backoff = Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let attempt = CONNECT_ATTEMPT.min(remaining);
        if addrs
            .iter()
            .any(|addr| TcpStream::connect_timeout(addr, attempt).is_ok())
        {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

#[cfg(feature = "discovery")]
fn mdns_browse(timeout: Duration) -> Vec<String> {
    use std::time::Instant;

    use mdns_sd::{ServiceDaemon, ServiceEvent};

    let Ok(daemon) = ServiceDaemon::new() else {
        return Vec::new();
    };
    let Ok(receiver) = daemon.browse(MDNS_SERVICE) else {
        let _ = daemon.shutdown();
        return Vec::new();
    };

    let deadline = Instant::now() + timeout;
    let mut hosts = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(resolved)) => {
                hosts.extend(
                    resolved
                        .addresses
                        .iter()
                        .map(|addr| addr.to_ip_addr().to_string()),
                );
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    hosts.sort();
    hosts.dedup();
    hosts
}

#[cfg(not(feature = "discovery"))]
fn mdns_browse(_timeout: Duration) -> Vec<String> {
    Vec::new()
}

/// What `resolve` and `discover_all` are given to work with. Everything here
/// is already read from its own source (the flag, the environment, the
/// config) — this module only orders and probes it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Inputs {
    #[cfg(not(target_os = "linux"))]
    pub(crate) flag: Option<String>,
    pub(crate) env: Option<String>,
    pub(crate) pinned: Option<String>,
    pub(crate) usb: Option<String>,
    pub(crate) cache: Option<String>,
}

/// No device could be resolved. `tried` names every source in resolution
/// order, so the message is useful without a debugger.
// Mac-only: `deploy`, `doctor` and `remote` are the only callers and all
// three are `#[cfg(not(target_os = "linux"))]` (main.rs, transport/mod.rs), so
// on a device build this is dead rather than merely unused. Gated for the same
// reason `remote` and `runlog` are, rather than left to look reachable.
#[cfg(not(target_os = "linux"))]
#[derive(Debug, thiserror::Error)]
#[error("no tablet found; tried, in order:\n{}", .tried.iter().map(|line| format!("  {line}")).collect::<Vec<_>>().join("\n"))]
pub(crate) struct NoDeviceFound {
    pub(crate) tried: Vec<String>,
}

/// Resolves one device, per the order in the issue: flag, env, pin, then
/// discovery (USB, mDNS, cache) in turn, taking the first reachable one.
#[cfg(not(target_os = "linux"))]
pub(crate) fn resolve(
    inputs: &Inputs,
    prober: &dyn Prober,
    timeout: Duration,
) -> Result<(String, DeviceSource), NoDeviceFound> {
    if let Some(host) = &inputs.flag {
        return Ok((host.clone(), DeviceSource::Flag));
    }
    if let Some(host) = &inputs.env {
        return Ok((host.clone(), DeviceSource::Env));
    }
    if let Some(host) = &inputs.pinned {
        return Ok((host.clone(), DeviceSource::Pinned));
    }

    let mut tried = vec![
        "--device flag: not given".to_owned(),
        "PAPERCTL_DEVICE: not set".to_owned(),
        "pinned config: none".to_owned(),
    ];

    if let Some(host) = &inputs.usb {
        if prober.reachable(host, timeout) {
            return Ok((host.clone(), DeviceSource::Usb));
        }
        tried.push(format!("USB ethernet {host}: unreachable"));
    } else {
        tried.push("USB ethernet: no candidate".to_owned());
    }

    let mdns = prober.mdns_candidates(MDNS_TIMEOUT);
    if mdns.is_empty() {
        tried.push("mDNS on the LAN: no responses".to_owned());
    } else {
        for host in &mdns {
            if prober.reachable(host, timeout) {
                return Ok((host.clone(), DeviceSource::Mdns));
            }
        }
        tried.push(format!(
            "mDNS on the LAN: {} found, none reachable",
            mdns.len()
        ));
    }

    if let Some(host) = &inputs.cache {
        if prober.reachable(host, timeout) {
            return Ok((host.clone(), DeviceSource::Cache));
        }
        tried.push(format!("cached last-good host {host}: unreachable"));
    } else {
        tried.push("cached last-good host: none".to_owned());
    }

    Err(NoDeviceFound { tried })
}

/// Every candidate `paperctl devices` knows about, each probed once. Unlike
/// `resolve`, this does not stop at the first reachable host — the whole
/// point is showing what else is out there.
pub(crate) fn discover_all(
    inputs: &Inputs,
    prober: &dyn Prober,
    timeout: Duration,
) -> Vec<Discovered> {
    let mut found: Vec<Discovered> = Vec::new();
    let record = |host: String, source: DeviceSource, found: &mut Vec<Discovered>| {
        if found.iter().any(|entry| entry.host == host) {
            return;
        }
        let reachable = prober.reachable(&host, timeout);
        let pinned = inputs.pinned.as_deref() == Some(host.as_str());
        found.push(Discovered {
            host,
            source,
            reachable,
            pinned,
        });
    };

    if let Some(host) = &inputs.env {
        record(host.clone(), DeviceSource::Env, &mut found);
    }
    if let Some(host) = &inputs.pinned {
        record(host.clone(), DeviceSource::Pinned, &mut found);
    }
    if let Some(host) = &inputs.usb {
        record(host.clone(), DeviceSource::Usb, &mut found);
    }
    for host in prober.mdns_candidates(MDNS_TIMEOUT) {
        record(host, DeviceSource::Mdns, &mut found);
    }
    if let Some(host) = &inputs.cache {
        record(host.clone(), DeviceSource::Cache, &mut found);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::test_doubles::FakeProber;

    // `resolve` and the `--device` flag are Mac-only, so the resolution-order
    // tests below and this helper do not exist on a device build. The
    // `discover_all` and probing tests are not gated: both halves run there.
    #[cfg(not(target_os = "linux"))]
    fn inputs() -> Inputs {
        Inputs {
            flag: None,
            env: None,
            pinned: None,
            usb: Some(USB_HOST.to_owned()),
            cache: None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn flag_beats_everything() {
        let mut inputs = inputs();
        inputs.flag = Some("flag-host".to_owned());
        inputs.env = Some("env-host".to_owned());
        inputs.pinned = Some("pinned-host".to_owned());
        let prober = FakeProber::reachable_hosts(&["env-host", "pinned-host", USB_HOST]);

        let (host, source) = resolve(&inputs, &prober, PROBE_TIMEOUT).expect("resolves");
        assert_eq!(host, "flag-host");
        assert_eq!(source, DeviceSource::Flag);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn env_beats_pinned_and_discovery() {
        let mut inputs = inputs();
        inputs.env = Some("env-host".to_owned());
        inputs.pinned = Some("pinned-host".to_owned());
        let prober = FakeProber::reachable_hosts(&["pinned-host", USB_HOST]);

        let (host, source) = resolve(&inputs, &prober, PROBE_TIMEOUT).expect("resolves");
        assert_eq!(host, "env-host");
        assert_eq!(source, DeviceSource::Env);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn pinned_beats_usb() {
        let mut inputs = inputs();
        inputs.pinned = Some("pinned-host".to_owned());
        let prober = FakeProber::reachable_hosts(&[USB_HOST]);

        let (host, source) = resolve(&inputs, &prober, PROBE_TIMEOUT).expect("resolves");
        assert_eq!(host, "pinned-host");
        assert_eq!(source, DeviceSource::Pinned);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn pinned_is_used_even_when_unreachable() {
        // A pin is a promise to skip discovery, not a promise the tablet is
        // awake right now.
        let mut inputs = inputs();
        inputs.pinned = Some("asleep-host".to_owned());
        let prober = FakeProber::reachable_hosts(&[USB_HOST]);

        let (host, source) = resolve(&inputs, &prober, PROBE_TIMEOUT).expect("resolves");
        assert_eq!(host, "asleep-host");
        assert_eq!(source, DeviceSource::Pinned);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn usb_beats_mdns_and_cache() {
        let mut inputs = inputs();
        inputs.cache = Some("cache-host".to_owned());
        let prober = FakeProber::reachable_hosts(&[USB_HOST, "mdns-host", "cache-host"])
            .with_mdns_candidates(&["mdns-host"]);

        let (host, source) = resolve(&inputs, &prober, PROBE_TIMEOUT).expect("resolves");
        assert_eq!(host, USB_HOST);
        assert_eq!(source, DeviceSource::Usb);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn mdns_beats_cache_when_usb_is_unreachable() {
        let mut inputs = inputs();
        inputs.cache = Some("cache-host".to_owned());
        let prober = FakeProber::reachable_hosts(&["mdns-host", "cache-host"])
            .with_mdns_candidates(&["mdns-host"]);

        let (host, source) = resolve(&inputs, &prober, PROBE_TIMEOUT).expect("resolves");
        assert_eq!(host, "mdns-host");
        assert_eq!(source, DeviceSource::Mdns);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn cache_is_the_last_resort() {
        let mut inputs = inputs();
        inputs.cache = Some("cache-host".to_owned());
        let prober = FakeProber::reachable_hosts(&["cache-host"]);

        let (host, source) = resolve(&inputs, &prober, PROBE_TIMEOUT).expect("resolves");
        assert_eq!(host, "cache-host");
        assert_eq!(source, DeviceSource::Cache);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn nothing_found_names_every_source_tried() {
        let inputs = inputs();
        let prober = FakeProber::default();

        let error = resolve(&inputs, &prober, PROBE_TIMEOUT).unwrap_err();
        assert_eq!(error.tried.len(), 6, "all six sources should be named");
        assert!(
            error
                .tried
                .iter()
                .any(|line| line.contains("--device flag"))
        );
        assert!(
            error
                .tried
                .iter()
                .any(|line| line.contains("PAPERCTL_DEVICE"))
        );
        assert!(
            error
                .tried
                .iter()
                .any(|line| line.contains("pinned config"))
        );
        assert!(error.tried.iter().any(|line| line.contains("USB ethernet")));
        assert!(error.tried.iter().any(|line| line.contains("mDNS")));
        assert!(
            error
                .tried
                .iter()
                .any(|line| line.contains("cached last-good"))
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn probes_are_bounded_by_the_given_timeout() {
        let inputs = inputs();
        let prober = FakeProber::default();

        let _ = resolve(&inputs, &prober, PROBE_TIMEOUT);

        for (_, timeout) in prober.reachable_calls() {
            assert!(timeout <= PROBE_TIMEOUT);
        }
        for timeout in prober.mdns_calls() {
            assert!(timeout <= MDNS_TIMEOUT);
        }
    }

    #[test]
    fn a_login_prefix_is_not_part_of_the_hostname() {
        // Every one of these is a form `--device`, a pin, the cache or
        // `PAPERCLIP_DEVICE` documents itself as accepting.
        assert_eq!(bare_host("root@192.168.0.180"), "192.168.0.180");
        assert_eq!(bare_host("192.168.0.180"), "192.168.0.180");
        assert_eq!(
            bare_host("root@imx8mm-ferrari.localdomain"),
            "imx8mm-ferrari.localdomain"
        );
        assert_eq!(bare_host("remarkable-wifi"), "remarkable-wifi");
    }

    #[test]
    fn a_bare_ip_resolves_without_consulting_ssh() {
        // The fast path: no `ssh -G`, no config, no network. Guards against a
        // regression that made every probe pay for a subprocess.
        assert_eq!(
            resolvable_host("root@127.0.0.1").as_deref(),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn discover_all_lists_every_source_once_each() {
        let inputs = Inputs {
            #[cfg(not(target_os = "linux"))]
            flag: None,
            env: Some("env-host".to_owned()),
            pinned: Some("pinned-host".to_owned()),
            usb: Some(USB_HOST.to_owned()),
            cache: Some("cache-host".to_owned()),
        };
        let prober = FakeProber::reachable_hosts(&["pinned-host", USB_HOST])
            .with_mdns_candidates(&["mdns-host"]);

        let all = discover_all(&inputs, &prober, PROBE_TIMEOUT);
        let hosts: Vec<&str> = all.iter().map(|d| d.host.as_str()).collect();
        assert_eq!(
            hosts,
            vec![
                "env-host",
                "pinned-host",
                USB_HOST,
                "mdns-host",
                "cache-host"
            ]
        );

        let pinned = all.iter().find(|d| d.host == "pinned-host").unwrap();
        assert!(pinned.pinned);
        assert!(pinned.reachable);

        let usb = all.iter().find(|d| d.host == USB_HOST).unwrap();
        assert!(!usb.pinned);
        assert!(usb.reachable);

        let mdns = all.iter().find(|d| d.host == "mdns-host").unwrap();
        assert!(!mdns.reachable);
    }

    #[test]
    fn discover_all_deduplicates_a_host_seen_from_two_sources() {
        let inputs = Inputs {
            #[cfg(not(target_os = "linux"))]
            flag: None,
            env: None,
            pinned: Some(USB_HOST.to_owned()),
            usb: Some(USB_HOST.to_owned()),
            cache: None,
        };
        let prober = FakeProber::reachable_hosts(&[USB_HOST]);

        let all = discover_all(&inputs, &prober, PROBE_TIMEOUT);
        assert_eq!(all.len(), 1, "the same host should appear once");
        assert_eq!(
            all[0].source,
            DeviceSource::Pinned,
            "higher precedence wins the label"
        );
    }

    #[test]
    fn backoff_probing_finds_a_real_listener_without_waiting_for_a_retry() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds a local port");
        let addr = listener.local_addr().expect("has an address");
        std::thread::spawn(move || {
            // Accept once so `connect_timeout` completes the handshake rather
            // than landing in a backlog nobody drains.
            let _ = listener.accept();
        });

        let started = Instant::now();
        assert!(
            probe_with_backoff(&[addr], Duration::from_secs(5)),
            "a real listener must be found"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a live listener must not wait for a backoff retry"
        );
    }

    #[test]
    fn backoff_probing_gives_up_at_the_budget_rather_than_hanging_past_it() {
        // A closed local port refuses every attempt, so the loop must still
        // return by the deadline rather than retrying forever.
        let closed: SocketAddr = "127.0.0.1:1".parse().expect("a valid address");
        let budget = Duration::from_millis(900);

        let started = Instant::now();
        assert!(!probe_with_backoff(&[closed], budget));
        assert!(
            started.elapsed() < budget * 3,
            "must not overrun its budget by more than a couple of backoff steps: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn backoff_probing_reports_unreachable_with_no_addresses_rather_than_looping() {
        assert!(!probe_with_backoff(&[], Duration::from_secs(1)));
    }
}
