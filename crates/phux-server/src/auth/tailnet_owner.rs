//! `[policy] tailnet-owner` (`docs/config.md#tailnet-owner-devices`): under
//! `paired`, a QUIC listener bound to this machine's tailnet address admits
//! the owner's own devices with no token or certificate, on the local
//! Tailscale client's word ([`phux_config::tailnet_owner`]).
//!
//! The credential minted here never leaves the server: only the QUIC
//! admission builds it, after the check, and the `paired` engine turns it
//! into a paired person's device grant only when the mode is on.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private auth module; the QUIC admission and tests use these through the auth re-export"
)]

use std::net::IpAddr;

use super::AuthenticatedCredential;

/// The principal an owner device's connection is attributed to.
pub const TAILNET_OWNER_PRINCIPAL: &str = "tailnet-owner";

/// Prefix of an owner device's credential id; the owner's login follows.
const ID_PREFIX: &str = "tailnet:";

/// The owner device credential for `peer`, which reached this machine's
/// tailnet address `local` with no client certificate, or `None` when it is
/// not one of the owner's own untagged devices or anything fails.
pub(crate) async fn admit(local: IpAddr, peer: IpAddr) -> Option<AuthenticatedCredential> {
    let decided = tokio::task::spawn_blocking(move || {
        phux_config::tailnet_owner::owner_device(local, peer, phux_config::tailnet_owner::whois)
    })
    .await;
    match decided {
        Ok(Ok(login)) => Some(credential(&login)),
        Ok(Err(reason)) => {
            tracing::debug!(%peer, %reason, "tailnet owner check refused");
            None
        }
        Err(_) => None,
    }
}

fn credential(login: &str) -> AuthenticatedCredential {
    AuthenticatedCredential {
        id: format!("{ID_PREFIX}{login}"),
        principal: TAILNET_OWNER_PRINCIPAL.to_owned(),
        scopes: Vec::new(),
        issued_at: jiff::Timestamp::now(),
        expires_at: None,
        generation: 0,
        registry_instance: None,
        subject: Some(login.to_owned()),
        managed: None,
    }
}

/// Whether `credential` is one the tailnet owner admission minted.
#[must_use]
pub fn is_tailnet_owner(credential: &AuthenticatedCredential) -> bool {
    credential.principal == TAILNET_OWNER_PRINCIPAL && credential.id.starts_with(ID_PREFIX)
}

#[cfg(test)]
pub(crate) fn test_credential(login: &str) -> AuthenticatedCredential {
    credential(login)
}
