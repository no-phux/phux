//! `ATTACH` handshake: tombstones, deferred roles, and mid-capture closes.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;

/// A Terminal handle with every channel closed (no actor behind it).
fn detached_handle(
    facet: crate::terminal_actor::TerminalHandle,
) -> crate::resource::ResourceHandle {
    crate::resource::ResourceHandle::new(
        None,
        tokio::sync::broadcast::channel(8).0,
        tokio::sync::mpsc::channel(1).0,
        facet,
    )
}

/// Drive `handle_attach` for session `name` with fixed defaults.
#[allow(
    clippy::too_many_arguments,
    reason = "test shorthand for handle_attach"
)]
pub(super) async fn attach(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    name: &str,
    role: Option<phux_protocol::wire::frame::RolePolicy>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    profile: BootstrapProfile,
    output_pumps: &mut JoinSet<()>,
    connection_token: &CancellationToken,
    defer_subscription: bool,
) {
    handle_attach(
        state,
        client_id,
        attach_id,
        AttachTarget::ByName(name.to_owned()),
        phux_protocol::wire::frame::ViewportInfo::new(80, 24),
        false,
        0,
        role,
        out_tx,
        ClientCapabilities::default(),
        profile,
        BootstrapLimits::default(),
        &CancellationToken::new(),
        output_pumps,
        connection_token,
        defer_subscription,
    )
    .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) fn native_attach_handle() -> (
    ResourceHandle,
    tokio::sync::mpsc::Receiver<crate::terminal_actor::ConsumerAttachRequest>,
    tokio::sync::mpsc::Receiver<crate::terminal_actor::NativeBootstrapRequest>,
    tokio::sync::mpsc::Receiver<crate::terminal_actor::NativePublicationRequest>,
) {
    let (consumer_attach, consumer_attach_rx) = tokio::sync::mpsc::channel(8);
    let (native_bootstrap, native_bootstrap_rx) = tokio::sync::mpsc::channel(8);
    let (native_publication, native_publication_rx) = tokio::sync::mpsc::channel(8);
    let handle = detached_handle(crate::terminal_actor::TerminalHandle {
        consumer_attach,
        native_bootstrap,
        native_publication,
        ..crate::terminal_actor::TerminalHandle::detached_for_test(80, 24)
    });
    (
        handle,
        consumer_attach_rx,
        native_bootstrap_rx,
        native_publication_rx,
    )
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) async fn answer_native_attach(
    consumer_attach_rx: &mut tokio::sync::mpsc::Receiver<
        crate::terminal_actor::ConsumerAttachRequest,
    >,
    native_bootstrap_rx: &mut tokio::sync::mpsc::Receiver<
        crate::terminal_actor::NativeBootstrapRequest,
    >,
    native_publication_rx: &mut tokio::sync::mpsc::Receiver<
        crate::terminal_actor::NativePublicationRequest,
    >,
    succeed: bool,
) {
    let registration = consumer_attach_rx
        .recv()
        .await
        .expect("consumer registration");
    registration
        .reply
        .send(Ok(crate::terminal_actor::ConsumerAttachOutcome {
            tick_managed: false,
            state_sync_bootstrap: None,
        }))
        .expect("consumer registration reply");
    let native = native_bootstrap_rx.recv().await.expect("native preflight");
    if !succeed {
        native
            .reply
            .send(Err(crate::native_state::NativeStateError::LimitExceeded))
            .expect("continuation-cap failure reply");
        return;
    }
    let terminal_id = native.terminal_id.clone();
    native
        .reply
        .send(Ok(crate::terminal_actor::NativeBootstrapReply {
            frames: vec![
                FrameKind::BootstrapBegin {
                    terminal_id: terminal_id.clone(),
                    stream_id: native.stream_id,
                    bootstrap_id: native.bootstrap_id,
                    profile: BootstrapStreamProfile::NativeState {
                        codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
                    },
                    cols: 80,
                    rows: 24,
                    base_seq: 0,
                },
                FrameKind::BootstrapChunk {
                    terminal_id: terminal_id.clone(),
                    stream_id: native.stream_id,
                    bootstrap_id: native.bootstrap_id,
                    chunk_seq: 0,
                    payload: bytes::Bytes::from_static(b"opaque"),
                },
                FrameKind::BootstrapReady {
                    terminal_id,
                    stream_id: native.stream_id,
                    bootstrap_id: native.bootstrap_id,
                    history_cursor: None,
                },
            ],
            retained_bytes: b"opaque".len(),
            base_seq: 0,
            publication_cursor: [7; 32],
        }))
        .expect("native success reply");
    let publication = native_publication_rx
        .recv()
        .await
        .expect("native publication fence");
    assert_eq!(publication.cursor, [7; 32]);
    publication
        .reply
        .send(Ok(crate::terminal_actor::NativePublicationReply {
            replay: Vec::new(),
            live: tokio::sync::broadcast::channel(1).1,
        }))
        .expect("native publication reply");
}

