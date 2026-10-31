//! `MOVE_RESOURCE` handling.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// Handle `MOVE_RESOURCE` (ADR-0056): translate into a `ResourceMove`
/// commit that moves `terminal` into the session holding `owner_terminal`,
/// attributed to the caller; an ephemeral source it empties is deleted in
/// the same commit. The `ResourceId` is stable, so subscriptions survive;
/// layout is the caller's concern. Local-only.
pub(crate) async fn handle_move_terminal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    terminal: phux_protocol::ids::ResourceId,
    owner_terminal: phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    debug!(
        ?client_id,
        request_id,
        terminal = ?terminal,
        owner_terminal = ?owner_terminal,
        "MOVE_RESOURCE",
    );

    let (result, clients_to_detach) = match state.with_mut(|s| {
        let actor = s.actor_for(client_id);
        move_local_terminal(s, &terminal, &owner_terminal, &actor)
    }) {
        Ok(clients) => (MoveResult::Ok(terminal), clients),
        Err(error) => (MoveResult::Err(error), Vec::new()),
    };

    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceMoved {
            request_id,
            result,
        }))
        .await;

    // Clients attached to a reaped session get DETACHED, each in its own task
    // so a wedged mailbox cannot block this command. Headless resource
    // subscriptions keep streaming the stable id.
    for (detached_client, tx) in clients_to_detach {
        let detached_state = state.clone();
        tokio::task::spawn_local(async move {
            let _ = tx
                .send(Outbound::Frame(FrameKind::Detached {
                    reason: Some(DetachReason::SessionKilled),
                    message: "the session this attach was rooted in was reaped".to_owned(),
                }))
                .await;
            crate::runtime::client::detach_and_release_consumer_state(
                &detached_state,
                detached_client,
            );
        });
    }
}

/// Move `terminal` into `owner_terminal`'s session under one lock. Returns
/// the clients attached to a session the move deleted.
fn move_local_terminal(
    s: &mut crate::state::ServerState,
    terminal: &phux_protocol::ids::ResourceId,
    owner_terminal: &phux_protocol::ids::ResourceId,
    actor: &phux_core::Actor,
) -> Result<Vec<(ClientId, tokio::sync::mpsc::Sender<Outbound>)>, MoveError> {
    let failed = |message: &str| MoveError::MoveFailed(message.to_owned());
    if !terminal.is_local() || !owner_terminal.is_local() {
        return Err(MoveError::UnsupportedSatelliteRoute);
    }
    let moved = s
        .terminal_from_wire(terminal)
        .ok_or_else(|| failed("terminal was not found on this server"))?;
    let owner = s
        .terminal_from_wire(owner_terminal)
        .ok_or_else(|| failed("owner terminal was not found on this server"))?;
    let to = s
        .registry()
        .session_of(owner)
        .ok_or_else(|| failed("owner terminal has no session on this server"))?;
    s.move_resource_to(moved, to, actor)
        .map_err(|refusal| MoveError::MoveFailed(refusal.to_string()))?;
    // A move that emptied an ephemeral source deleted it in the same commit.
    Ok(s.take_killed_sessions()
        .into_iter()
        .flat_map(|session| s.attached_clients_in_session(session))
        .collect())
}
