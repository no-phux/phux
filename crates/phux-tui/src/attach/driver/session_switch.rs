//! Same-connection switching: creation commits first, then ATTACH selects
//! only the exact identity returned by the organization receipt.

use phux_client::org;
use phux_protocol::ids::OperationId;
use phux_protocol::wire::frame::{AttachTarget, Committed, FrameKind, LaunchSpec, SessionLifetime};

use crate::attach::agent_forward::AgentLink;
use crate::attach::connection::Connection;
use crate::attach::input_dispatch::ReattachTarget;
use crate::attach::outcome::AttachError;

use super::loop_exit::EntryPick;
use super::session_io::{send_attach, wait_for_attached_serving};

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

/// Detach and re-handshake against `target` on the same connection. A
/// served agent keeps answering while the switch waits on the server.
pub(super) async fn reattach_on_same_connection(
    conn: &mut Connection,
    target: ReattachTarget,
    pick: &mut EntryPick,
    orphan_kills: &mut super::orphans::OrphanKills,
    review: &mut crate::attach::review::ReviewIndex,
    agent: Option<&AgentLink>,
) -> Result<FrameKind, AttachError> {
    let (attach_target, receipt) = match target {
        ReattachTarget::Existing { name, id, window, pane, resource } => {
            *pick = EntryPick { window, pane, resource };
            (id.map_or(AttachTarget::ByName(name), AttachTarget::ById), None)
        }
        ReattachTarget::Create { name, directory, host } => {
            // A satellite launch cannot be silently redirected to this server.
            if host.is_some() {
                return Err(AttachError::Refused(
                    "new-session on a satellite host is unsupported; nothing was created".into(),
                ));
            }
            // Check attach policy before accepting any persistent work.
            crate::attach::attach_role::check_attach_role(conn)?;
            let (receipt, target) = create_and_select(conn, org::mint_operation(), name, directory).await?;
            *pick = EntryPick::default();
            (target, Some(receipt))
        }
    };
    let result = async {
        detach_and_drain(conn, orphan_kills, review, agent).await?;
        let attach_id = send_attach(conn, attach_target).await?;
        wait_for_attached_serving(conn, attach_id, agent).await
    }.await;
    if result.is_ok() {
        tracing::info!("attach loop: re-attach handshake complete");
    }
    match receipt {
        Some(receipt) => result.map_err(|error| committed_selection_error(&receipt, error)),
        None => result,
    }
}

async fn create_and_select(
    conn: &mut Connection,
    operation: OperationId,
    name: String,
    directory: Option<String>,
) -> Result<(Committed, AttachTarget), AttachError> {
    let launch = LaunchSpec::default().with_cwd(directory.filter(|path| !path.is_empty()));
    let receipt = org::create_session(
        conn, operation, name, SessionLifetime::Persistent, Some(Box::new(launch)),
    ).await.map_err(creation_error)?;
    let target = org::attach_target_for_receipt(conn, &receipt).await
        .map_err(|error| committed_selection_error(&receipt, error))?;
    Ok((receipt, target))
}

fn creation_error(error: org::OrgError) -> AttachError {
    match error {
        org::OrgError::OutcomeUnknown { operation } => AttachError::OperationOutcomeUnknown { operation },
        other => AttachError::Refused(other.to_string()),
    }
}

fn committed_selection_error(receipt: &Committed, error: impl std::fmt::Display) -> AttachError {
    match receipt.session {
        Some(session) => AttachError::CommittedSelectionUnavailable {
            operation: receipt.operation,
            session,
            message: error.to_string(),
        },
        None => AttachError::OperationOutcomeUnknown { operation: Some(receipt.operation) },
    }
}

/// Release per-consumer state before ATTACH. Connection-lifetime observers
/// (orphan kills, the review index, the agent) still see drained frames; no
/// failed creation is implicitly killed.
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;
