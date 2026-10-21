// L3 metadata dispatch (SPEC §7.4 / §11.L3). Replies are gated on
// `client_speaks_l3` (SPEC §16.4): a non-L3 consumer gets silence.

pub(crate) async fn handle_get_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    out_tx: &mpsc::Sender<Outbound>,
) {
    state.with(|s| kick_satellite_metadata_mirror(s, scope, key));
    let nonce_result = is_reserved_session_create_result(scope, key);
    let (value, speaks_l3) = state.with(|s| {
        (
            read_metadata_value(s, client_id, scope, key, nonce_result),
            s.client_speaks_l3(client_id),
        )
    });
    let one_shot = value.is_some() && nonce_result;
    debug!(
        ?client_id,
        request_id,
        ?scope,
        %key,
        present = value.is_some(),
        speaks_l3,
        "GET_METADATA",
    );
    if !speaks_l3 {
        return;
    }
    if out_tx
        .send(Outbound::Frame(FrameKind::MetadataValue {
            request_id,
            value,
        }))
        .await
        .is_err()
    {
        trace!(
            ?client_id,
            request_id, "METADATA_VALUE send dropped: writer gone"
        );
    } else if one_shot {
        state.with_mut(|s| s.consume_session_create_result(key));
    }
}

/// The value one `GET_METADATA` answers with: the asking connection's own
/// `phux.whoami/v1` record (never stored), a nonce-bearing create result only
/// its owner may read, or whatever the store holds.
fn read_metadata_value(
    s: &ServerState,
    client_id: ClientId,
    scope: &Scope,
    key: &str,
    nonce_result: bool,
) -> Option<Vec<u8>> {
    if super::whoami::is_whoami_key(scope, key) {
        return super::whoami::record_for(s, client_id);
    }
    if nonce_result && !s.owns_session_create_result(client_id, key) {
        return None;
    }
    s.metadata().get(scope, key)
}

/// Why a client write or delete of `(scope, key)` is ignored, if it is.
fn protected_key_refusal(scope: &Scope, key: &str) -> Option<&'static str> {
    if is_reserved_session_create_result(scope, key) {
        return Some("reserved session-create result key");
    }
    if is_satellite_terminal_scope(scope) {
        return Some("satellite terminal metadata is read-only");
    }
    if is_server_owned_key(key) {
        return Some("server-owned key");
    }
    None
}

/// Keys only the server writes, in any scope.
fn is_server_owned_key(key: &str) -> bool {
    use phux_protocol::wire::frame::{
        APPROVAL_KEY_PREFIX, RESOURCE_ASKED_KEY, RESOURCE_PANE_OCCUPANT_KEY, WHOAMI_KEY,
    };

    key == RESOURCE_PANE_OCCUPANT_KEY
        || key == RESOURCE_ASKED_KEY
        || key == phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_KEY
        || key.starts_with(phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_RECORD_PREFIX)
        || key == WHOAMI_KEY
        || key.starts_with(APPROVAL_KEY_PREFIX)
}
