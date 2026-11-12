//! Classify a remote endpoint string before the runtime owns the session.

use super::*;

/// Classify the stable string constructor's endpoint before the runtime owns
/// the session. QUIC drops its URI scheme because [`Transport::Quic`] carries
/// only the authority; WebSocket keeps the complete URL.
pub(super) fn classify_remote_endpoint(endpoint: &str) -> Result<Transport, String> {
    let endpoint = endpoint.trim();
    if let Some(authority) = endpoint.strip_prefix("quic://") {
        validate_quic_authority(authority)?;
        return Ok(Transport::Quic(authority.to_owned()));
    }
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(Transport::Ws(endpoint.to_owned()));
    }
    Err(format!(
        "{endpoint:?} must start with quic://, wss://, or ws://"
    ))
}

/// Reject malformed QUIC targets without starting the runtime's reconnect
/// loop. DNS resolution and reachability remain the runtime dialer's job.
fn validate_quic_authority(authority: &str) -> Result<(), String> {
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| format!("quic://{authority} needs HOST:PORT (bracket an IPv6 address)"))?;
    let bracketed = host.starts_with('[') && host.ends_with(']');
    let bare_host = host.trim_matches(['[', ']']);
    let valid_host = !bare_host.is_empty()
        && !authority.contains(['/', '?', '#', '@'])
        && (bracketed || !host.contains(':'));
    let valid_port = port.parse::<u16>().is_ok_and(|port| port != 0);
    if valid_host && valid_port {
        return Ok(());
    }
    Err(format!(
        "quic://{authority} needs HOST:PORT with a non-zero numeric port; bracket an IPv6 address"
    ))
}
