//! Explicit native composition used only by host unit fixtures.
#![allow(
    clippy::redundant_pub_crate,
    reason = "test-only re-export for sibling host fixtures, not a public server API"
)]
pub(super) use phux_terminal::downsample;

/// A peer on `transport` running as the serving user.
pub(crate) fn peer(
    transport: phux_protocol::policy::TransportType,
) -> phux_protocol::policy::PeerIdentity {
    phux_protocol::policy::PeerIdentity {
        uid: nix::unistd::geteuid().as_raw(),
        pid: None,
        exe_path: None,
        mcp_host_key: None,
        transport,
        source_addr: None,
    }
}

/// A generation-1 credential `id`, its own principal, holding `scopes`.
pub(crate) fn credential(id: &str, scopes: &[&str]) -> crate::auth::AuthenticatedCredential {
    crate::auth::AuthenticatedCredential {
        id: id.to_owned(),
        principal: id.to_owned(),
        scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        issued_at: jiff::Timestamp::now(),
        expires_at: None,
        generation: 1,
        registry_instance: None,
        subject: None,
        managed: None,
    }
}
