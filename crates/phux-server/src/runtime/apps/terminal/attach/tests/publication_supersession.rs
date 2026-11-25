//! A real actor's post-READY resize refusal through the real replacement pump.

use super::*;
use phux_terminal::actor::{
    NativePublicationRequest, ScreenReply, ScreenRequest, TerminalActor, TerminalHandle,
    TerminalResource as _,
};

pub(super) fn context(
    terminal: TerminalHandle,
    client_id: ClientId,
) -> (OutputPumpContext, tokio::sync::mpsc::Receiver<Outbound>) {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(32);
    let ctx = OutputPumpContext {
        out_tx,
        resize: terminal.resize.clone(),
        wire_terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: two_pump_stream(),
        initial_bootstrap_id: two_pump_initial_generation(),
        client_id,
        client_caps: ClientCapabilities::default(),
        profile: BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
        },
        limits: BootstrapLimits::default(),
        lag_label: "real actor supersession fixture",
        stale_skip: false,
        cancel: None,
        drain: None,
        snapshot: None,
        last_seq: None,
        terminal,
    };
    (ctx, out_rx)
}

/// A screen reply acknowledges that the actor serviced the higher-priority
/// geometry queue. No timer, scheduler sleep, or fabricated refusal is involved.
pub(super) async fn resize_and_acknowledge(terminal: &TerminalHandle) {
    resize_to_and_acknowledge(terminal, 30, 8).await;
}

pub(super) async fn resize_to_and_acknowledge(terminal: &TerminalHandle, cols: u16, rows: u16) {
    terminal
        .resize
        .send(ResizeRequest {
            cols,
            rows,
            cell_px: None,
            resync_clients: true,
            resync_only: false,
            resync_for: None,
        })
        .await
        .expect("admitted resize");
    let (reply, response) = oneshot::channel();
    terminal
        .screen
        .send(ScreenRequest {
            pane: 1,
            scrollback: None,
            cells: false,
            format: 0,
            reply,
        })
        .await
        .expect("geometry acknowledgement request");
    let ScreenReply::Projection(screen) = response.await.expect("screen reply") else {
        panic!("bounded empty screen");
    };
    assert_eq!((screen.cols, screen.rows), (cols, rows));
}

fn queued_ready(out_rx: &mut tokio::sync::mpsc::Receiver<Outbound>) -> bool {
    let mut ready = false;
    while let Ok(outbound) = out_rx.try_recv() {
        ready |= matches!(outbound, Outbound::Frame(FrameKind::BootstrapReady { .. }));
    }
    ready
}

