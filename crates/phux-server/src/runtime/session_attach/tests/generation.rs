//! Dropping a partially captured aggregate retires both staged and pending interest.

use super::*;
use crate::runtime::apps::terminal::test_support::{
    StubRx, state_sync_outcome, stub_terminal, terminal_state,
};
use phux_protocol::caps::OutputMode;

fn panes() -> (
    SharedState,
    ClientId,
    [phux_core::ids::ResourceId; 2],
    [StubRx; 2],
) {
    let state = terminal_state();
    let (session, first) = state.with_mut(|s| s.seed_session("aggregate-generation"));
    let second = state.with_mut(|s| s.add_pane_to_session(session).unwrap());
    let (first_handle, first_rx) = stub_terminal();
    let (second_handle, second_rx) = stub_terminal();
    let client = state.with_mut(|s| {
        s.register_resource_handle(first, first_handle, CancellationToken::new())
            .unwrap();
        s.register_resource_handle(second, second_handle, CancellationToken::new())
            .unwrap();
        s.clients_mut().new_client_id()
    });
    (state, client, [first, second], [first_rx, second_rx])
}

fn start(
    state: &SharedState,
    client: ClientId,
    outbound: &mpsc::Sender<Outbound>,
    token: &CancellationToken,
) -> tokio::task::JoinHandle<(bool, JoinSet<()>)> {
    let state = state.clone();
    let outbound = outbound.clone();
    let token = token.clone();
    tokio::task::spawn_local(async move {
        let mut pumps = JoinSet::new();
        let result = handle_attach(
            &state,
            client,
            7,
            AttachTarget::ByName("aggregate-generation".to_owned()),
            ViewportInfo::new(0, 0),
            false,
            0,
            None,
            &outbound,
            ClientCapabilities::default().with_output_mode(OutputMode::StateSync),
            BootstrapProfile::SynthesizedVtStateSync,
            BootstrapLimits::default(),
            &CancellationToken::new(),
            &mut pumps,
            &token,
            false,
        )
        .await;
        (result, pumps)
    })
}

async fn next_registration(
    first: &mut StubRx,
    second: &mut StubRx,
) -> (bool, phux_terminal_api::actor::ConsumerAttachRequest) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            request = first.consumer_attach.recv() => (true, request.unwrap()),
            request = second.consumer_attach.recv() => (false, request.unwrap()),
        }
    })
    .await
    .expect("aggregate registration")
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_aggregate_future_withdraws_staged_and_pending_actor_generations() {
    crate::runtime::test_support::local(async {
        let (state, client, resources, [mut first, mut second]) = panes();
        let (outbound, mut output) = mpsc::channel(32);
        let connection = CancellationToken::new();
        let task = start(&state, client, &outbound, &connection);
        let (_, captured) = next_registration(&mut first, &mut second).await;
        let captured_interest = captured.cancellation.clone();
        let captured_gate = captured.live_gate.clone();
        captured
            .reply
            .send(Ok(state_sync_outcome(b"captured")))
            .unwrap();
        let (_, pending) = next_registration(&mut first, &mut second).await;
        assert!(!*captured_gate.borrow());
        assert!(!captured_interest.is_cancelled());
        assert!(
            output.try_recv().is_err(),
            "no ATTACHED or bootstrap escapes a partial aggregate"
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            captured_interest.cancelled(),
        )
        .await
        .unwrap();
        assert!(pending.cancellation.is_cancelled());
        assert!(pending.reply.send(Ok(state_sync_outcome(b"late"))).is_err());
        assert!(
            !connection.is_cancelled(),
            "drop ownership does not depend on connection cancellation"
        );
        assert!(
            !*captured_gate.borrow(),
            "aborted capture never releases live emission"
        );
        assert!(output.try_recv().is_err());
        assert!(
            state.with(|s| resources
                .iter()
                .all(|id| s.registry().resource(*id).is_some())),
            "withdrawing interest must not terminate shared resources"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn aggregate_ready_transfers_every_registration_to_connection_lifetime() {
    crate::runtime::test_support::local(async {
        let (state, client, _, [mut first, mut second]) = panes();
        let (outbound, mut output) = mpsc::channel(32);
        let connection = CancellationToken::new();
        let task = start(&state, client, &outbound, &connection);
        let (_, captured) = next_registration(&mut first, &mut second).await;
        let first_interest = captured.cancellation.clone();
        let first_gate = captured.live_gate.clone();
        captured
            .reply
            .send(Ok(state_sync_outcome(b"first")))
            .unwrap();
        let (_, pending) = next_registration(&mut first, &mut second).await;
        let second_interest = pending.cancellation.clone();
        assert!(!*first_gate.borrow());
        assert!(output.try_recv().is_err());
        pending
            .reply
            .send(Ok(state_sync_outcome(b"second")))
            .unwrap();
        let (attached, mut owners) = task.await.unwrap();
        assert!(attached);
        while let Some(result) = owners.join_next().await {
            result.unwrap();
        }
        assert!(*first_gate.borrow());
        let mut frames = Vec::new();
        while let Ok(Outbound::Frame(frame)) = output.try_recv() {
            frames.push(frame);
        }
        assert!(matches!(
            frames.first(),
            Some(FrameKind::Attached { attach_id: 7, .. })
        ));
        assert!(matches!(
            frames.last(),
            Some(FrameKind::AttachReady { attach_id: 7 })
        ));
        assert_eq!(
            frames
                .iter()
                .filter(|frame| matches!(frame, FrameKind::BootstrapReady { .. }))
                .count(),
            2
        );
        assert!(!first_interest.is_cancelled());
        assert!(!second_interest.is_cancelled());
        assert!(first.consumer_detach.try_recv().is_err());
        assert!(second.consumer_detach.try_recv().is_err());
        connection.cancel();
        assert!(first_interest.is_cancelled());
        assert!(second_interest.is_cancelled());
    })
    .await;
}