/// ADR-0127 (security review): a QUIC Terminal-stream reset tears the
/// subscription down through `handle_detach_terminal`, which must keep the
/// viewer tombstone, or the connection could shed it and then send
/// subscription-free input.
#[tokio::test(flavor = "current_thread")]
async fn a_terminal_stream_reset_keeps_the_viewer_tombstone() {
    let state = crate::state::SharedState::new();
    let (_session, pane) = state.with_mut(|s| s.seed_session("roles"));
    let wire = state.with_mut(|s| s.intern_terminal_wire(pane));
    let client_id = state.with_mut(crate::state::ServerState::new_client_id);
    state.with_mut(|s| {
        let _ = s.apply_attach_role(
            client_id,
            &wire,
            Some(pane),
            phux_protocol::wire::frame::RolePolicy::VIEWER,
            false,
        );
    });
    let _ = crate::runtime::commands::handle_detach_terminal(&state, client_id, &wire).await;
    assert!(
        state.with(|s| s.is_viewer(client_id, &wire)),
        "a stream reset must not shed the viewer tombstone"
    );
}

/// ADR-0127: `STREAM_BIND` carries no role, so a deferred (QUIC
/// multi-stream) session `ATTACH` must apply its declared role itself.
#[tokio::test(flavor = "current_thread")]
async fn deferred_session_attach_applies_the_declared_role() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = crate::state::SharedState::new();
            let (_session, pane) = state.with_mut(|s| s.seed_session("roles"));
            let handle = detached_handle(crate::terminal_actor::TerminalHandle::detached_for_test(
                80, 24,
            ));
            let wire = state.with_mut(|s| {
                let _ = s.register_resource_handle(pane, handle, CancellationToken::new());
                s.intern_terminal_wire(pane)
            });
            let (out_tx, _out_rx) = tokio::sync::mpsc::channel::<crate::state::Outbound>(
                crate::state::DEFAULT_CLIENT_MAILBOX,
            );
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            attach(
                &state,
                client_id,
                1,
                "roles",
                Some(phux_protocol::wire::frame::RolePolicy::VIEWER),
                &out_tx,
                BootstrapProfile::SynthesizedVtRaw,
                &mut JoinSet::new(),
                &CancellationToken::new(),
                true,
            )
            .await;
            assert!(
                state.with(|s| s.is_viewer(client_id, &wire)),
                "a deferred (QUIC multi-stream) session ATTACH must still mark its viewer"
            );
        })
        .await;
}

/// phux-ginr: a pane that closes between the attach's snapshot and its
/// capture (here, an actor whose mailboxes are already closed, as after a
/// reap) is published as closed beside its live neighbour, and the attach
/// still reaches `ATTACH_READY` (L1 §8: READY *or closed*). Before the
/// fix it failed the whole attach and cancelled the connection.
#[tokio::test(flavor = "current_thread")]
async fn a_pane_closed_mid_capture_is_published_closed_not_a_failed_attach() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = crate::state::SharedState::new();
            let (window, live) = state.with_mut(|s| s.seed_session("closing"));
            let token = CancellationToken::new();
            let bundle = crate::terminal_actor::TerminalActor::build_with_token(
                80,
                24,
                None,
                phux_config::ScrollbackLimits::default(),
                token.clone(),
            )
            .expect("test terminal actor");
            let (live_wire, closed_wire) = state.with_mut(|s| {
                let _ = s.register_resource_handle(live, bundle.handle.clone(), token.clone());
                let closed = s
                    .add_pane_to_session(window)
                    .expect("second pane");
                let ended = detached_handle(
                    crate::terminal_actor::TerminalHandle::detached_for_test(80, 24),
                );
                let _ = s.register_resource_handle(closed, ended, CancellationToken::new());
                (s.intern_terminal_wire(live), s.intern_terminal_wire(closed))
            });
            tokio::task::spawn_local(bundle.actor.run());
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
            let connection_token = CancellationToken::new();
            attach(
                &state,
                client_id,
                7,
                "closing",
                None,
                &out_tx,
                BootstrapProfile::SynthesizedVtRaw,
                &mut JoinSet::new(),
                &connection_token,
                false,
            )
            .await;

            let mut frames = Vec::new();
            while let Ok(Outbound::Frame(frame)) = out_rx.try_recv() {
                frames.push(frame);
            }
            assert!(
                !connection_token.is_cancelled(),
                "a pane closing mid-capture must not fail the attach: {frames:?}"
            );
            let ready_for = |wire: &phux_protocol::ids::ResourceId| {
                frames.iter().any(|frame| {
                    matches!(frame, FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == wire)
                })
            };
            assert!(ready_for(&live_wire), "the live pane bootstraps: {frames:?}");
            assert!(!ready_for(&closed_wire), "{frames:?}");
            let closed_at = frames.iter().position(|frame| {
                matches!(frame, FrameKind::ResourceClosed { terminal_id, .. } if *terminal_id == closed_wire)
            });
            let ready_at = frames
                .iter()
                .position(|frame| matches!(frame, FrameKind::AttachReady { attach_id: 7 }));
            assert!(
                matches!((closed_at, ready_at), (Some(closed), Some(ready)) if closed < ready),
                "RESOURCE_CLOSED for the closed pane precedes ATTACH_READY: {frames:?}"
            );
            token.cancel();
        })
        .await;
}
