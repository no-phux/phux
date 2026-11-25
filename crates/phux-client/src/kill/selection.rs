use phux_protocol::ids::{IdempotencyKey, SessionKey};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::org::OrgError;
use crate::selector::{self, Selector};
use crate::state::Degradation;

use super::session::{kill_each_terminal, kill_keyed, kill_session, kill_whole_session};

/// A successful [`selected`] kill.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selected {
    /// Interleaved notices from the kill command(s) themselves (a hub's
    /// per-satellite unreachability on the kill round trip).
    pub interleaved: Vec<String>,
    /// How many members a `SESSION_KILL` ended, when the server said (a
    /// replayed kill does not).
    pub killed: Option<u32>,
    /// Original operation evidence when the accepted close has an identity.
    pub recovery: Option<crate::recovery::Recovery>,
}

/// The request identity actually observed, not an eagerly formatted recovery document.
/// Certainty belongs to the outcome variant; the scope and safe next action are fixed.
#[derive(Debug, Default)]
pub struct KillIdentity {
    /// Original operation bytes, absent for an unkeyed close.
    pub operation: Option<IdempotencyKey>,
    /// Actual organization authority identity, never inferred from a handle or path.
    pub authority: Option<Box<str>>,
}

impl KillIdentity {
    pub(super) const fn new(operation: Option<IdempotencyKey>) -> Self {
        Self { operation, authority: None }
    }

    pub(super) fn recovery(&self, certainty: crate::recovery::EffectCertainty) -> crate::recovery::Recovery {
        use crate::recovery::{NextAction, Recovery, ScopeKind};
        Recovery::new(
            ScopeKind::OrganizationAuthority,
            self.operation.map(IdempotencyKey::to_hex),
            certainty,
            NextAction::Reconcile,
        ).with_scope_id(self.authority.as_deref())
    }
}

/// Why [`selected`] did not kill the target.
#[derive(Debug, thiserror::Error)]
pub enum KillError {
    /// The authority proved this close was not sent.
    #[error("kill not sent for {target}: {message}")]
    NotSent {
        /// Original selector.
        target: String,
        /// Original identity; non-application is established by this variant.
        identity: KillIdentity,
        /// Authority's reason.
        message: String,
        /// Notices from the request.
        interleaved: Vec<String>,
    },
    /// A possibly sent close whose end was not observed. Never retry with a fresh identity.
    #[error("kill outcome unknown for {target}: {message}")]
    OutcomeUnknown {
        /// Original target, not a replacement operation.
        target: String,
        /// Original identity; uncertainty is established by this variant.
        identity: KillIdentity,
        /// Transport or protocol evidence.
        message: String,
        /// Cold aggregate evidence is owned once, without copying or serializing the JSON.
        results: Option<Box<serde_json::Value>>,
        /// Notices received before the failure.
        interleaved: Vec<String>,
    },
    /// A federated close returned per-target results; some targets did not close.
    #[error("kill returned partial results for {target}: {results}")]
    Partial {
        /// Original selector.
        target: String,
        /// Cold per-host evidence is owned once, without copying or serializing the JSON.
        results: Box<serde_json::Value>,
        /// Original identity, independent of the mixed per-host outcomes.
        identity: KillIdentity,
        /// Notices from the request.
        interleaved: Vec<String>,
    },
    /// The selector matched nothing against a complete view, or a named
    /// session is absent (hub-local, so a partial fleet cannot hide it).
    #[error("no such target: {target}")]
    NoSuchTarget {
        /// The selector as the caller typed it.
        target: String,
    },
    /// The selector matched nothing against a partial fleet view: a miss
    /// here does not mean the target is gone.
    #[error(
        "could not resolve '{target}': this server's view of the fleet is incomplete, so a \
         miss here does not mean the target is gone"
    )]
    Unresolved {
        /// The selector as the caller typed it.
        target: String,
        /// What the snapshot could not see.
        degradation: Degradation,
    },
    /// The server refused a whole-target kill.
    #[error("kill refused for {label}: {message}")]
    Refused {
        /// The target label used in the refusal (`session "work"`, `"@1"`).
        label: String,
        /// The server's message.
        message: String,
        /// Interleaved notices from that round trip.
        interleaved: Vec<String>,
        /// Original identity; non-application is established by this variant.
        identity: KillIdentity,
    },
    /// The server answered a whole-target kill with an unexpected shape.
    #[error("{label}: {message}")]
    Unexpected {
        /// The target label used in the sentence.
        label: String,
        /// The unexpected-reply sentence from [`crate::explain`].
        message: String,
        /// Interleaved notices from that round trip.
        interleaved: Vec<String>,
    },
    /// The session has no members and the server predates `SESSION_KILL`,
    /// which is what deletes an empty session.
    #[error(
        "session {session:?} is empty and this server predates SESSION_KILL, so it cannot \
         delete it"
    )]
    EmptySessionUnsupported {
        /// The session.
        session: String,
    },
    /// An organization request failed outright.
    #[error(transparent)]
    Org(OrgError),
    /// `%name` did not resolve to one agent (ADR-0075 point 3); nothing
    /// was killed.
    #[error(transparent)]
    Agent(#[from] crate::selector::AgentResolveError),
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
}

