use super::metadata_read::protected_key_refusal;
use super::session_create::{apply_session_keep_empty, handle_session_create_metadata};
use crate::runtime::apps::terminal::agent_state::invalidate_agent_detector;
use crate::state::{ClientId, Outbound, ServerInterceptedKey, ServerState, SharedState};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_AGENT_KEY, Scope};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

/// Reject writes into a local Terminal namespace after its owner is gone.
fn reject_unknown_local_terminal_scope(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
) -> bool {
    let Scope::Resource(terminal @ WireResourceId::Local { .. }) = scope else {
        return false;
    };
    if state.with(|s| s.terminal_from_wire(terminal)).is_some() {
        return false;
    }
    warn!(
        ?client_id,
        request_id,
        ?terminal,
        %key,
        "SET_METADATA: unknown terminal scope; ignoring",
    );
    true
}

/// The writes `SET_METADATA` refuses outright. Every rejection is a logged
/// no-op: `SET_METADATA` has no reply frame.
fn reject_set_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: &[u8],
) -> bool {
    use phux_protocol::wire::frame::{
        MAX_AGENT_SESSION_RECORD_BYTES, RESOURCE_AGENT_SESSION_KEY, Scope,
    };

    if let Some(reason) = protected_key_refusal(scope, key) {
        warn!(?client_id, request_id, %key, reason, "SET_METADATA: ignoring");
        return true;
    }
    // ADR-0129 / L3 §2: an oversized value is refused whole.
    let cap = state.with(ServerState::metadata_value_bytes) as usize;
    if value.len() > cap {
        warn!(
            ?client_id,
            request_id,
            %key,
            value_len = value.len(),
            cap,
            "SET_METADATA: value exceeds limits.metadata-value-bytes; ignoring (no reply frame exists to tell the writer)"
        );
        return true;
    }
    // ADR-0129: a late write must not resurrect a reaped session's layout.
    if state.with(|s| s.layout_key_names_a_dead_session(scope, key)) == Some(true) {
        warn!(
            ?client_id,
            request_id,
            %key,
            "SET_METADATA: layout key names a session that is no longer live; ignoring"
        );
        return true;
    }
    if reject_unknown_local_terminal_scope(state, client_id, request_id, scope, key) {
        return true;
    }
    // The agent record is read by every client and by the server's own
    // arbiter; a writer may not make it larger than the shared reader takes.
    if key == RESOURCE_AGENT_KEY
        && value.len() > phux_client_core::agent_record::MAX_AGENT_RECORD_BYTES
    {
        warn!(
            ?client_id,
            request_id,
            value_len = value.len(),
            "SET_METADATA(agent): record exceeds 4096 bytes; ignoring",
        );
        return true;
    }
    if key == RESOURCE_AGENT_SESSION_KEY
        && (!matches!(scope, Scope::Resource(WireResourceId::Local { .. }))
            || value.is_empty()
            || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        warn!(
            ?client_id,
            request_id,
            ?scope,
            value_len = value.len(),
            "SET_METADATA(agent-session): want a local Terminal scope and 1..=4096 bytes; ignoring",
        );
        return true;
    }
    false
}

/// Translate a session rename written as `current_name\0new_name` into a
/// `SessionRename` commit attributed to the writer. An applied change fans out the `current\0new` value to the
/// key's subscribers; a malformed value or unknown session is a no-op.
fn apply_session_rename(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    value: &[u8],
) {
    let parsed = std::str::from_utf8(value).ok().and_then(|s| {
        s.split_once('\0')
            .map(|(cur, new)| (cur.to_owned(), new.to_owned()))
    });
    let Some((current, new_name)) = parsed else {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-name): malformed value (want current\\0new); ignoring",
        );
        return;
    };
    let (outcome, delivered) = state.with_mut(|s| {
        let actor = s.actor_for(client_id);
        let outcome = s.rename_session_as(&current, &new_name, &actor);
        // `Renamed` also covers a rename to the same name; broadcast only a
        // real change.
        let delivered =
            if matches!(outcome, crate::state::RenameOutcome::Renamed) && current != new_name {
                s.metadata_broadcast_by(
                    scope,
                    ServerInterceptedKey::SessionName,
                    value,
                    Some(client_id),
                )
            } else {
                Vec::new()
            };
        (outcome, delivered)
    });
    debug!(
        ?client_id,
        request_id,
        %current,
        %new_name,
        ?outcome,
        subscriber_count = delivered.len(),
        "SET_METADATA(session-name): applied registry rename",
    );
}

