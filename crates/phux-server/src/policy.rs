//! The server's HELLO authorization seam.
//!
//! # Status: deliberately permissive, deliberately kept (ADR-0072)
//!
//! [`PolicyEngine`] has exactly one production implementation,
//! [`PermissivePolicy`], and it allows everything. That is not an oversight
//! and it is not scaffolding to delete — it is the injection point that
//! **phux-pjc5** ("paired workload authentication and scoped Phux policy
//! enforcement", a P0 that must replace the permissive default) will
//! implement. Please do not re-file the "policy engine is permissive-only"
//! audit finding: ADR-0072 recorded the decision to keep this seam after
//! pruning the audit/provenance/per-operation vocabulary that grew around
//! it and that nothing ever called.
//!
//! What is real today, and covered by `tests/policy_deny.rs`:
//!
//! - the seam is called once per connection, at HELLO, with the
//!   [`PeerIdentity`] the accepting transport authenticated;
//! - a [`PolicyError`] from the engine refuses the handshake with
//!   `ERROR { code: PermissionDenied }` and closes the connection;
//! - the default engine, and the only one the shipped binary installs, is
//!   [`PermissivePolicy`].
//!
//! What is deferred to phux-pjc5: minting a real requested-capability set
//! from a paired credential, enforcing the granted set in the authoritative
//! handlers, per-operation authorization, revocation, and audit. None of
//! those had a call site, so none of them are pre-shaped here — pjc5 brings
//! the vocabulary its scope matrix actually needs.

use std::future::Future;
use std::pin::Pin;

use phux_protocol::policy::{Capability, PeerIdentity};
use tracing::trace;

/// Extension point for the server's HELLO authorization decision.
///
/// The server consults this once per connection, after the transport has
/// authenticated the peer and before any other frame is processed. An
/// implementation may refuse the connection outright (return an `Err`) or
/// attenuate the capability set the connection carries.
///
/// `&self` so the implementation can be shared across tasks (it is held as
/// `Arc<dyn PolicyEngine>`), and the method returns a boxed future so the
/// trait stays object-safe.
pub trait PolicyEngine: Send + Sync + std::fmt::Debug {
    /// Authorize a HELLO handshake. Returns the capabilities that should
    /// be granted to this consumer, which the server intersects with what
    /// the consumer requested. An `Err` refuses the handshake.
    fn authorize_hello<'a>(
        &'a self,
        peer_identity: &'a PeerIdentity,
        requested_caps: Vec<Capability>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Capability>, PolicyError>> + Send + 'a>>;
}

/// The policy engine every shipped phux server runs: it grants whatever was
/// requested and refuses nothing.
///
/// This is the honest default for the local trust model phux has today —
/// a UDS whose peer is the same OS user, kernel-enforced
/// (`docs/operations.md`, "Local trust model"). It is *not* an adequate
/// default for the remote/paired deployments ADR-0031 and phux-pjc5
/// describe, which is exactly why the seam above exists.
#[derive(Debug, Clone, Copy)]
pub struct PermissivePolicy;

impl PermissivePolicy {
    /// Shared instance (stateless).
    pub const INSTANCE: Self = Self;
}

impl PolicyEngine for PermissivePolicy {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        requested_caps: Vec<Capability>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Capability>, PolicyError>> + Send + 'a>> {
        Box::pin(async move {
            trace!("PermissivePolicy: authorizing HELLO");
            Ok(requested_caps)
        })
    }
}

/// Errors from the policy seam. An `Err` at HELLO is a refusal.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The consumer is not permitted to connect.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The policy engine itself failed. Fails closed, like a denial: the
    /// server must not admit a peer it could not evaluate.
    #[error("internal: {0}")]
    Internal(String),
}
