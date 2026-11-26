//! Cancellation cuts through the actual `ATTACH_RESOURCE` generation future.

use super::*;
use crate::runtime::apps::terminal::test_support::{
    StubRx, state_sync_outcome, stub_terminal, terminal_state,
};
use crate::state::SharedState;
use futures_util::FutureExt;
use phux_protocol::caps::{BootstrapStreamProfile, OutputMode};
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_terminal_api::actor::TerminalResource as _;

struct Harness {
    state: SharedState,
    host: crate::runtime::apps::StreamHost,
    handle: ResourceHandle,
    core: phux_core::ids::ResourceId,
    wire: phux_protocol::ids::ResourceId,
    client: ClientId,
    connection: CancellationToken,
    outbound: tokio::sync::mpsc::Sender<Outbound>,
    output: tokio::sync::mpsc::Receiver<Outbound>,
}

impl Harness {
    fn new() -> (Self, StubRx) {
        let state = terminal_state();
        let (_, core) = state.with_mut(|s| s.seed_session("registration"));
        let (handle, rx) = stub_terminal();
        let (client, wire) = state.with_mut(|s| {
            s.register_resource_handle(core, handle.clone(), CancellationToken::new())
                .unwrap();
            (
                s.clients_mut().new_client_id(),
                s.idspace.intern_resource(core),
            )
        });
        let (outbound, output) = tokio::sync::mpsc::channel(32);
        (
            Self {
                host: crate::runtime::apps::StreamHost::new(&state, client),
                state,
                handle,
                core,
                wire,
                client,
                connection: CancellationToken::new(),
                outbound,
                output,
            },
            rx,
        )
    }

    fn reserve(&self, bootstrap: BootstrapId) -> crate::runtime::apps::StreamReservation {
        self.host
            .reserve(self.core, bootstrap, &self.connection)
            .unwrap()
    }