/// Store an ordinary metadata write and fan it out to subscribers. The only
/// path an explicit agent-record write takes, so it records the declaration
/// for the arbiter (ADR-0046 §E); the bytes alone cannot say who wrote them.
fn store_metadata_value(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: Vec<u8>,
) {
    let declared_agent_record = matches!(scope, Scope::Resource(_)) && key == RESOURCE_AGENT_KEY;
    let agent_value = declared_agent_record.then(|| value.clone());

    let (delivered, hook) = state.with_mut(|s| {
        let hook = if let (Some(bytes), Scope::Resource(terminal)) = (agent_value.as_deref(), scope)
        {
            s.agent_records_mut().note_explicit_set(terminal, bytes);
            agent_record_hook(s, terminal, scope, Some(bytes))
        } else {
            None
        };
        (s.metadata_set_by(scope, key, value, Some(client_id)), hook)
    });
    // The store just changed under the detector's edge filter.
    invalidate_agent_detector(state, scope, key);
    if let Some(event) = hook {
        crate::hooks::fire_hook(state, event);
    }
    trace!(
        ?client_id,
        request_id,
        subscriber_count = delivered.len(),
        "SET_METADATA delivered"
    );
}

/// The `agent-state-changed` hook an explicit write of `after` to the
/// pane's `phux.agent/v1` record owes, read before the store changes. `None`
/// when hooks are off or the effective state is unchanged.
fn agent_record_hook(
    s: &ServerState,
    terminal: &WireResourceId,
    scope: &Scope,
    after: Option<&[u8]>,
) -> Option<crate::hooks::HookEvent> {
    s.hook_dispatcher()?;
    let before = s.metadata().get(scope, RESOURCE_AGENT_KEY);
    crate::hooks::HookEvent::agent_record_transition(terminal, before.as_deref(), after)
}

pub(super) fn handle_set_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: Vec<u8>,
    root_token: &CancellationToken,
) {
    use phux_protocol::wire::frame::{SESSION_CREATE_KEY, Scope};

    debug!(?client_id, request_id, ?scope, %key, "SET_METADATA");
    if reject_set_metadata(state, client_id, request_id, scope, key, &value) {
        return;
    }
    // ADR-0019 / ADR-0027: these global writes commit session operations.
    if matches!(scope, Scope::Global) {
        match key {
            SESSION_CREATE_KEY => {
                handle_session_create_metadata(state, client_id, request_id, &value, root_token);
                return;
            }
            phux_protocol::wire::frame::SESSION_NAME_KEY => {
                apply_session_rename(state, client_id, request_id, scope, &value);
                return;
            }
            phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY => {
                apply_session_keep_empty(state, client_id, request_id, scope, &value, root_token);
                return;
            }
            _ => {}
        }
    }
    translate_layout_metadata(state, client_id, request_id, scope, key, &value);
    if !register_push_grant(state, client_id, request_id, scope, key, &value) {
        return;
    }
    let config_reload =
        key == phux_protocol::wire::frame::CONFIG_RELOAD_KEY && matches!(scope, Scope::Global);
    store_metadata_value(state, client_id, request_id, scope, key, value);
    // A hub is a doorbell consumer too: it re-reads its own `[[satellites]]`
    // (L3 §3.8), before any later frame on this connection is handled, so a
    // writer's read-back barrier also covers the reload.
    if config_reload {
        crate::hub::reload_satellites(state);
        crate::uplink::reload_uplinks(state);
        crate::plugin_startup::request_reload(state);
    }
}

