use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{IdempotencyKey, SessionKey};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::org::OrgError;
use crate::selector::{self, Selector};
use crate::state::Degradation;

use super::keyed_signal_supported;
use super::session::{
    kill_each_terminal, kill_keyed, kill_session, kill_whole_session, session_kill_supported,
};

/// A successful [`selected`] kill.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selected {
    /// Interleaved notices from the kill command(s) themselves (a hub's
    /// per-satellite unreachability on the kill round trip).
    pub interleaved: Vec<String>,
    /// How many members a `SESSION_KILL` ended, when the server said (a
    /// replayed kill does not).
    pub killed: Option<u32>,
}

/// Why [`selected`] did not kill the target.
#[derive(Debug, thiserror::Error)]
pub enum KillError {
    /// The server does not advertise `KEYED_SIGNAL`, so a keyed retry would
    /// run again. Nothing was sent.
    #[error(
        "the server does not advertise KEYED_SIGNAL, so it would run a keyed retry again; \
         nothing was sent"
    )]
    UnsupportedKeyedSignal,
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
    /// One or more per-pane kills were refused; others may have landed.
    #[error("{}", messages.join("\n"))]
    PaneRefusals {
        /// One sentence per refused pane, without a `phux:` prefix.
        messages: Vec<String>,
        /// Interleaved notices collected across the loop.
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
    /// Interleaved notices from a kill command that still produced an error.
    #[must_use]
    pub fn interleaved(&self) -> &[String] {
        match self {
            Self::Refused { interleaved, .. }
            | Self::Unexpected { interleaved, .. }
            | Self::PaneRefusals { interleaved, .. } => interleaved,
            _ => &[],
        }
    }
}

/// Resolve `selector` against a fresh snapshot on `conn` and kill what it names.
///
/// A whole-session target runs [`kill_session`] against a server that
/// advertises `SESSION_KILL` (an empty session is simply deleted), and one
/// atomic `KILL_RESOURCES` against an older one; a group, pane, `@id`, or
/// `#tag` target kills each resolved Terminal with `KILL_RESOURCE`. A clean
/// disconnect after a kill is the server self-exiting once its last session
/// was reaped: success.
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
    if key.is_some() && !keyed_signal_supported(conn) {
        return Err(KillError::UnsupportedKeyedSignal);
    }
    if let (Some(key), Some(id)) = (key, selector.explicit_id()) {
        return kill_keyed(conn, target, vec![id], key).await;
    }
    let (snapshot, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    if let Some(session) = selector::whole_session_name(selector, &snapshot) {
        if session_kill_supported(conn)
            && let Some(key) = session_key_named(&snapshot, &session)
        {
            notices.extend(degradation.notices().iter().cloned());
            return kill_session(conn, key, &session).await;
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
        let killed = kill_whole_session(conn, &session, ids.clone(), key).await?;
        await_reaped(conn, &ids).await;
        return Ok(killed);
    }
    let terminals = crate::state::resolve_targets_on(conn, selector, &snapshot).await?;
    if terminals.is_empty() {
        return Err(target_miss(target, degradation));
    }
    notices.extend(degradation.notices().iter().cloned());
    let killed = match key {
        Some(key) => kill_keyed(conn, target, terminals.clone(), key).await,
        None => kill_each_terminal(conn, terminals.clone()).await,
    }?;
    await_reaped(conn, &terminals).await;
    Ok(killed)
}

/// The longest a successful kill waits, against an older server, for its
/// panes to leave the server's state: past the pane-kill grace (`SIGHUP`,
/// then `SIGKILL` after 500ms) with room for a loaded host.
const REAP_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Poll cadence while [`await_reaped`] waits.
const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Whether the server's kill reply already follows the whole visible
/// teardown (`features_ext.COMMITTED_TEARDOWN`, `docs/spec/L1.md` §5.2), so
/// the next `GET_STATE` omits the killed resources.
fn kill_reply_is_the_teardown(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features_ext
            .contains(ServerFeatureExt::CommittedTeardown)
    })
}

/// Against a server that predates `COMMITTED_TEARDOWN`, wait until none of
/// the local `ids` is still listed by `GET_STATE`; a current server's reply
/// already guarantees it, so this returns at once.
///
/// An older server commits a kill before it replies, but keeps each pane
/// (and so its session) listed until the pane's process is reaped. Without
/// this wait `phux kill work && phux new work` fails on a name that is about
/// to be free, and `phux ls` right after a kill still lists the killed panes
/// as running. Best-effort and bounded: a disconnect is the server exiting
/// after its last session, and a pane that outlives [`REAP_WAIT`] was still
/// killed. Satellite ids are not waited on; their reap is the satellite's
/// business.
async fn await_reaped(conn: &mut Connection, ids: &[ResourceId]) {
    if kill_reply_is_the_teardown(conn) {
        return;
    }
    let local: Vec<&ResourceId> = ids
        .iter()
        .filter(|id| matches!(id, ResourceId::Local { .. }))
        .collect();
    if local.is_empty() {
        return;
    }
    let deadline = tokio::time::Instant::now() + REAP_WAIT;
    loop {
        let Ok(view) = crate::state::get_state_on(conn).await else {
            return;
        };
        let (snapshot, _) = view.into_parts();
        let listed = snapshot
            .resources
            .iter()
            .any(|resource| local.contains(&&resource.id));
        if !listed || tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(REAP_POLL).await;
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
