//! Cancellation cuts through the actual `ATTACH_RESOURCE` generation future.

use super::*;
use crate::runtime::apps::terminal::test_support::{
    StubRx, state_sync_outcome, stub_terminal, terminal_state,
};
use futures_util::FutureExt;
use phux_protocol::caps::{BootstrapStreamProfile, OutputMode};
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_terminal_api::actor::TerminalResource as _;
use crate::state::SharedState;

struct Harness {
    state: SharedState,
    host: crate::runtime::apps::StreamHost,
    handle: ResourceHandle,
    core: phux_core::ids::ResourceId,
    wire: phux_protocol::ids::ResourceId,
    client: ClientId,
    connection: CancellationToken,
    outbound: tokio::sync::mpsc::Sender<Outbound>,
    _output: tokio::sync::mpsc::Receiver<Outbound>,
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
                _output: output,
            },
            rx,
        )
    }

    fn session(&self) -> AttachResourceSession<'_> {
        AttachResourceSession {
            host: &self.host,
            out_tx: &self.outbound,
            connection_token: &self.connection,
            terminal_id: &self.wire,
            core: self.core,
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
    let mut establish = Box::pin(session.establish_generation(bootstrap));
    assert!(establish.as_mut().now_or_never().is_none());
    drop(establish);
    drop(reservations);
    assert!(actor.consumer_attach.try_recv().is_err());
    assert!(actor.consumer_detach.try_recv().is_err(), "actor-owned cancellation needs no detach allocation");
    assert!(!harness.connection.is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_the_attach_future_with_an_unanswered_actor_retires_exact_interest() {
    let (harness, mut actor) = Harness::new();
    let session = harness.session();
    let bootstrap = BootstrapId::new(1).unwrap();
    let mut establish = Box::pin(session.establish_generation(bootstrap));
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
        let mut establish = Box::pin(session.establish_generation(bootstrap));
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
    let mut old = Box::pin(session.establish_generation(old_id));
    assert!(old.as_mut().now_or_never().is_none());
    actor
        .consumer_attach
        .try_recv()
        .unwrap()
        .reply
        .send(Ok(state_sync_outcome(b"old")))
        .unwrap();
    let old = old.await.unwrap();
    let mut new = Box::pin(session.establish_generation(new_id));
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
    let mut establish = Box::pin(session.establish_generation(BootstrapId::new(1).unwrap()));
    assert!(establish.as_mut().now_or_never().is_none());
    let request = actor.consumer_attach.try_recv().unwrap();
    let cancellation = request.cancellation.clone();
    let live = request.live_gate.clone();
    request.reply.send(Ok(state_sync_outcome(b"unpublished"))).unwrap();
    let generation = establish.await.unwrap();
    while harness.outbound.try_send(Outbound::Frame(FrameKind::ResourceClosed {
        terminal_id: harness.wire.clone(), exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Unknown, signal: None,
    })).is_ok() {}
    let mut publish = Box::pin(session.publish_generation(generation));
    assert!(publish.as_mut().now_or_never().is_none());
    assert!(!*live.borrow());
    harness.connection.cancel();
    assert!(publish.await.is_err());
    assert!(cancellation.is_cancelled());
    assert!(!*live.borrow());
    assert!(actor.consumer_detach.try_recv().is_err());
}
