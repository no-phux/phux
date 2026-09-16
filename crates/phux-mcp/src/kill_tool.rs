//! `phux_kill` in-process: the same resolution and teardown `phux kill`
//! performs, composed from the `phux_client::kill` wire homes.
//!
//! A whole-session target (`.` or a bare name) resolves to its full
//! Terminal-id list and rides one atomic `KILL_RESOURCES`; an empty session
//! (ADR-0105) clears its keep-empty mark instead and a `GET_STATE` confirms
//! it went; a window, pane, `@id`, or `#tag` target kills each resolved
//! Terminal with `KILL_RESOURCE`. A clean disconnect after a kill is the
//! server self-exiting once its last session was reaped: success.
//!
//! This is a mirror of `crates/phux/src/commands/kill.rs::kill_selected`,
//! sharing only the `phux_client::kill` wire helpers; unifying the two is
//! tracked separately. One CLI behaviour is not mirrored: the partial-fleet
//! warning (`partial::warn_partial_view`) the CLI prints when a hit comes
//! from an incomplete view. A tool result has no warning channel, so a
//! kill that lands under degradation reports plain success.
//!
//! With an `idempotency_key` the kill is one keyed command, as `phux kill
//! --idempotency-key` sends it (L1 §5.1.1): `KILL_RESOURCE` for one
//! Terminal, `KILL_RESOURCES` for several, and an `@N` / `host/@N` target is
//! sent as written, without a snapshot lookup, so a retry after the first
//! attempt removed the pane still reaches the server. A server without
//! `KEYED_SIGNAL` is refused before anything is sent.

use std::path::Path;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_client::kill::{KeyedError, KillOutcome};
use phux_client::selector::{self, Selector};
use phux_client::state::{self, Degradation};
use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::info::SessionSnapshot;

use crate::tools::ToolError;

/// Resolve `selector` against a fresh snapshot of the server at `socket` and
/// kill what it names. `target` is the selector as the caller typed it, for
/// the miss message.
///
/// # Errors
///
/// A transport failure, a selector miss (worded as unresolvable when the
/// fleet view was partial), or any kill the server refused.
pub(crate) async fn kill_selected(
    socket: &Path,
    selector: &Selector,
    target: &str,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let mut conn = Connection::connect(socket).await?;
    let result = kill_on(&mut conn, selector, target, key).await;
    drop(conn);
    result
}

/// [`kill_selected`] on an open connection: resolve against its snapshot,
/// then tear down.
async fn kill_on(
    conn: &mut Connection,
    selector: &Selector,
    target: &str,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    if key.is_some() && !phux_client::kill::keyed_signal_supported(conn) {
        return Err(crate::pane_tools::unsupported_keyed_signal());
    }
    if let (Some(key), Some(id)) = (key, explicit_id(selector)) {
        return kill_keyed(conn, target, vec![id], key).await;
    }
    let (snapshot, degradation) = state::get_state_on(conn).await?.into_parts();
    if let Some(session) = selector::whole_session_name(selector, &snapshot) {
        return kill_session(conn, selector, &snapshot, &session, target, key).await;
    }
    let terminals = resolve_terminals(conn, selector, &snapshot).await;
    if terminals.is_empty() {
        return Err(target_miss(target, &degradation));
    }
    match key {
        Some(key) => kill_keyed(conn, target, terminals, key).await,
        None => kill_each_terminal(conn, terminals).await,
    }
}

/// The one id an explicit `@N` / `host/@N` target names.
fn explicit_id(selector: &Selector) -> Option<ResourceId> {
    match selector {
        Selector::ResourceId(id) => Some(ResourceId::local(*id)),
        Selector::SatelliteResourceId { host, id } => {
            Some(ResourceId::satellite(host.as_str(), *id))
        }
        _ => None,
    }
}

/// Kill `terminals` as one keyed command: `KILL_RESOURCE` for one,
/// `KILL_RESOURCES` for several, so the key names the whole operation.
async fn kill_keyed(
    conn: &mut Connection,
    target: &str,
    mut terminals: Vec<ResourceId>,
    key: IdempotencyKey,
) -> Result<(), ToolError> {
    let reply = if terminals.len() == 1 {
        let terminal = terminals.remove(0);
        phux_client::kill::kill_resource_keyed(conn, 1, terminal, key).await
    } else {
        phux_client::kill::kill_resources_keyed(conn, 1, terminals, key).await
    };
    batch_outcome(reply, &format!("{target:?}"))
}

/// A whole-session target: one atomic `KILL_RESOURCES`, or the keep-empty
/// clear for a session with no panes.
///
/// A named session is hub-local by construction (a hub discards its
/// satellites' session lists), so a session that resolved to a name and
/// then to no panes is genuinely empty, degraded fleet or not.
async fn kill_session(
    conn: &mut Connection,
    selector: &Selector,
    snapshot: &SessionSnapshot,
    session: &str,
    target: &str,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let ids = selector::resolve(selector, snapshot);
    if !ids.is_empty() {
        return kill_whole_session(conn, session, ids, key).await;
    }
    if session_is_empty(snapshot, session) {
        return kill_empty_session(conn, session).await;
    }
    Err(ToolError::new(format!("no such target: {target}")))
}

