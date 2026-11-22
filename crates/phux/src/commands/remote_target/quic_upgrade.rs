//! Moving a registry entry onto its QUIC route when that route answers.

use std::time::Duration;

use super::super::enroll;
use super::super::remote::{self, Endpoint, RemoteEntry};
use super::{DEFAULT_QUIC_PORT, endpoint_host, endpoint_port};

/// How long an attach waits on the QUIC probe of a `wss://` entry before
/// dialing it as saved: ample for a handshake that will answer at all, short
/// enough that a network filtering UDP costs little.
const WSS_UPGRADE_DEADLINE: Duration = Duration::from_millis(1500);

/// Move an entry onto its QUIC route before the dial when that route answers
/// ([`upgrade_to_quic`]), so this and every later attach go over QUIC.
/// Anything short of an answer leaves the entry as it was, and so does a
/// route that went unanswered lately ([`enroll::udp_unanswered`]): the
/// ssh attach that follows checks UDP again in the background.
pub(super) fn promote_direct_route(entry: RemoteEntry) -> RemoteEntry {
    if quic_candidate(&entry).is_some_and(|route| enroll::udp_unanswered(&route)) {
        return entry;
    }
    // A `wss://` entry works as saved, so its probe is opportunistic and
    // short; skipping ssh is worth the full wait.
    let deadline = if entry.endpoint.starts_with("ssh://") {
        enroll::probe_deadline()
    } else {
        enroll::probe_deadline().min(WSS_UPGRADE_DEADLINE)
    };
    match upgrade_to_quic(&entry, deadline) {
        QuicUpgrade::Upgraded { from, to } => {
            ok!(
                "{}: QUIC answers; upgraded the registry entry from {from} to {to}",
                entry.name
            );
            RemoteEntry {
                endpoint: to,
                direct: None,
                ..entry
            }
        }
        QuicUpgrade::NotSaved { error, .. } => {
            warn!(
                "{}: QUIC answers but the entry could not be rewritten ({error}); \
                 attaching over {}",
                entry.name, entry.endpoint
            );
            entry
        }
        QuicUpgrade::AlreadyQuic | QuicUpgrade::NoRoute | QuicUpgrade::Unreachable { .. } => entry,
    }
}

/// What [`upgrade_to_quic`] did with an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuicUpgrade {
    /// The entry already dials `quic://`.
    AlreadyQuic,
    /// No QUIC route to try: a plaintext `ws://`, loopback, or relay-routed
    /// entry, an `ssh://` entry with no paired QUIC route, or no saved token.
    NoRoute,
    /// The candidate route did not answer; the entry is unchanged.
    Unreachable { candidate: String, error: String },
    /// The candidate answered but the registry could not be rewritten.
    NotSaved { candidate: String, error: String },
    /// The entry now dials `to` instead of `from`.
    Upgraded { from: String, to: String },
}

/// The QUIC route an entry could move to: a kept `direct` QUIC route, else,
/// for a `wss://` entry, the same host on the default QUIC port. A server
/// binds QUIC beside WSS with the same certificate and token store
/// (ADR-0081), so the entry's credentials carry over unchanged.
pub(crate) fn quic_candidate(entry: &RemoteEntry) -> Option<String> {
    let kept = entry
        .direct
        .as_deref()
        .filter(|direct| matches!(Endpoint::parse(direct), Ok(Endpoint::Quic(_))));
    match Endpoint::parse(&entry.endpoint).ok()? {
        Endpoint::Quic(_) => None,
        Endpoint::Ssh(_) => kept.map(str::to_owned),
        // A relay route has no WebSocket leg to upgrade from, and plaintext
        // `ws://` is a loopback development listener.
        Endpoint::Ws(url) if url.starts_with("ws://") || entry.tls_server_name.is_some() => None,
        // The QUIC port is only guessed beside the default WebSocket port: a
        // listener moved off 8787 says nothing about where QUIC went. And a
        // loopback `wss://` is a local test listener: guessing its QUIC port
        // would dial whatever else on this machine owns 8788.
        Endpoint::Ws(url) => kept.map(str::to_owned).or_else(|| {
            if endpoint_port(&url) != Some(phux_server::runtime::DEFAULT_WS_PORT) {
                return None;
            }
            let host = endpoint_host(&url).filter(|host| !is_loopback(host))?;
            let host = if host.contains(':') {
                format!("[{host}]")
            } else {
                host
            };
            Some(format!("quic://{host}:{DEFAULT_QUIC_PORT}"))
        }),
    }
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Probe the entry's [`quic_candidate`] with its saved credentials and, when
/// it answers, rewrite the entry to dial it. The rewrite keeps the token,
/// pin, session, ssh destination, and enrolled client identity.
pub(crate) fn upgrade_to_quic(entry: &RemoteEntry, deadline: Duration) -> QuicUpgrade {
    if matches!(Endpoint::parse(&entry.endpoint), Ok(Endpoint::Quic(_))) {
        return QuicUpgrade::AlreadyQuic;
    }
    let Some(candidate) = quic_candidate(entry) else {
        return QuicUpgrade::NoRoute;
    };
    let (Some(target), Ok(Some(token))) =
        (candidate.strip_prefix("quic://"), remote::read_token(entry))
    else {
        return QuicUpgrade::NoRoute;
    };
    let identity = match entry.client_identity() {
        Ok(identity) => identity,
        Err(error) => return QuicUpgrade::Unreachable { candidate, error },
    };
    if let Err(error) = enroll::probe_within(
        target,
        &token,
        entry.cert_fingerprint.as_deref(),
        &entry.authority_pin(),
        identity,
        deadline,
    ) {
        return QuicUpgrade::Unreachable { candidate, error };
    }
    // Rebuild from the entry as it is now, not as it was before the probe:
    // a `host renew` meanwhile must keep its new certificate.
    let upgraded = current_entry(entry).and_then(|current| {
        remote::NewRemote::new(
            &current.name,
            &candidate,
            current.token_file.as_deref(),
            current.cert_fingerprint.as_deref(),
            current.session.as_deref(),
        )
        .map(|new| {
            new.with_ssh(current.ssh.as_deref())
                .with_remote_phux(current.remote_phux.as_deref())
        })
        .and_then(|new| new.with_client_identity(current.client_identity_paths()))
        .and_then(|new| remote::add_or_update(&new).map(|()| new.endpoint))
    });
    match upgraded {
        Ok(to) => QuicUpgrade::Upgraded {
            from: entry.endpoint.clone(),
            to,
        },
        Err(error) => QuicUpgrade::NotSaved { candidate, error },
    }
}

/// The registered entry named like `probed`, re-read; refused when it no
/// longer dials what was probed or pins what the probe trusted.
fn current_entry(probed: &RemoteEntry) -> Result<RemoteEntry, String> {
    let current = remote::load_registry()?
        .into_iter()
        .find(|entry| entry.name == probed.name)
        .ok_or_else(|| "the entry was removed while QUIC was probed".to_owned())?;
    if current.endpoint != probed.endpoint || current.cert_fingerprint != probed.cert_fingerprint {
        return Err("the entry changed while QUIC was probed; run it again".to_owned());
    }
    Ok(current)
}
