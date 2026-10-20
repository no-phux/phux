use phux_protocol::ids::{ResourceId as WireResourceId, SatelliteHost};
use phux_protocol::wire::frame::{ErrorCode, FrameKind, Scope};
use tokio_util::sync::CancellationToken;

use super::{handle_delete_metadata, handle_set_metadata, handle_subscribe_metadata};
use crate::state::{ClientId, Outbound, SharedState};

const AGENT_KEY: &str = "phux.agent/v1";

fn set(state: &SharedState, scope: &Scope, key: &str, value: &[u8]) {
    handle_set_metadata(
        state,
        ClientId(1),
        1,
        scope,
        key,
        value.to_vec(),
        &CancellationToken::new(),
    );
}

fn get(state: &SharedState, scope: &Scope, key: &str) -> Option<Vec<u8>> {
    state.with(|s| s.metadata().get(scope, key))
}

fn satellite_scope() -> Scope {
    Scope::Resource(WireResourceId::Satellite {
        host: SatelliteHost::new("gpubox"),
        id: 7,
    })
}

fn expect_satellite_refusal(rx: &mut tokio::sync::mpsc::Receiver<Outbound>, needle: &str) {
    match rx.try_recv() {
        Ok(Outbound::Frame(FrameKind::Error {
            request_id: None,
            code: ErrorCode::UnsupportedSatelliteRoute,
            message,
        })) => {
            assert!(message.contains("does not federate"), "{message}");
            assert!(message.contains(needle), "{message}");
        }
        other => panic!("expected an uncorrelated satellite refusal, got {other:?}"),
    }
}

/// phux-w7z2.57: a `SUBSCRIBE_METADATA` naming a satellite pane is
/// refused with a typed error and records nothing, while local and
/// unscoped subscriptions are untouched.
#[test]
fn satellite_subscriptions_are_refused_and_local_ones_recorded() {
    let state = SharedState::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Outbound>(4);
    let client = ClientId(1);
    let scope = satellite_scope();
    handle_subscribe_metadata(&state, client, scope.clone(), AGENT_KEY.to_owned(), &tx);
    assert!(
        state
            .with(|s| s.metadata().subscribers_for(&scope, AGENT_KEY))
            .is_empty()
    );
    expect_satellite_refusal(&mut rx, "gpubox");

    let local = Scope::Resource(WireResourceId::local(1));
    handle_subscribe_metadata(&state, client, local.clone(), AGENT_KEY.to_owned(), &tx);
    handle_subscribe_metadata(
        &state,
        client,
        Scope::Global,
        "phux.tui.focus/v1".to_owned(),
        &tx,
    );
    assert_eq!(
        state.with(|s| s.metadata().subscribers_for(&local, AGENT_KEY)),
        vec![client]
    );
    assert_eq!(
        state.with(|s| s
            .metadata()
            .subscribers_for(&Scope::Global, "phux.tui.focus/v1")),
        vec![client]
    );
    assert!(
        rx.try_recv().is_err(),
        "an accepted subscription pushes nothing"
    );
}

