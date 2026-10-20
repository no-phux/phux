//! Dial specs and link planning: the SSH argv, transport trust, and the link token.

use super::{
    CertTrust, HubEntry, LINK_IDLE_TIMEOUT, LINK_KEEPALIVE_INTERVAL, LinkRefusal, Path, PathBuf,
    SatelliteTarget, WsTarget,
};

/// What the planner decided to dial. The token stays a path, re-read on
/// every attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialSpec {
    /// Dial a QUIC listener (`quic://host:port`).
    Quic {
        /// Hostname, IPv4, or bracketed IPv6 literal to resolve.
        host: String,
        /// UDP port.
        port: u16,
        /// Certificate trust: pinned for routable, skip for loopback dev.
        trust: CertTrust,
        /// Pairing-token file, read per attempt.
        token_file: Option<PathBuf>,
    },
    /// Dial a WebSocket listener (`ws://` loopback dev or `wss://`).
    Ws {
        /// The full endpoint URL as configured.
        url: String,
        /// Certificate trust (`wss://` only).
        trust: CertTrust,
        /// Pairing-token file, read per attempt.
        token_file: Option<PathBuf>,
    },
    /// Spawn `ssh` bridging to the satellite's UDS via `phux stdio-bridge`.
    /// No auth material: SSH authenticates the channel.
    Ssh {
        /// Login user (`-l`), if configured.
        user: Option<String>,
        /// Destination host (bare — IPv6 without brackets).
        host: String,
        /// SSH port (`-p`), if configured.
        port: Option<u16>,
    },
    /// Dial nothing: adopt the connection the satellite dials in with
    /// (ADR-0182). Its certificate is the credential, so no token or pin.
    Inbound,
}

impl DialSpec {
    /// The token file this spec dials with (`None` for SSH and inbound).
    #[must_use]
    pub fn token_file(&self) -> Option<&Path> {
        match self {
            Self::Quic { token_file, .. } | Self::Ws { token_file, .. } => token_file.as_deref(),
            Self::Ssh { .. } | Self::Inbound => None,
        }
    }
}

impl core::fmt::Display for DialSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Quic { host, port, .. } => write!(f, "quic://{host}:{port}"),
            Self::Ws { url, .. } => f.write_str(url),
            Self::Ssh { user, host, port } => {
                f.write_str("ssh://")?;
                if let Some(user) = user {
                    write!(f, "{user}@")?;
                }
                if host.contains(':') {
                    write!(f, "[{host}]")?;
                } else {
                    f.write_str(host)?;
                }
                if let Some(port) = port {
                    write!(f, ":{port}")?;
                }
                Ok(())
            }
            Self::Inbound => f.write_str(crate::hub::INBOUND_ENDPOINT),
        }
    }
}

/// Build the argv (excluding the program) for one SSH-stdio dial.
///
/// Shell-free, host/user charset-validated at parse time, and `--` before
/// the host anyway. `BatchMode=yes` fails fast without a key, `-T` refuses a
/// remote PTY, and `ClearAllForwardings=yes` ignores config forwardings.
/// `ServerAliveInterval`/`ServerAliveCountMax` implement the link's
/// keepalive/idle contract at the SSH layer.
#[must_use]
pub fn ssh_argv(user: Option<&str>, host: &str, port: Option<u16>) -> Vec<String> {
    // Derived so the SSH liveness window matches the WS/QUIC one.
    let alive_interval = LINK_KEEPALIVE_INTERVAL.as_secs().max(1);
    let alive_count = (LINK_IDLE_TIMEOUT.as_secs() / alive_interval).max(1);
    let mut argv = vec![
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ClearAllForwardings=yes".to_owned(),
        "-o".to_owned(),
        format!("ServerAliveInterval={alive_interval}"),
        "-o".to_owned(),
        format!("ServerAliveCountMax={alive_count}"),
        "-T".to_owned(),
    ];
    if let Some(port) = port {
        argv.push("-p".to_owned());
        argv.push(port.to_string());
    }
    if let Some(user) = user {
        argv.push("-l".to_owned());
        argv.push(user.to_owned());
    }
    argv.push("--".to_owned());
    argv.push(host.to_owned());
    argv.push("phux".to_owned());
    argv.push("stdio-bridge".to_owned());
    argv
}

