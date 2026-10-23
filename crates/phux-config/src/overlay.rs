//! Best-effort overlay-network address detection for `phux pair` (ADR-0037).
//!
//! `$PHUX_OVERLAY_ADDRS` explicitly selects addresses for any private network
//! (including Defguard/WireGuard); when set it is authoritative, even empty or
//! invalid. Otherwise the source is `tailscale ip -4` (overridable via
//! `$PHUX_TAILSCALE`), with a fallback UDP route probe reported only inside the
//! Tailscale CGNAT range. The probe runs only when `$PHUX_TAILSCALE` is unset.
//! Every failure degrades to detecting nothing.
//!
//! [`tailscale_cli`] finds that CLI where a service's or app's `PATH` would
//! miss it. [`detect_magic_dns`] reads the same CLI's `status --json` for the host's
//! `MagicDNS` name, so a pairing link can name the host rather than an address
//! the tailnet may reassign.

use std::net::IpAddr;

/// Detect the host's overlay-network addresses, best effort (empty when
/// nothing is detected).
#[must_use]
pub fn detect() -> Vec<IpAddr> {
    detect_with_addresses(std::env::var_os("PHUX_OVERLAY_ADDRS").as_deref(), || {
        detect_from_override(std::env::var_os("PHUX_TAILSCALE").as_deref())
    })
}

/// Explicit addresses replace discovery, never augment it. In particular,
/// an invalid configuration must not expose a listener on a different network.
fn detect_with_addresses(
    addresses: Option<&std::ffi::OsStr>,
    discover: impl FnOnce() -> Vec<IpAddr>,
) -> Vec<IpAddr> {
    let Some(addresses) = addresses else {
        return discover();
    };
    addresses
        .to_str()
        .and_then(parse_explicit_addresses)
        .unwrap_or_else(|| {
            tracing::warn!(
                "invalid PHUX_OVERLAY_ADDRS; use comma-separated concrete unicast IPs \
             (no CIDRs, ports, or link-local addresses); overlay discovery is disabled"
            );
            Vec::new()
        })
}

/// Parse a comma-separated list atomically, preserving preference order.
/// Empty means disabled; one bad item refuses the whole list. These are
/// concrete unicast addresses, not interface names, CIDRs, or scoped IPv6.
fn parse_explicit_addresses(raw: &str) -> Option<Vec<IpAddr>> {
    let mut addresses = Vec::new();
    if raw.trim().is_empty() {
        return Some(addresses);
    }
    for item in raw.split(',') {
        let address: IpAddr = item.trim().parse().ok()?;
        if !is_explicit_unicast(address) {
            return None;
        }
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    Some(addresses)
}

/// Link-local IPv6 needs a scope the address-only detector cannot carry.
/// Reject mapped IPv4 too, so loopback/unspecified checks cannot be bypassed.
const fn is_explicit_unicast(address: IpAddr) -> bool {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return false;
    }
    match address {
        IpAddr::V4(address) => !address.is_broadcast() && !address.is_link_local(),
        IpAddr::V6(address) => {
            !address.is_unicast_link_local() && address.to_ipv4_mapped().is_none()
        }
    }
}

/// The host's Tailscale `MagicDNS` identity: its fully qualified name and the
/// tailnet addresses that name resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicDns {
    /// The fully qualified `MagicDNS` name, without the trailing dot
    /// (`studio.tail9a8b.ts.net`).
    pub name: String,
    /// The node's tailnet addresses: the only addresses the name stands for.
    pub addresses: Vec<IpAddr>,
}

impl MagicDns {
    /// Whether the name resolves to `address` on the tailnet.
    #[must_use]
    pub fn names(&self, address: IpAddr) -> bool {
        self.addresses.contains(&address)
    }
}

/// Detect the host's `MagicDNS` name from `tailscale status --json`, best
/// effort.
///
/// `None` when `$PHUX_OVERLAY_ADDRS` is set (explicit addresses are
/// authoritative and may not be Tailscale's), when the CLI is missing, fails,
/// or is wedged, when the tailnet has `MagicDNS` disabled, or when the output
/// names no DNS name or address. `$PHUX_TAILSCALE` substitutes the CLI, as
/// for [`detect`].
#[must_use]
pub fn detect_magic_dns() -> Option<MagicDns> {
    if std::env::var_os("PHUX_OVERLAY_ADDRS").is_some() {
        return None;
    }
    let program = tailscale_cli()?;
    run_tailscale(
        program.as_os_str(),
        &["status", "--json"],
        TAILSCALE_DEADLINE,
    )
    .as_deref()
    .and_then(parse_tailscale_status)
}