/// ADR-0136: on a hub that routes the host, the two agent keys install a
/// subscription and queue the mirror; any other key still refuses, and a
/// client cannot write the satellite scope.
#[test]
fn a_hub_mirrors_the_agent_allowlist_and_refuses_the_rest() {
    use phux_protocol::wire::frame::RESOURCE_ASKED_KEY;

    use crate::hub::relay::{HubRelays, RelayHandle, RelayRequest};

    let state = SharedState::new();
    let (handle, mut mailbox) = RelayHandle::new(SatelliteHost::new("gpubox"));
    let relays = HubRelays::default();
    relays.insert(handle);
    state.with_mut(|s| s.set_hub_relays(relays));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Outbound>(4);
    let client = ClientId(3);
    let scope = satellite_scope();

    for key in [AGENT_KEY, RESOURCE_ASKED_KEY] {
        handle_subscribe_metadata(&state, client, scope.clone(), key.to_owned(), &tx);
        assert_eq!(
            state
                .with(|s| s.metadata().subscribers_for(&scope, key))
                .len(),
            1
        );
        assert!(matches!(
            mailbox.requests.try_recv(),
            Ok(RelayRequest::MirrorTerminal { terminal: 7 })
        ));
    }
    assert!(rx.try_recv().is_err());

    handle_subscribe_metadata(
        &state,
        client,
        scope.clone(),
        "phux.tags/v1".to_owned(),
        &tx,
    );
    expect_satellite_refusal(&mut rx, "phux.tags/v1");

    let mirrored = br#"{"name":"reviewer"}"#;
    state.with_mut(|s| s.metadata_set(&scope, AGENT_KEY, mirrored.to_vec()));
    set(&state, &scope, AGENT_KEY, b"overwrite");
    handle_delete_metadata(&state, client, 2, &scope, AGENT_KEY);
    assert_eq!(
        get(&state, &scope, AGENT_KEY).as_deref(),
        Some(&mirrored[..])
    );
}

#[test]
fn terminal_metadata_cannot_outlive_or_target_a_missing_terminal() {
    use phux_protocol::wire::frame::RESOURCE_AGENT_SESSION_KEY;

    let state = SharedState::new();
    let (_session, pane) = state.with_mut(|s| s.seed_session("scope-test"));
    let scope = Scope::Resource(state.with_mut(|s| s.intern_terminal_wire(pane)));
    set(&state, &scope, "phux.test/v1", b"live");
    assert_eq!(get(&state, &scope, "phux.test/v1"), Some(b"live".to_vec()));

    for invalid in [Vec::new(), vec![b'x'; 4097]] {
        set(&state, &scope, RESOURCE_AGENT_SESSION_KEY, &invalid);
        assert!(get(&state, &scope, RESOURCE_AGENT_SESSION_KEY).is_none());
    }

    state.with_mut(|s| s.reap_terminal(pane));
    set(&state, &scope, "phux.test/v1", b"orphan");
    assert!(get(&state, &scope, "phux.test/v1").is_none());

    let missing = Scope::Resource(WireResourceId::local(u32::MAX));
    set(&state, &missing, "phux.test/v1", b"missing");
    assert!(get(&state, &missing, "phux.test/v1").is_none());
}

/// ADR-0129 / L3 §2: an over-cap value is refused whole; a value exactly
/// at the cap is stored.
#[test]
fn a_metadata_value_over_the_cap_is_refused_and_nothing_is_stored() {
    let state = SharedState::new();
    let cap = state.with(crate::state::ServerState::metadata_value_bytes) as usize;
    set(
        &state,
        &Scope::Global,
        "phux.test.cap/v1",
        &vec![b'x'; cap + 1],
    );
    assert!(get(&state, &Scope::Global, "phux.test.cap/v1").is_none());
    let at_cap = vec![b'x'; cap];
    set(&state, &Scope::Global, "phux.test.cap/v1", &at_cap);
    assert_eq!(
        get(&state, &Scope::Global, "phux.test.cap/v1"),
        Some(at_cap)
    );
}

/// ADR-0129: the TUI's post-close layout republish can land after the
/// reap; it must not resurrect the dead session's layout key.
#[test]
fn a_late_layout_write_for_a_reaped_session_is_not_stored() {
    let state = SharedState::new();
    let (session, pane) = state.with_mut(|s| {
        s.create_terminal(
            crate::state::TerminalTarget::NewSession {
                name: "resurrect-test",
                lifetime: phux_core::SessionLifetime::Ephemeral,
            },
            phux_core::TerminalLaunch::default(),
            phux_core::ExitPolicy::Remove,
            &phux_core::Actor::owner(),
        )
        .expect("ephemeral session")
    });
    let wire = state.with_mut(|s| s.idspace.intern_session(session));
    let key = format!("phux.tui.layout/v1/{}", wire.get());
    let scope = Scope::Group(crate::state::DEFAULT_GROUP_ID);
    set(&state, &scope, &key, b"live");
    assert_eq!(get(&state, &scope, &key), Some(b"live".to_vec()));
    state.with_mut(|s| s.reap_terminal(pane));
    assert!(state.with(|s| s.registry().session(session).is_none()));
    set(&state, &scope, &key, b"late republish");
    assert!(get(&state, &scope, &key).is_none());
}