/// Decide how (or whether) to dial one hub-table entry. Pure.
///
/// The fail-closed gate (ADR-0038): routable endpoints need a token file and
/// a pin; plaintext `ws://` is loopback-only; loopback keeps the dev
/// carve-out. `ssh://` needs neither, and ignores any configured.
/// # Errors
///
/// A [`LinkRefusal`] naming the configuration gap.
pub fn plan_link(entry: &HubEntry) -> Result<DialSpec, LinkRefusal> {
    match &entry.target {
        SatelliteTarget::Inbound => Ok(DialSpec::Inbound),
        SatelliteTarget::Ssh { user, host, port } => Ok(DialSpec::Ssh {
            user: user.clone(),
            host: host.clone(),
            port: *port,
        }),
        SatelliteTarget::Quic { host, port } => {
            let trust = plan_trust(host_is_loopback(host), host, entry)?;
            Ok(DialSpec::Quic {
                host: host.clone(),
                port: *port,
                trust,
                token_file: entry.token_file.clone(),
            })
        }
        SatelliteTarget::Ws { url } => {
            let target = parse_ws(url)?;
            if !target.is_loopback() {
                return Err(LinkRefusal::PlaintextRoutable { url: url.clone() });
            }
            Ok(DialSpec::Ws {
                url: url.clone(),
                // No TLS handshake on ws://; the trust value is inert.
                trust: CertTrust::SkipVerify,
                token_file: entry.token_file.clone(),
            })
        }
        SatelliteTarget::Wss { url } => {
            let target = parse_ws(url)?;
            let trust = plan_trust(target.is_loopback(), url, entry)?;
            Ok(DialSpec::Ws {
                url: url.clone(),
                trust,
                token_file: entry.token_file.clone(),
            })
        }
    }
}

/// Trust for TLS transports: routable requires pin and token; loopback pins
/// when a fingerprint is configured and skips verification otherwise.
pub(super) fn plan_trust(
    loopback: bool,
    endpoint: &str,
    entry: &HubEntry,
) -> Result<CertTrust, LinkRefusal> {
    if loopback {
        return Ok(entry
            .cert_fingerprint
            .clone()
            .map_or(CertTrust::SkipVerify, CertTrust::Pinned));
    }
    let Some(fingerprint) = entry.cert_fingerprint.clone() else {
        return Err(LinkRefusal::MissingFingerprint {
            endpoint: endpoint.to_owned(),
        });
    };
    if entry.token_file.is_none() {
        return Err(LinkRefusal::MissingToken {
            endpoint: endpoint.to_owned(),
        });
    }
    Ok(CertTrust::Pinned(fingerprint))
}

pub(super) fn parse_ws(url: &str) -> Result<WsTarget, LinkRefusal> {
    WsTarget::parse(url).map_err(|err| LinkRefusal::Malformed {
        endpoint: url.to_owned(),
        reason: err.to_string(),
    })
}

/// Whether a host token names loopback (`localhost`, IPv4, or IPv6).
pub(super) fn host_is_loopback(host: &str) -> bool {
    let bare = host.trim_matches(['[', ']']);
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|addr| addr.is_loopback())
}

/// Read and validate the pairing token: first non-empty line, hex. Errors
/// count as a failed attempt, so a fixed file is picked up on redial.
pub(super) fn read_link_token(path: Option<&Path>) -> Result<Option<String>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("read token file {}: {err}", path.display()))?;
    let Some(token) = raw.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return Err(format!("token file {} is empty", path.display()));
    };
    if hex::decode(token).is_err() {
        return Err(format!(
            "token file {} does not hold a valid hex pairing token",
            path.display()
        ));
    }
    Ok(Some(token.to_owned()))
}