#[tokio::test(flavor = "current_thread")]
async fn a_real_actor_resize_after_ready_does_not_close_either_replacement_connection() {
    crate::runtime::test_support::local(async {
        let bundle = TerminalActor::new(20, 5).expect("real actor");
        let real = bundle.handle.terminal().expect("terminal").clone();
        let token = bundle.token.clone();
        let output = bundle.handle.output.clone();
        let actor = tokio::task::spawn_local(bundle.actor.run());
        let (barrier, mut activations) = tokio::sync::mpsc::channel::<NativePublicationRequest>(2);
        let mut held_terminal = real.clone();
        held_terminal.native_publication = barrier;
        let state = SharedState::new();
        let connections = [CancellationToken::new(), CancellationToken::new()];
        let mut tasks = Vec::new();
        let mut mailboxes = Vec::new();
        for (i, connection) in connections.iter().enumerate() {
            let (ctx, out_rx) = context(held_terminal.clone(), ClientId(7 + i as u64));
            let mut live = output.subscribe();
            let reservation = crate::runtime::apps::StreamHost::new(&state, ctx.client_id)
                .reserve(ResourceId::default(), ctx.initial_bootstrap_id, connection)
                .unwrap();
            let lease = reservation.lease.clone();
            assert!(reservation.published(|| true));
            tasks.push(tokio::task::spawn_local(async move {
                let mut generation = PumpGeneration::opened_at(0, two_pump_initial_generation());
                let result = ctx
                    .republish_generation(
                        &mut generation,
                        &mut live,
                        &PaneResync {
                            cols: 20,
                            rows: 5,
                            bytes: bytes::Bytes::new(),
                            base_seq: 0,
                            reason: crate::resource::ResyncReason::Resize,
                        },
                    )
                    .await;
                if let ControlFlow::Break(Some(fault)) = result {
                    lease.retire_terminal_fault(fault);
                }
                result
            }));
            mailboxes.push(out_rx);
        }

        // Each request is emitted only after its real checkpoint READY is queued.
        let held = [
            activations.recv().await.expect("first activation barrier"),
            activations.recv().await.expect("second activation barrier"),
        ];
        assert!(mailboxes.iter_mut().all(queued_ready));
        resize_and_acknowledge(&real).await;
        for mut request in held {
            let (reply, response) = oneshot::channel();
            let pump_reply = std::mem::replace(&mut request.reply, reply);
            real.native_publication
                .send(request)
                .await
                .expect("real activation");
            let result = response.await.expect("real actor reply");
            assert_eq!(
                result.as_ref().err(),
                Some(&phux_terminal_api::native_state::NativeStateError::Resize)
            );
            pump_reply.send(result).expect("pump awaiting activation");
        }
        for task in tasks {
            assert!(matches!(
                task.await.expect("pump"),
                ControlFlow::Continue(())
            ));
        }
        let cancelled = connections.each_ref().map(CancellationToken::is_cancelled);
        token.cancel();
        actor.await.expect("actor shutdown");
        assert_eq!(
            cancelled, [false; 2],
            "post-READY reflow must recover without closing old and current connections"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_real_actor_teardown_after_ready_is_quiet_pane_gone_not_supersession() {
    crate::runtime::test_support::local(async {
        let bundle = TerminalActor::new(20, 5).expect("actor");
        let real = bundle.handle.terminal().expect("terminal").clone();
        let token = bundle.token.clone();
        let output = bundle.handle.output.clone();
        let actor = tokio::task::spawn_local(bundle.actor.run());
        let (barrier, mut activations) = tokio::sync::mpsc::channel::<NativePublicationRequest>(2);
        let mut terminal = real.clone();
        terminal.native_publication = barrier;
        let state = SharedState::new();
        let connection = CancellationToken::new();
        let (ctx, mut mailbox) = context(terminal, ClientId(7));
        let mut live = output.subscribe();
        let reservation = crate::runtime::apps::StreamHost::new(&state, ctx.client_id)
            .reserve(ResourceId::default(), ctx.initial_bootstrap_id, &connection)
            .unwrap();
        let lease = reservation.lease.clone();
        assert!(reservation.published(|| true));
        let pump = tokio::task::spawn_local(async move {
            let mut generation = PumpGeneration::opened_at(0, two_pump_initial_generation());
            let result = ctx
                .republish_generation(
                    &mut generation,
                    &mut live,
                    &PaneResync {
                        cols: 20,
                        rows: 5,
                        bytes: bytes::Bytes::new(),
                        base_seq: 0,
                        reason: crate::resource::ResyncReason::Resize,
                    },
                )
                .await;
            if let ControlFlow::Break(Some(fault)) = result {
                lease.retire_terminal_fault(fault);
            }
            result
        });
        let request = activations.recv().await.expect("READY activation barrier");
        assert!(queued_ready(&mut mailbox));
        token.cancel();
        actor.await.expect("actual actor teardown");
        let unsent = real
            .native_publication
            .send(request)
            .await
            .expect_err("actor really gone");
        drop(unsent); // Drop the original reply, not a fabricated Resize answer.
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), pump)
            .await
            .expect("quiet pane-gone deadline")
            .expect("pump");
        assert!(matches!(
            result,
            ControlFlow::Break(Some(PumpFault::PaneGone))
        ));
        assert!(
            !connection.is_cancelled(),
            "pane close must not close its connection"
        );
        assert!(
            mailbox.try_recv().is_err(),
            "no fabricated activated successor"
        );
    })
    .await;
}