/// Parse `tailscale status --json`: `Self.DNSName` (trailing dot trimmed)
/// and `Self.TailscaleIPs`, only when `CurrentTailnet.MagicDNSEnabled` is
/// true. A name the tailnet does not serve would pair a device with a host
/// it cannot resolve.
fn parse_tailscale_status(out: &str) -> Option<MagicDns> {
    let status: serde_json::Value = serde_json::from_str(out).ok()?;
    let enabled = status
        .pointer("/CurrentTailnet/MagicDNSEnabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    let me = status.get("Self")?;
    let name = me
        .get("DNSName")?
        .as_str()?
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !is_dns_name(&name) {
        return None;
    }
    let addresses: Vec<IpAddr> = me
        .get("TailscaleIPs")?
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str()?.parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .collect();
    if addresses.is_empty() {
        return None;
    }
    Some(MagicDns { name, addresses })
}

/// A dotted DNS name a link can carry as a URL host unescaped: two or more
/// labels of letters, digits, and inner hyphens, at most 253 bytes.
fn is_dns_name(name: &str) -> bool {
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    name.len() <= 253
        && name.contains('.')
        && name.parse::<IpAddr>().is_err()
        && name.split('.').all(label_ok)
}

/// [`detect`] with the `$PHUX_TAILSCALE` value injected for tests.
fn detect_from_override(program: Option<&std::ffi::OsStr>) -> Vec<IpAddr> {
    let Some(program) = program else {
        return detect_with(
            || run_tailscale_ip(tailscale_cli()?.as_os_str()),
            cgnat_route_probe,
        );
    };
    detect_with(|| run_tailscale_ip(program), || None)
}

/// [`detect`] with both sources injectable.
fn detect_with(
    tailscale: impl Fn() -> Option<String>,
    route_probe: impl Fn() -> Option<IpAddr>,
) -> Vec<IpAddr> {
    let addrs = tailscale()
        .map(|out| parse_tailscale_ip_output(&out))
        .unwrap_or_default();
    if !addrs.is_empty() {
        return addrs;
    }
    route_probe()
        .filter(|ip| is_cgnat(*ip))
        .into_iter()
        .collect()
}

/// How long a `tailscale` subcommand may run before it is killed: a wedged
/// tailscaled must never hang `phux pair`.
const TAILSCALE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Run `<program> ip -4` and return its stdout, or `None` when it is missing,
/// fails, or outlives [`TAILSCALE_DEADLINE`] (it is then killed).
fn run_tailscale_ip(program: &std::ffi::OsStr) -> Option<String> {
    run_tailscale(program, &["ip", "-4"], TAILSCALE_DEADLINE)
}

/// Run `<program> <args>` and return its stdout, or `None` when it is
/// missing, fails, or outlives `deadline` (it is then killed).
fn run_tailscale(
    program: &std::ffi::OsStr,
    args: &[&str],
    deadline: std::time::Duration,
) -> Option<String> {
    run_tailscale_capture(program, args, deadline)
        .filter(|(success, _)| *success)
        .map(|(_, stdout)| stdout)
}

/// [`run_tailscale`], keeping a failing run's stdout: `(success, stdout)`.
/// `None` only when the program is missing or outlives `deadline`.
pub(crate) fn run_tailscale_capture(
    program: &std::ffi::OsStr,
    args: &[&str],
    deadline: std::time::Duration,
) -> Option<(bool, String)> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Drain stdout while the command runs: `status --json` lists every peer
    // and can outgrow a pipe buffer, which would stall the CLI until killed.
    let mut stdout = child.stdout.take()?;
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = std::io::Read::read_to_end(&mut stdout, &mut buf).map(|_| buf);
        let _ = sender.send(read);
    });

    let deadline = std::time::Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            // Deadline expired: kill and reap so no zombie outlives us. The
            // reader ends when the last holder of the pipe exits; it is not
            // waited for.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // A descendant that inherited the pipe could hold it open past the exit;
    // the deadline bounds the wait for end-of-file too.
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let buf = output.recv_timeout(remaining).ok()?.ok()?;
    Some((status.success(), String::from_utf8_lossy(&buf).into_owned()))
}

/// The Tailscale CLI to run.
///
/// `$PHUX_TAILSCALE` when set (even to a missing program), else `tailscale`
/// on `PATH`, else where the installers put it. A server started by launchd
/// or systemd, or an app opened from the Dock,
/// has a `PATH` that misses Homebrew and the macOS app's bundled CLI.
#[must_use]
pub fn tailscale_cli() -> Option<std::path::PathBuf> {
    if let Some(program) = std::env::var_os("PHUX_TAILSCALE") {
        return Some(program.into());
    }
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("tailscale"))
            .collect::<Vec<_>>()
    });
    on_path
        .chain(KNOWN_TAILSCALE_CLIS.iter().map(std::path::PathBuf::from))
        .find(|candidate| is_executable(candidate))
}

/// Where the Tailscale installers put the CLI when it is not on `PATH`.
const KNOWN_TAILSCALE_CLIS: &[&str] = &[
    "/opt/homebrew/bin/tailscale",
    "/usr/local/bin/tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/usr/bin/tailscale",
    "/snap/bin/tailscale",
];

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Parse `tailscale ip` output: one address per line, tolerating v6 lines
/// and garbage, dropping loopback and unspecified addresses.
fn parse_tailscale_ip_output(out: &str) -> Vec<IpAddr> {
    out.lines()
        .filter_map(|line| line.trim().parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .collect()
}

/// Ask the kernel which source address it would route toward the Tailscale
/// service IP. `connect` on UDP sends no packets; it only selects a route.
fn cgnat_route_probe() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    sock.connect(("100.100.100.100", 1)).ok()?;
    sock.local_addr().ok().map(|addr| addr.ip())
}

/// Whether `ip` sits inside the CGNAT range Tailscale assigns from,
/// `100.64.0.0/10`.
fn is_cgnat(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
mod tests;