/// Commit a TUI layout's arrangement before storing and broadcasting its metadata.
fn translate_layout_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: &[u8],
) {
    // R17: the TUI layout key is translated into the session's arrangement,
    // attributed to the writer, then stored and broadcast as before.
    if let Some(session) = state.with(|s| s.tui_layout_session(scope, key)) {
        let translated = state.with_mut(|s| {
            let actor = s.actor_for(client_id);
            s.translate_layout_write(session, value, &actor)
        });
        debug!(
            ?client_id,
            request_id,
            %key,
            path = "metadata",
            "arrangement save attempt"
        );
        if let Err(reason) = translated {
            info!(
                ?client_id,
                request_id,
                %key,
                reason = %reason,
                path = "metadata",
                "arrangement save refused"
            );
        } else {
            debug!(
                ?client_id,
                request_id,
                %key,
                path = "metadata",
                "arrangement save committed"
            );
        }
    }
}

/// Register the writer of a valid push grant before publishing that grant.
fn register_push_grant(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: &[u8],
) -> bool {
    // ADR-0155: a push grant is stored like any key, but the server also
    // remembers who wrote it, so a later ask is pushed only once that
    // connection is gone.
    if crate::push::is_grant_key(scope, key) {
        match crate::push::parse_grant(value) {
            Ok(_) => {
                let admitted = state.with_mut(|s| s.push_grants_mut().register(key, client_id));
                if !admitted {
                    warn!(?client_id, request_id, %key, "SET_METADATA: push grant registry full; ignoring");
                    return false;
                }
            }
            Err(reason) => {
                warn!(?client_id, request_id, %key, %reason, "SET_METADATA: ignoring push grant");
                return false;
            }
        }
    }
    true
}

pub(super) fn handle_delete_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
) {
    debug!(?client_id, request_id, ?scope, %key, "DELETE_METADATA");
    if let Some(reason) = protected_key_refusal(scope, key) {
        warn!(?client_id, request_id, %key, reason, "DELETE_METADATA: ignoring");
        return;
    }
    let (delivered, hook) = state.with_mut(|s| {
        // ADR-0046 §E: deleting the record withdraws any human declaration,
        // so the detector resumes ownership of this Terminal.
        let hook = if let Scope::Resource(terminal) = scope
            && key == RESOURCE_AGENT_KEY
        {
            s.agent_records_mut().note_explicit_delete(terminal);
            agent_record_hook(s, terminal, scope, None)
        } else {
            None
        };
        if crate::push::is_grant_key(scope, key) {
            s.push_grants_mut().forget(key);
        }
        (s.metadata_delete_by(scope, key, Some(client_id)), hook)
    });
    invalidate_agent_detector(state, scope, key);
    if let Some(event) = hook {
        crate::hooks::fire_hook(state, event);
    }
    trace!(
        ?client_id,
        request_id,
        subscriber_count = delivered.len(),
        "DELETE_METADATA delivered"
    );
}

pub(super) async fn handle_list_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    out_tx: &mpsc::Sender<Outbound>,
) {
    let (mut keys, speaks_l3) =
        state.with(|s| (s.metadata().list(scope), s.client_speaks_l3(client_id)));
    if matches!(scope, Scope::Global) {
        keys.retain(|key| {
            !key.starts_with(phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX)
        });
    }
    debug!(
        ?client_id,
        request_id,
        ?scope,
        key_count = keys.len(),
        speaks_l3,
        "LIST_METADATA",
    );
    if !speaks_l3 {
        return;
    }
    if out_tx
        .send(Outbound::Frame(FrameKind::MetadataKeys {
            request_id,
            keys,
        }))
        .await
        .is_err()
    {
        trace!(
            ?client_id,
            request_id, "METADATA_KEYS send dropped: writer gone"
        );
    }
}