/// Server-owned records and the owner-only create-result namespace refuse
/// client writes and deletes.
#[test]
fn clients_cannot_forge_server_owned_or_reserved_keys() {
    let state = SharedState::new();
    let reserved_key = format!(
        "{}11111111-1111-4111-8111-111111111111",
        phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX,
    );
    set(&state, &Scope::Global, &reserved_key, b"forged");
    assert!(get(&state, &Scope::Global, &reserved_key).is_none());

    let (_session, pane) = state.with_mut(|s| s.seed_session("occupant-owner"));
    let scope = Scope::Resource(state.with_mut(|s| s.intern_terminal_wire(pane)));
    let key = phux_protocol::wire::frame::RESOURCE_PANE_OCCUPANT_KEY;
    let authoritative = br#"{"foreground":"zsh","is_pane_shell":true}"#.to_vec();
    state.with_mut(|s| s.metadata_set(&scope, key, authoritative.clone()));
    set(
        &state,
        &scope,
        key,
        br#"{"foreground":"vim","is_pane_shell":true}"#,
    );
    handle_delete_metadata(&state, ClientId(2), 6, &scope, key);
    assert_eq!(get(&state, &scope, key), Some(authoritative));
}

/// A create receipt binds the created identity, so it survives a rename
/// and the reuse of the original name.
#[tokio::test(flavor = "current_thread")]
async fn empty_create_receipt_keeps_original_identity_after_name_reuse() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let nonce = "11111111-1111-4111-8111-111111111111";
            let value = serde_json::to_vec(&serde_json::json!({
                "name": "original", "request_token": nonce, "empty": true, "keep_empty": true,
            }))
            .expect("request JSON");
            set(
                &state,
                &Scope::Global,
                phux_protocol::wire::frame::SESSION_CREATE_KEY,
                &value,
            );
            let key = super::session_create_result_key(Some(nonce));
            let bytes = get(&state, &Scope::Global, &key).expect("receipt");
            let receipt: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
            let session_id = |state: &SharedState| {
                state.with_mut(|s| {
                    s.idspace
                        .intern_session(s.find_session_by_name("original").expect("session"))
                        .get()
                })
            };
            let original_id = session_id(&state);
            assert_eq!(receipt["session_id"], original_id);
            state.with_mut(|s| s.rename_session("original", "renamed"));
            crate::runtime::commands::create_empty_session(
                &state,
                "original",
                &phux_core::Actor::owner(),
            )
            .expect("replacement");
            assert_ne!(original_id, session_id(&state));
            assert_eq!(get(&state, &Scope::Global, &key), Some(bytes));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_session_create_token_cannot_be_reused_by_another_connection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let root_token = CancellationToken::new();
            for (client_id, name) in [(ClientId(1), "first"), (ClientId(2), "collision")] {
                let value = serde_json::to_vec(&serde_json::json!({
                    "name": name,
                    "request_token": "11111111-1111-4111-8111-111111111111",
                }))
                .expect("request JSON");
                handle_set_metadata(
                    &state,
                    client_id,
                    1,
                    &Scope::Global,
                    phux_protocol::wire::frame::SESSION_CREATE_KEY,
                    value,
                    &root_token,
                );
            }
            assert!(state.with(|s| s.session_by_name("first").is_some()));
            assert!(state.with(|s| s.session_by_name("collision").is_none()));
            root_token.cancel();
        })
        .await;
}