/// The Terminals a non-session selector names. A `#tag` selector resolves
/// against L3 tag metadata fetched on this same connection; every other form
/// is pure snapshot resolution.
async fn resolve_terminals(
    conn: &mut Connection,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Vec<ResourceId> {
    if matches!(selector, Selector::Tag(_)) {
        let index = state::fetch_tag_index(conn, snapshot).await;
        return selector::resolve_with_tags(selector, snapshot, &index);
    }
    selector::resolve(selector, snapshot)
}

/// A miss, worded so a partial fleet view never claims the target is gone:
/// `#tag` and `@id` search the pane list a hub aggregates, and an
/// unreachable satellite may hold the match. Shared with `phux_tag`.
pub(crate) fn target_miss(target: &str, degradation: &Degradation) -> ToolError {
    if degradation.notices().is_empty() {
        return ToolError::new(format!("no such target: {target}"));
    }
    ToolError::new(format!(
        "could not resolve {target}: this server's view of the fleet is incomplete ({}), so a \
         miss here does not mean the target is gone",
        degradation.notices().join("; ")
    ))
}

/// Whether the session named `name` holds no windows (ADR-0105).
fn session_is_empty(snapshot: &SessionSnapshot, name: &str) -> bool {
    snapshot
        .sessions
        .iter()
        .any(|session| session.name == name && session.is_empty())
}

/// Kill an empty session (ADR-0105): clear its keep-empty mark, then confirm
/// with a `GET_STATE` on the same ordered connection that it is gone. A
/// disconnect in its place is the server self-exiting after its last
/// session went, which is success.
async fn kill_empty_session(conn: &mut Connection, session: &str) -> Result<(), ToolError> {
    phux_client::kill::clear_session_keep_empty(conn, 1, session).await?;
    match state::get_state_on(conn).await {
        Ok(view) if view.snapshot().sessions.iter().any(|s| s.name == session) => {
            Err(ToolError::new(format!(
                "kill refused for session {session:?}: the server kept it"
            )))
        }
        Ok(_) | Err(AttachError::Disconnected) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// One `KILL_RESOURCES` for a whole session, keyed when `key` is set.
async fn kill_whole_session(
    conn: &mut Connection,
    session: &str,
    ids: Vec<ResourceId>,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let reply = match key {
        Some(key) => phux_client::kill::kill_resources_keyed(conn, 1, ids, key).await,
        None => phux_client::kill::kill_resources(conn, 1, ids)
            .await
            .map_err(KeyedError::from),
    };
    batch_outcome(reply, &format!("session {session:?}"))
}

/// The tool result of one kill round trip that covered a whole target. A
/// disconnect is the server self-exiting after its last session was reaped,
/// which is success.
fn batch_outcome(
    reply: Result<(KillOutcome, Degradation), KeyedError>,
    label: &str,
) -> Result<(), ToolError> {
    match reply {
        Ok((KillOutcome::Killed, _)) | Err(KeyedError::Attach(AttachError::Disconnected)) => Ok(()),
        Err(KeyedError::Unsupported) => Err(crate::pane_tools::unsupported_keyed_signal()),
        Ok((KillOutcome::Refused(message), _)) => Err(ToolError::new(format!(
            "kill refused for {label}: {message}"
        ))),
        Ok((KillOutcome::Unexpected(other), _)) => Err(ToolError::new(format!(
            "{label}: {}",
            phux_client::explain::explain_unexpected("kill", &other)
        ))),
        Err(KeyedError::Attach(err)) => Err(err.into()),
    }
}

/// Kill each Terminal in turn, the way `phux kill` does: every refusal is
/// collected rather than stopping the loop, and a disconnect means the
/// remaining targets are already gone.
async fn kill_each_terminal(
    conn: &mut Connection,
    terminals: Vec<ResourceId>,
) -> Result<(), ToolError> {
    let mut refusals = Vec::new();
    for (i, terminal) in terminals.into_iter().enumerate() {
        let request_id = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        match kill_one(conn, request_id, terminal).await {
            KillStep::Killed => {}
            KillStep::Refused(message) => refusals.push(message),
            KillStep::ServerGone => break,
        }
    }
    if refusals.is_empty() {
        Ok(())
    } else {
        Err(ToolError::new(refusals.join("\n")))
    }
}

/// How one `KILL_RESOURCE` ended.
enum KillStep {
    Killed,
    Refused(String),
    ServerGone,
}

async fn kill_one(conn: &mut Connection, request_id: u32, terminal: ResourceId) -> KillStep {
    let label = selector::format_terminal_id(&terminal);
    match phux_client::kill::kill_resource(conn, request_id, terminal).await {
        Ok((KillOutcome::Killed, _)) => KillStep::Killed,
        Ok((KillOutcome::Refused(message), _)) => {
            KillStep::Refused(format!("kill refused for {label}: {message}"))
        }
        Ok((KillOutcome::Unexpected(other), _)) => KillStep::Refused(format!(
            "{label}: {}",
            phux_client::explain::explain_unexpected("kill", &other)
        )),
        Err(AttachError::Disconnected) => KillStep::ServerGone,
        Err(err) => KillStep::Refused(format!("kill failed for {label}: {err}")),
    }
}
