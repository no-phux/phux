#[derive(serde::Deserialize)]
struct SessionCreateRequest {
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    agent_session: Option<Vec<u8>>,
    #[serde(default)]
    request_token: Option<String>,
    /// ADR-0105: the session survives its last window.
    #[serde(default)]
    keep_empty: bool,
    /// ADR-0105: create the session with no seed terminal (implies
    /// `keep_empty`); refused alongside a `command`.
    #[serde(default)]
    empty: bool,
}

/// Translate a `SESSION_CREATE_KEY` request into a `SessionCreate` commit
/// attributed to the writer: `empty` creates an empty persistent session,
/// `keep_empty` a persistent one with a seed pane, and a bare create an
/// ephemeral one whose last pane's exit deletes it. Returns the seed pane's
/// wire id, or `None` for an empty session.
fn run_session_create(
    state: &SharedState,
    writer: ClientId,
    request: SessionCreateRequest,
    root_token: &CancellationToken,
) -> Result<Option<WireResourceId>, String> {
    if request.empty {
        let actor = state.with(|s| s.actor_for(writer));
        return crate::runtime::commands::create_empty_session(state, &request.name, &actor)
            .map(|()| None);
    }
    // The seed pane's `pane_spawned` names this writer and the create's
    // token (ADR-0123, ADR-0126).
    let origin = crate::runtime::commands::SeedOrigin {
        agent_session: request.agent_session,
        attribution: crate::runtime::commands::SpawnAttribution {
            actor: Some(writer),
            operation_id: request
                .request_token
                .as_deref()
                .and_then(crate::runtime::idempotent_create::uuid_bytes)
                .and_then(phux_protocol::ids::IdempotencyKey::new),
            retain_secs: None,
        },
        lifetime: if request.keep_empty {
            phux_core::SessionLifetime::Persistent
        } else {
            phux_core::SessionLifetime::Ephemeral
        },
    };
    let wire = crate::runtime::commands::create_named_session(
        state,
        &request.name,
        request.command,
        request.cwd.as_deref(),
        request.env,
        origin,
        root_token,
    )?;
    Ok(Some(wire))
}

/// The result document published under a create's result key. `terminal_id`
/// is `null` and `empty` is `true` for an empty session; a seeded session's
/// document keeps its pre-ADR-0105 fields. `session_id` binds the receipt to
/// the created identity even if another client later renames the session.
fn session_create_payload(
    name: &str,
    session_id: Option<u32>,
    wire: Option<&WireResourceId>,
    request_token: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "name": name,
        "session_id": session_id,
        "terminal_id": wire.map(WireResourceId::local_id),
        "request_token": request_token,
    });
    if wire.is_none() {
        payload["empty"] = serde_json::Value::Bool(true);
    }
    payload
}

/// Translate a `phux.session.keep_empty/v1` write into a
/// `SessionSetLifetime` commit attributed to the writer: `true` is
/// persistent, `false` ephemeral. A change is broadcast to the key's
/// subscribers; ephemeral on an empty session deletes it, detaching its
/// clients and possibly self-exiting.
fn apply_session_keep_empty(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    value: &[u8],
    root_token: &CancellationToken,
) {
    use crate::state::KeepEmptyOutcome;

    let Some((name, keep)) = phux_protocol::wire::frame::decode_session_keep_empty(value) else {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-keep-empty): malformed value (want name\\0true|false); ignoring",
        );
        return;
    };
    let (outcome, clients, server_drained) = state.with_mut(|s| {
        let actor = s.actor_for(client_id);
        let outcome = s.set_session_keep_empty(name, keep, &actor);
        if matches!(
            outcome,
            KeepEmptyOutcome::Changed | KeepEmptyOutcome::Removed
        ) {
            let _ = s.metadata_broadcast_by(
                scope,
                ServerInterceptedKey::SessionKeepEmpty,
                value,
                Some(client_id),
            );
        }
        let clients: Vec<_> = s
            .take_killed_sessions()
            .into_iter()
            .flat_map(|session| s.attached_clients_in_session(session))
            .collect();
        let drained = outcome == KeepEmptyOutcome::Removed && s.self_exit_due();
        (outcome, clients, drained)
    });
    debug!(
        ?client_id,
        request_id,
        %name,
        keep,
        ?outcome,
        "SET_METADATA(session-keep-empty): applied",
    );
    detach_clients_of_killed_session(state, clients);
    if server_drained && !root_token.is_cancelled() {
        info!("nothing left running after serving clients; server self-exit");
        root_token.cancel();
    }
}

/// Detach the clients attached to a session that was just killed, with
/// `DETACHED { SESSION_KILLED }`. Each delivery waits in its own task, so a
/// wedged client's full mailbox cannot block the write that killed it.
pub(super) fn detach_clients_of_killed_session(
    state: &SharedState,
    clients: Vec<(ClientId, mpsc::Sender<Outbound>)>,
) {
    for (detached_client, tx) in clients {
        let detached_state = state.clone();
        tokio::task::spawn_local(async move {
            let _ = tx
                .send(Outbound::Frame(FrameKind::Detached {
                    reason: Some(DetachReason::SessionKilled),
                    message: "the session this attach was rooted in was killed".to_owned(),
                }))
                .await;
            detach_and_release_consumer_state(&detached_state, detached_client);
        });
    }
}

/// Why a parsed `SESSION_CREATE_KEY` request is ignored, if it is. Each
/// refusal is silent on the wire (`SET_METADATA` has no reply) and logged by
/// the caller.
fn session_create_refusal(request: &SessionCreateRequest) -> Option<&'static str> {
    use phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES;

    // ADR-0105: an empty session has no seed terminal.
    if request.empty && (request.command.is_some() || request.agent_session.is_some()) {
        return Some("`empty` cannot carry a command or agent session");
    }
    if request
        .agent_session
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        return Some("invalid agent session record size");
    }
    if request
        .request_token
        .as_deref()
        .is_some_and(|token| !valid_session_create_token(token))
    {
        return Some("invalid request token");
    }
    None
}