impl KillError {
    /// Lossless error projection shared by CLI and MCP.
    #[must_use]
    pub fn outcome_document(&self) -> Option<serde_json::Value> {
        use crate::recovery::EffectCertainty;
        let (code, target, recovery, results) = match self {
            Self::NotSent { target, identity, .. } => (
                "not_sent", Some(target.as_str()),
                Some(identity.recovery(EffectCertainty::NotApplied)), None,
            ),
            Self::OutcomeUnknown { target, identity, results, .. } => (
                "outcome_unknown", Some(target.as_str()),
                Some(identity.recovery(EffectCertainty::Unknown)), results.as_deref(),
            ),
            Self::Partial { target, results, identity, .. } => (
                "partial_results", Some(target.as_str()),
                Some(identity.recovery(EffectCertainty::Unknown)), Some(results.as_ref()),
            ),
            Self::Refused { label, identity, .. } => (
                "refused", Some(label.as_str()),
                Some(identity.recovery(EffectCertainty::NotApplied)), None,
            ),
            Self::Org(error) => (error.code(), None, Some(error.recovery()), None),
            _ => return None,
        };
        Some(serde_json::json!({
            "schema_version": 1,
            "error": {
                "code": code,
                "interleaved": self.interleaved(),
                "message": self.to_string(),
                "target": target,
                "recovery": recovery,
                "results": results,
                "remedy": "reconcile authoritative state before retrying",
            },
            "exit_code": 2,
        }))
    }

    /// Interleaved notices from a kill command that still produced an error.
    #[must_use]
    pub fn interleaved(&self) -> &[String] {
        match self {
            Self::Refused { interleaved, .. }
            | Self::NotSent { interleaved, .. }
            | Self::Unexpected { interleaved, .. }
            | Self::OutcomeUnknown { interleaved, .. }
            | Self::Partial { interleaved, .. } => interleaved,
            _ => &[],
        }
    }

    /// Move notices to an aggregate outcome instead of copying their strings.
    pub(super) fn take_interleaved(&mut self) -> Vec<String> {
        match self {
            Self::Refused { interleaved, .. }
            | Self::NotSent { interleaved, .. }
            | Self::Unexpected { interleaved, .. }
            | Self::OutcomeUnknown { interleaved, .. }
            | Self::Partial { interleaved, .. } => std::mem::take(interleaved),
            _ => Vec::new(),
        }
    }
}

/// Resolve `selector` against a fresh snapshot on `conn` and kill what it names.
///
/// A whole-session target runs [`kill_session`] (an empty session is simply
/// deleted), or one atomic `KILL_RESOURCES` when the snapshot carries no
/// session key; a group, pane, `@id`, or
/// `#tag` target kills each resolved Terminal with `KILL_RESOURCE`. A
/// disconnect cannot establish whether a close was applied.
///
/// With `key`, the kill is one keyed command (`KILL_RESOURCE` for one
/// Terminal, `KILL_RESOURCES` for several). An `@N` / `host/@N` target is
/// sent as written, without a snapshot lookup, so a retry after the first
/// attempt removed the pane still reaches the server.
///
/// Snapshot partial-view notices for a *hit* are appended to `notices`
/// before the kill runs, so a caller can warn even when the kill then
/// fails. A miss does not append: the [`KillError::Unresolved`] /
/// [`KillError::NoSuchTarget`] is the whole answer.
///
/// # Errors
///
/// [`KillError`] — see its variants.
pub async fn selected(
    conn: &mut Connection,
    selector: &Selector,
    target: &str,
    key: Option<IdempotencyKey>,
    notices: &mut Vec<String>,
) -> Result<Selected, KillError> {
    if let (Some(key), Some(id)) = (key, selector.explicit_id()) {
        return kill_keyed(conn, target, vec![id], key).await;
    }
    let (snapshot, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    if let Some(session) = selector::whole_session_name(selector, &snapshot) {
        if let Some(session_key) = session_key_named(&snapshot, &session) {
            notices.extend(degradation.notices().iter().cloned());
            return match key {
                Some(operation) => {
                    super::session::kill_session_keyed(conn, session_key, &session, operation).await
                }
                None => kill_session(conn, session_key, &session).await,
            };
        }
        let ids = selector::resolve(selector, &snapshot);
        if ids.is_empty() && session_is_empty(&snapshot, &session) {
            return Err(KillError::EmptySessionUnsupported { session });
        }
        if ids.is_empty() {
            return Err(KillError::NoSuchTarget {
                target: target.to_owned(),
            });
        }
        notices.extend(degradation.notices().iter().cloned());
        return kill_whole_session(conn, &session, ids, key).await;
    }
    let terminals = crate::state::resolve_targets_on(conn, selector, &snapshot).await?;
    if terminals.is_empty() {
        return Err(target_miss(target, degradation));
    }
    notices.extend(degradation.notices().iter().cloned());
    match key {
        Some(key) => kill_keyed(conn, target, terminals, key).await,
        None => kill_each_terminal(conn, terminals, target).await,
    }
}

/// A miss, told apart so a partial fleet view never claims the target is
/// gone: `#tag` and `@id` search the pane list a hub aggregates.
fn target_miss(target: &str, degradation: Degradation) -> KillError {
    if degradation.is_complete() {
        KillError::NoSuchTarget {
            target: target.to_owned(),
        }
    } else {
        KillError::Unresolved {
            target: target.to_owned(),
            degradation,
        }
    }
}

/// Whether the session named `name` holds no Terminal (ADR-0105): read from
/// the session facet on a server that reports it (ADR-0161 §7), else from
/// the legacy window count.
pub(super) fn session_is_empty(snapshot: &SessionSnapshot, name: &str) -> bool {
    let by_facet = crate::resource::reports_sessions(snapshot);
    snapshot.sessions.iter().any(|session| {
        session.name == name
            && if by_facet {
                crate::resource::terminals_in(snapshot, session.id)
                    .next()
                    .is_none()
            } else {
                session.is_empty()
            }
    })
}

fn session_key_named(snapshot: &SessionSnapshot, name: &str) -> Option<SessionKey> {
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name)
        .and_then(|session| session.key)
}
