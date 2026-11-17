//! A scripted [`PolicyEngine`] for tests: one closure decides every HELLO.
//!
//! Compiled for this crate's own tests and, behind the `testing` feature, for
//! `phux-server-testkit` and the live-server suites of other crates. No
//! configuration of `phux server` selects it.

use std::fmt;
use std::sync::Arc;

use phux_protocol::policy::PeerIdentity;
use phux_protocol::scope::TerminalScopeSet;

use super::{ConnectionGrant, GrantFuture, PolicyEngine, PolicyError};
use crate::auth::AuthenticatedCredential;

/// The decision a [`FnPolicy`] runs at each HELLO.
pub type GrantFn = dyn Fn(&PeerIdentity, Option<&AuthenticatedCredential>) -> Result<ConnectionGrant, PolicyError>
    + Send
    + Sync;

/// A [`PolicyEngine`] whose every HELLO is decided by one closure. A test
/// that changes its grants mid-run captures shared state (an atomic, a
/// mutex) in the closure and flips it. Exposes no workload registry.
#[derive(Clone)]
pub struct FnPolicy(Arc<GrantFn>);

impl fmt::Debug for FnPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FnPolicy")
    }
}

impl FnPolicy {
    /// Decide every HELLO with `decide`.
    pub fn new(
        decide: impl Fn(
            &PeerIdentity,
            Option<&AuthenticatedCredential>,
        ) -> Result<ConnectionGrant, PolicyError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self(Arc::new(decide))
    }

    /// Every connection holds the owner's grant: for tests of transport and
    /// routing mechanics, not of who may act (ADR-0179).
    #[must_use]
    pub fn owner() -> Self {
        Self::new(|_, _| Ok(ConnectionGrant::owner()))
    }

    /// Every connection holds `scopes`, as its credential's principal when it
    /// presents one.
    ///
    /// # Panics
    ///
    /// When a scope does not parse; the scopes are a test's literal.
    #[must_use]
    pub fn scoped(scopes: &[&str]) -> Self {
        Self::scoped_set(parse_scopes(scopes))
    }

    /// [`Self::scoped`] over an already-parsed set.
    #[must_use]
    pub fn scoped_set(set: TerminalScopeSet) -> Self {
        Self::new(move |_, credential| scoped_grant(&set, credential))
    }

    /// Refuse every HELLO as unauthorized, with `message`.
    #[must_use]
    pub fn deny(message: &str) -> Self {
        let message = message.to_owned();
        Self::new(move |_, _| Err(PolicyError::Unauthorized(message.clone())))
    }
}

impl PolicyEngine for FnPolicy {
    fn authorize_hello<'a>(
        &'a self,
        peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let grant = (self.0)(peer_identity, credential);
        Box::pin(async move { grant })
    }
}

/// Parse a test's literal scopes.
///
/// # Panics
///
/// When a scope does not parse.
#[must_use]
#[allow(clippy::expect_used, reason = "a test's literal scopes must parse")]
pub fn parse_scopes(scopes: &[&str]) -> TerminalScopeSet {
    TerminalScopeSet::parse_all(scopes).expect("test scopes parse")
}

/// A scoped grant over `set`, as `credential`'s principal when there is one.
///
/// # Errors
///
/// [`PolicyError::Internal`] when `set` cannot back a grant.
pub fn scoped_grant(
    set: &TerminalScopeSet,
    credential: Option<&AuthenticatedCredential>,
) -> Result<ConnectionGrant, PolicyError> {
    ConnectionGrant::scoped(set.clone(), credential.map(|c| c.id.clone()))
        .map_err(|err| PolicyError::Internal(err.to_string()))
}
