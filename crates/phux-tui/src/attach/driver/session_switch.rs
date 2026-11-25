//! Switching sessions on one connection: detach, drain, attach again.

use phux_protocol::wire::frame::{AttachTarget, FrameKind};

use super::entry::EntryPick;
use super::session_io::{send_attach, wait_for_attached_serving};
use crate::attach::agent_forward::AgentLink;
use crate::attach::connection::Connection;
use crate::attach::input_dispatch::ReattachTarget;
use crate::attach::outcome::AttachError;

/// Re-handshake against `target` on the same connection and return the new
/// `ATTACHED`. The glass is not cleared: the next entry's held first paint
/// replaces the old frame in one synchronized frame (`handover`).
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(super) async fn switch_session(
    conn: &mut Connection,
    target: ReattachTarget,
    pick: &mut EntryPick,
    orphan_kills: &mut super::orphans::OrphanKills,
    review: &mut crate::attach::review::ReviewIndex,
    agent: Option<&AgentLink>,
) -> Result<FrameKind, AttachError> {
    // Lifecycle transition (info): switching sessions on the same
    // connection. `?target` names the destination.
    tracing::info!(?target, "attach loop: SWITCH_TO; re-attaching");
    reattach_on_same_connection(conn, target, pick, orphan_kills, review, agent).await
}

/// Detach and re-handshake against `target` on the same connection, returning
/// the new `ATTACHED`; a one-step pick is stashed for the next entry.
async fn reattach_on_same_connection(
    conn: &mut Connection,
    target: ReattachTarget,
    pick: &mut EntryPick,
    orphan_kills: &mut super::orphans::OrphanKills,
    review: &mut crate::attach::review::ReviewIndex,
    agent: Option<&AgentLink>,
) -> Result<phux_protocol::wire::frame::FrameKind, AttachError> {
    detach_and_drain(conn, orphan_kills, review, agent).await?;
    let attach_target = match target {
        ReattachTarget::Existing {
            name,
            id,
            window,
            pane,
            resource,
        } => {
            *pick = EntryPick {
                window,
                pane,
                resource,
            };
            id.map_or(AttachTarget::ByName(name), AttachTarget::ById)
        }
        ReattachTarget::Create {
            name,
            directory,
            host: _,
        } => create_session_target(name, directory),
    };
    super::session_io::create_persistent_session(conn, &attach_target).await;
    let attach_id = send_attach(conn, attach_target).await?;
    let attached = wait_for_attached_serving(conn, attach_id, agent).await?;
    tracing::info!("attach loop: re-attach handshake complete");
    Ok(attached)
}

/// The `CreateIfMissing` target for an in-TUI session create, in
/// `directory` (never the client process's cwd).
fn create_session_target(name: String, directory: Option<String>) -> AttachTarget {
    AttachTarget::CreateIfMissing {
        name,
        command: None,
        cwd: directory.filter(|path| !path.is_empty()),
    }
}

/// Send `DETACH` and discard frames until `DETACHED`, so the server releases
/// per-consumer state before the next `ATTACH`. `orphan_kills`, the review
/// index, and the agent still see drained frames: they outlive the session.
async fn detach_and_drain(
    conn: &mut Connection,
    orphan_kills: &mut super::orphans::OrphanKills,
    review: &mut crate::attach::review::ReviewIndex,
    agent: Option<&AgentLink>,
) -> Result<(), AttachError> {
    conn.send(&FrameKind::Detach).await?;
    loop {
        let frame = conn.recv().await?;
        if agent.is_some_and(|agent| agent.take(&frame).is_some()) {
            continue;
        }
        match frame {
            FrameKind::Detached { .. } => {
                orphan_kills.finish_switch_drain();
                if let Some(agent) = agent {
                    agent.session_switched();
                }
                conn.unbind_all_terminals();
                return Ok(());
            }
            other => {
                tracing::trace!(kind = ?other, "draining frame during session switch");
                review.observe_switch_drain(&other);
                orphan_kills.observe_switch_drain(other, std::time::Instant::now());
            }
        }
    }
}