/// The key a create publishes its result under: a one-shot, request-specific
/// key for a nonce-bearing client, the original global key for a legacy one.
fn session_create_result_key(request_token: Option<&str>) -> String {
    use phux_protocol::wire::frame::{SESSION_CREATE_RESULT_KEY, SESSION_CREATE_RESULT_KEY_PREFIX};

    request_token.map_or_else(
        || SESSION_CREATE_RESULT_KEY.to_owned(),
        |token| format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}"),
    )
}

/// Publish a successful create's result document under `result_key`, and
/// track a one-shot key so it is consumed on its owner's first read.
fn publish_session_create_result(
    state: &SharedState,
    client_id: ClientId,
    result_key: String,
    payload: &serde_json::Value,
    one_shot: bool,
) {
    let Ok(bytes) = serde_json::to_vec(payload) else {
        return;
    };
    state.with_mut(|s| {
        let _ = s.metadata_set(&Scope::Global, &result_key, bytes);
        if one_shot {
            s.track_session_create_result(client_id, result_key);
        }
    });
}

/// The digest a create's token is bound to: every field of the request but
/// the token (ADR-0126). `env` is a sorted map, so the encoding is canonical.
fn session_create_digest(request: &SessionCreateRequest) -> [u8; 32] {
    use sha2::Digest as _;

    let canonical = serde_json::json!({
        "name": request.name,
        "command": request.command,
        "cwd": request.cwd,
        "env": request.env,
        "agent_session": request.agent_session,
        "keep_empty": request.keep_empty,
        "empty": request.empty,
    });
    sha2::Sha256::digest(canonical.to_string().as_bytes()).into()
}

fn valid_session_create_token(token: &str) -> bool {
    token.len() == 36
        && token.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn is_reserved_session_create_result(scope: &Scope, key: &str) -> bool {
    matches!(scope, Scope::Global)
        && key.starts_with(phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX)
}

fn handle_session_create_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    value: &[u8],
    root_token: &CancellationToken,
) {
    use crate::runtime::idempotent_create::SessionCreateAdmission;

    let Ok(request) = serde_json::from_slice::<SessionCreateRequest>(value) else {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-create): malformed JSON value (want {{name, command?, cwd?, env?, request_token?, agent_session?, keep_empty?, empty?}}); ignoring",
        );
        return;
    };
    if let Some(reason) = session_create_refusal(&request) {
        warn!(
            ?client_id,
            request_id, reason, "SET_METADATA(session-create): refused; ignoring"
        );
        return;
    }
    // ADR-0126: the request token is the create's idempotency key.
    let token = request
        .request_token
        .as_deref()
        .and_then(crate::runtime::idempotent_create::uuid_bytes);
    let claim = match crate::runtime::idempotent_create::admit_session_create(
        state,
        token,
        session_create_digest(&request),
    ) {
        SessionCreateAdmission::Unkeyed => None,
        SessionCreateAdmission::Owner(claim) => Some(claim),
        SessionCreateAdmission::Replay(mut payload) => {
            // Dedupe is case-insensitive; answer in the repeat's spelling.
            payload["request_token"] = serde_json::Value::from(request.request_token.clone());
            // The repeating connection becomes the result's only owner: the
            // original sender may be the connection whose loss caused it.
            let result_key = session_create_result_key(request.request_token.as_deref());
            state.with_mut(|s| s.disown_session_create_result(&result_key));
            publish_session_create_result(state, client_id, result_key, &payload, true);
            debug!(
                ?client_id,
                request_id, "SET_METADATA(session-create): repeat replayed the original result"
            );
            return;
        }
        SessionCreateAdmission::Refused(reason) => {
            warn!(
                ?client_id,
                request_id, reason, "SET_METADATA(session-create): refused; ignoring"
            );
            return;
        }
    };
    run_and_publish_session_create(
        state,
        client_id,
        request_id,
        request,
        claim.as_ref(),
        root_token,
    );
    // An unbound claim releases the token, so a failed create can repeat.
    drop(claim);
}

/// Run an admitted create and publish its result. A keyed create binds the
/// result to its token before publishing, so a repeat answers the same one.
fn run_and_publish_session_create(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    request: SessionCreateRequest,
    claim: Option<&crate::runtime::operation_dedupe::OperationClaim>,
    root_token: &CancellationToken,
) {
    let name = request.name.clone();
    let request_token = request.request_token.clone();
    let result_key = session_create_result_key(request_token.as_deref());
    if request_token.is_some() && state.with(|s| s.session_create_result_is_pending(&result_key)) {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-create): request token already has a pending result; ignoring"
        );
        return;
    }
    let outcome = run_session_create(state, client_id, request, root_token);
    if let Ok(wire) = &outcome {
        // Creation and this lookup run synchronously in the same server turn.
        let session_id = state.with_mut(|s| {
            s.find_session_by_name(&name)
                .map(|id| s.idspace.intern_session(id).get())
        });
        let payload =
            session_create_payload(&name, session_id, wire.as_ref(), request_token.as_deref());
        if let Some(claim) = claim {
            claim.bind(
                &crate::runtime::operation_dedupe::CachedOutcome::SessionCreate(payload.clone()),
            );
        }
        publish_session_create_result(
            state,
            client_id,
            result_key,
            &payload,
            request_token.is_some(),
        );
    }
    debug!(
        ?client_id,
        request_id,
        %name,
        ok = outcome.is_ok(),
        "SET_METADATA(session-create): create attempted",
    );
}
