//! An embedder's own HELLO engine (ADR-0072), for tests of transport and
//! routing mechanics rather than of authorization.
//!
//! `phux server` has no mode in which a remote connection holds the owner's
//! grant (ADR-0179): with no `[policy] mode`, a remote listener or relay
//! connector refuses to start. A test that exercises a QUIC dial, a relay
//! route, or a WebSocket upgrade, and is not about who may act, injects
//! [`OwnerEngine`] through `ServerConfig::policy_engine` instead. The server
//! then starts in the embedder posture: the engine decides every HELLO and
//! the listeners require no workload certificate. Tests of authorization use
//! `paired` with an enrolled certificate.

use std::sync::Arc;

use phux_protocol::policy::PeerIdentity;
use phux_server::ServerConfig;
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{ConnectionGrant, GrantFuture, PolicyEngine};

/// Grants every admitted connection the owner's authority. Test-only: no
/// configuration of `phux server` selects it.
#[derive(Debug, Clone, Copy)]
pub struct OwnerEngine;

impl PolicyEngine for OwnerEngine {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        Box::pin(async { Ok(ConnectionGrant::owner()) })
    }
}

/// Install [`OwnerEngine`] as `cfg`'s embedder engine.
pub fn embed_owner_engine(cfg: &mut ServerConfig) {
    cfg.policy_engine = Some(Arc::new(OwnerEngine));
}

/// Write a pairing-token store at `path` holding one fresh credential, and
/// return its bearer secret. The store is the versioned one `phux pair`
/// writes; the secret is shown only here.
pub fn mint_bearer(path: &std::path::Path) -> Vec<u8> {
    let minted = phux_server::auth::mint_token(path).expect("mint a pairing credential");
    hex_decode(minted.secret())
}

fn hex_decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex secret"))
        .collect()
}