    fn session(&self) -> AttachResourceSession<'_> {
        AttachResourceSession {
            out_tx: &self.outbound,
            terminal_id: &self.wire,
            handle: self.handle.clone(),
            terminal: self.handle.terminal().unwrap().clone(),
            client_id: self.client,
            stream_id: StreamId::new(7).unwrap(),
            client_caps: ClientCapabilities::default().with_output_mode(OutputMode::StateSync),
            stream_profile: BootstrapStreamProfile::SynthesizedVtStateSync,
            bootstrap_limits: BootstrapLimits::default(),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_before_mailbox_admission_sends_no_registration() {
    let (harness, mut actor) = Harness::new();
    let session = harness.session();
    let bootstrap = BootstrapId::new(1).unwrap();
    let mut reservations = Vec::new();
    for _ in 0..session.terminal.consumer_attach.capacity() {
        reservations.push(
            session
                .terminal
                .consumer_attach
                .clone()
                .reserve_owned()
                .await
                .unwrap(),
        );
    }
    let mut establish = Box::pin(session.establish_generation(harness.reserve(bootstrap)));
    assert!(establish.as_mut().now_or_never().is_none());
    drop(establish);
    drop(reservations);
    assert!(actor.consumer_attach.try_recv().is_err());
    assert!(
        actor.consumer_detach.try_recv().is_err(),
        "actor-owned cancellation needs no detach allocation"
    );
    assert!(!harness.connection.is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_the_attach_future_with_an_unanswered_actor_retires_exact_interest() {
    let (harness, mut actor) = Harness::new();
    let session = harness.session();
    let bootstrap = BootstrapId::new(1).unwrap();
    let mut establish = Box::pin(session.establish_generation(harness.reserve(bootstrap)));
    assert!(establish.as_mut().now_or_never().is_none());
    let request = actor.consumer_attach.try_recv().unwrap();
    assert!(!request.cancellation.is_cancelled());
    drop(establish);
    assert!(request.cancellation.is_cancelled());
    assert!(request.reply.send(Ok(state_sync_outcome(b"late"))).is_err());
    assert!(actor.consumer_detach.try_recv().is_err());
    assert!(!harness.connection.is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn unread_success_and_unpublished_generation_both_remain_drop_owned() {
    for receive_reply in [false, true] {
        let (harness, mut actor) = Harness::new();
        let session = harness.session();
        let bootstrap = BootstrapId::new(1).unwrap();
        let mut establish = Box::pin(session.establish_generation(harness.reserve(bootstrap)));
        assert!(establish.as_mut().now_or_never().is_none());
        let request = actor.consumer_attach.try_recv().unwrap();
        let cancellation = request.cancellation.clone();
        request
            .reply
            .send(Ok(state_sync_outcome(b"installed")))
            .unwrap();
        if receive_reply {
            let generation = establish.await.unwrap();
            assert!(!cancellation.is_cancelled());
            drop(generation);
        } else {
            drop(establish);
        }
        assert!(cancellation.is_cancelled());
        assert!(actor.consumer_detach.try_recv().is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_an_old_bootstrap_cannot_cancel_its_replacement_generation() {
    let (harness, mut actor) = Harness::new();
    let session = harness.session();
    let old_id = BootstrapId::new(1).unwrap();
    let new_id = BootstrapId::new(2).unwrap();
    let mut old = Box::pin(session.establish_generation(harness.reserve(old_id)));
    assert!(old.as_mut().now_or_never().is_none());
    actor
        .consumer_attach
        .try_recv()
        .unwrap()
        .reply
        .send(Ok(state_sync_outcome(b"old")))
        .unwrap();
    let old = old.await.unwrap();
    let mut new = Box::pin(session.establish_generation(harness.reserve(new_id)));
    assert!(new.as_mut().now_or_never().is_none());
    let replacement = actor.consumer_attach.try_recv().unwrap();
    let cancellation = replacement.cancellation.clone();
    replacement
        .reply
        .send(Ok(state_sync_outcome(b"new")))
        .unwrap();
    let new = new.await.unwrap();
    drop(old);
    assert!(actor.consumer_detach.try_recv().is_err());
    assert!(!cancellation.is_cancelled());
    session.publish_generation(new).await.unwrap();
    assert!(
        !cancellation.is_cancelled(),
        "successful publication transfers interest to the pump generation"
    );
    harness
        .state
        .with_mut(|s| s.resources_mut().cancel_pump(harness.client, harness.core));
    assert!(cancellation.is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_a_full_publication_mailbox_never_opens_the_live_gate() {
    let (harness, mut actor) = Harness::new();
    let session = harness.session();
    let mut establish =
        Box::pin(session.establish_generation(harness.reserve(BootstrapId::new(1).unwrap())));
    assert!(establish.as_mut().now_or_never().is_none());
    let request = actor.consumer_attach.try_recv().unwrap();
    let cancellation = request.cancellation.clone();
    let live = request.live_gate.clone();
    request
        .reply
        .send(Ok(state_sync_outcome(b"unpublished")))
        .unwrap();
    let generation = establish.await.unwrap();
    while harness
        .outbound
        .try_send(Outbound::Frame(FrameKind::ResourceClosed {
            terminal_id: harness.wire.clone(),
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .is_ok()
    {}
    let mut publish = Box::pin(session.publish_generation(generation));
    assert!(publish.as_mut().now_or_never().is_none());
    assert!(!*live.borrow());
    harness.connection.cancel();
    assert!(publish.await.is_err());
    assert!(cancellation.is_cancelled());
    assert!(!*live.borrow());
    assert!(actor.consumer_detach.try_recv().is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn suspended_establish_cannot_retire_or_reap_a_newer_published_stream() {
    crate::runtime::test_support::local(async {
        let (mut harness, mut actor) = Harness::new();
        let session = harness.session();
        let predecessor = harness.reserve(BootstrapId::new(1).unwrap());
        let old_scope = predecessor.lease.clone();
        let (started, running) = oneshot::channel();
        assert!(old_scope.spawn(None, None, async move {
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        }));
        assert!(predecessor.published(|| true));
        running.await.unwrap();

        // Both real establish futures must retain the actual emitter's fence.
        // No runtime turn occurs between aborting it and polling these sources.
        let mut older = Box::pin(session.establish_generation(harness.reserve(BootstrapId::new(2).unwrap())));
        assert!(older.as_mut().now_or_never().is_none());
        let newest = harness.reserve(BootstrapId::new(3).unwrap());
        let newest_scope = newest.lease.clone();
        let mut newer = Box::pin(session.establish_generation(newest));
        assert!(newer.as_mut().now_or_never().is_none());
        assert!(actor.consumer_attach.try_recv().is_err());
        assert!(older.await.is_err());
        assert!(!newest_scope.cancellation.is_cancelled());
        tokio::task::yield_now().await;
        assert!(newer.as_mut().now_or_never().is_none());
        let request = actor.consumer_attach.try_recv().unwrap();
        let interest = request.cancellation.clone();
        request.reply.send(Ok(state_sync_outcome(b"newest"))).unwrap();
        session.publish_generation(newer.await.unwrap()).await.unwrap();

        // Delayed faults and creation withdrawal all carry the original fence.
        old_scope.detach(&harness.wire).await;
        old_scope.retire_terminal_fault(crate::runtime::apps::terminal::attach::PumpFault::GenerationLost);
        old_scope.retire_spawn_fault(crate::runtime::apps::terminal::attach::PumpFault::GenerationLost);
        old_scope.reap_spawn(&harness.wire, None);
        assert!(!interest.is_cancelled());
        assert!(!harness.connection.is_cancelled());
        assert!(harness.state.with(|s| s.registry().resource(harness.core).is_some()));
        assert!(actor.consumer_detach.try_recv().is_err());
        let mut ready = false;
        while let Ok(Outbound::Frame(frame)) = harness.output.try_recv() {
            ready |= matches!(frame, FrameKind::BootstrapReady { bootstrap_id, .. } if bootstrap_id == BootstrapId::new(3).unwrap());
        }
        assert!(ready, "the surviving real publication reaches READY");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_full_old_publication_cannot_open_or_withdraw_the_replacement_gate() {
    let (mut harness, mut actor) = Harness::new();
    let session = harness.session();
    let mut establish =
        Box::pin(session.establish_generation(harness.reserve(BootstrapId::new(1).unwrap())));
    assert!(establish.as_mut().now_or_never().is_none());
    let old = actor.consumer_attach.try_recv().unwrap();
    let old_gate = old.live_gate.clone();
    old.reply.send(Ok(state_sync_outcome(b"old"))).unwrap();
    let generation = establish.await.unwrap();
    while harness
        .outbound
        .try_send(Outbound::Frame(FrameKind::ResourceClosed {
            terminal_id: harness.wire.clone(),
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .is_ok()
    {}
    let mut publish = Box::pin(session.publish_generation(generation));
    assert!(publish.as_mut().now_or_never().is_none());
    let reservation = harness.reserve(BootstrapId::new(2).unwrap());
    assert!(publish.await.is_err());
    assert!(!*old_gate.borrow());
    while harness.output.try_recv().is_ok() {}
    let session = harness.session();
    let mut replacement = Box::pin(session.establish_generation(reservation));
    assert!(replacement.as_mut().now_or_never().is_none());
    let current = actor.consumer_attach.try_recv().unwrap();
    let live = current.live_gate.clone();
    let interest = current.cancellation.clone();
    current
        .reply
        .send(Ok(state_sync_outcome(b"current")))
        .unwrap();
    session
        .publish_generation(replacement.await.unwrap())
        .await
        .unwrap();
    assert!(*live.borrow());
    assert!(!interest.is_cancelled());
    assert!(!harness.connection.is_cancelled());
    assert!(actor.consumer_detach.try_recv().is_err());
}
