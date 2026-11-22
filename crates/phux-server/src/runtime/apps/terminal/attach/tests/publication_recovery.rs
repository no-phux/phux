//! Full actor/pump/kernel convergence across every native activation caller.

use super::publication_client::Replica;
use super::publication_supersession::{context, resize_and_acknowledge, resize_to_and_acknowledge};
use super::*;
use phux_terminal::actor::{
    NativePublicationRequest, PtyEvent, TerminalActor, TerminalHandle, TerminalResource as _,
};

#[derive(Clone, Copy, Debug)]
enum Entry {
    Session,
    Spawn,
    Resource,
    Replacement,
    FinalDrain,
}

struct Client {
    id: ClientId,
    connection: CancellationToken,
    mailbox: tokio::sync::mpsc::Receiver<Outbound>,
    replica: Replica,
}

async fn ready_clients(clients: &mut [Client], terminal: &TerminalHandle, generation: BootstrapId) {
    for client in clients {
        client
            .replica
            .through_ready(&mut client.mailbox, generation)
            .await;
        assert!(client.replica.finish_history(terminal, client.id.0).await > 0);
    }
}

async fn activate_pair(
    terminal: &TerminalHandle,
    pair: [NativePublicationRequest; 2],
    superseded: bool,
) {
    for request in pair {
        forward(terminal, request, superseded).await;
    }
}

async fn recovered_clients(
    clients: &mut [Client],
    retired: BootstrapId,
    successor: BootstrapId,
    replay: u64,
) {
    for client in clients {
        client
            .replica
            .through_output(&mut client.mailbox, replay)
            .await;
        client.replica.drain(&mut client.mailbox);
        assert!(!client.connection.is_cancelled(), "connection survives");
        assert_recovery(&client.replica, retired, successor, replay);
    }
}

async fn live_clients(
    clients: &mut [Client],
    pty: &tokio::sync::mpsc::Sender<PtyEvent>,
    observed: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
    replay: u64,
) {
    let live = inject(pty, observed, b"successor-live\r\n").await;
    assert_eq!(live, replay + 1);
    for client in clients {
        client
            .replica
            .through_output(&mut client.mailbox, live)
            .await;
        client.replica.drain(&mut client.mailbox);
        let sequences = client.replica.output_sequences();
        assert_eq!(sequences, vec![replay, live], "replay then live, once each");
    }
}

impl Entry {
    fn held_generation(self) -> BootstrapId {
        match self {
            Self::Replacement | Self::FinalDrain => BootstrapId::new(2).unwrap(),
            _ => two_pump_initial_generation(),
        }
    }
}

async fn capture(ctx: &OutputPumpContext) -> phux_terminal::actor::NativeBootstrapReply {
    request_native_checkpoint(&ctx.terminal, |reply| {
        phux_terminal::actor::NativeBootstrapRequest::full_budget(
            ctx.client_id.0,
            ctx.wire_terminal_id.clone(),
            ctx.stream_id,
            ctx.initial_bootstrap_id,
            ctx.limits,
            reply,
        )
    })
    .await
    .expect("real capture")
}

/// Activate through the real actor. The barrier changes only scheduling.
async fn forward(
    terminal: &TerminalHandle,
    mut request: NativePublicationRequest,
    superseded: bool,
) {
    let (reply, response) = oneshot::channel();
    let pump_reply = std::mem::replace(&mut request.reply, reply);
    terminal
        .native_publication
        .send(request)
        .await
        .expect("actor activation");
    let publication = response.await.expect("actor reply");
    if superseded {
        assert_eq!(
            publication.as_ref().err(),
            Some(&phux_terminal_api::native_state::NativeStateError::Resize)
        );
    } else {
        assert!(publication.is_ok(), "successor actually activated");
        assert!(
            !publication.as_ref().unwrap().replay.is_empty(),
            "nonzero fenced replay"
        );
    }
    pump_reply.send(publication).expect("pump waiting");
}

async fn inject(
    pty: &tokio::sync::mpsc::Sender<PtyEvent>,
    observed: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
    text: &'static [u8],
) -> u64 {
    pty.send(PtyEvent::Bytes {
        chunk: bytes::Bytes::from_static(text),
        read_at: std::time::Instant::now(),
    })
    .await
    .expect("PTY event");
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), observed.recv())
            .await
            .expect("actor output deadline")
            .expect("actor output");
        if let ResourceOutput::Live { seq, bytes, .. } = event {
            assert_eq!(bytes.as_ref(), text);
            assert!(seq > 0);
            return seq;
        }
    }
}

async fn held_pair(
    activations: &mut tokio::sync::mpsc::Receiver<NativePublicationRequest>,
    generation: BootstrapId,
) -> [NativePublicationRequest; 2] {
    let mut pair = Vec::new();
    for _ in 0..2 {
        let request = tokio::time::timeout(std::time::Duration::from_secs(10), activations.recv())
            .await
            .expect("activation deadline")
            .expect("activation barrier");
        assert_eq!(request.bootstrap_id, generation);
        pair.push(request);
    }
    pair.try_into().unwrap_or_else(|_| panic!("two pumps"))
}

async fn resize_resync(observed: &mut tokio::sync::broadcast::Receiver<ResourceOutput>) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if matches!(
                observed.recv().await.expect("actor resync"),
                ResourceOutput::Resync {
                    reason: crate::resource::ResyncReason::Resize,
                    ..
                }
            ) {
                return;
            }
        }
    })
    .await
    .expect("real actor resize resync deadline");
}

async fn overwrite_resize_storm(
    terminal: &TerminalHandle,
    pty: &tokio::sync::mpsc::Sender<PtyEvent>,
    observed: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
) {
    resize_resync(observed).await;
    for cols in [29, 31, 30] {
        resize_to_and_acknowledge(terminal, cols, 8).await;
        resize_resync(observed).await;
    }
    // The real actor overwrites the held pumps' broadcast windows, including
    // every original resize resync. Recovery must use the existing gap path.
    for _ in 0..=phux_resource::DEFAULT_OUTPUT_BROADCAST {
        inject(pty, observed, b"fenced-storm-output\r\n").await;
    }
}

async fn trigger_replacement(
    terminal: &TerminalHandle,
    initially_active: &mut tokio::sync::mpsc::Receiver<()>,
) {
    for _ in 0..2 {
        initially_active.recv().await.expect("initial active");
    }
    terminal
        .resize
        .send(ResizeRequest {
            cols: 20,
            rows: 5,
            cell_px: None,
            resync_clients: true,
            resync_only: true,
            resync_for: None,
        })
        .await
        .expect("real replacement trigger");
}

async fn session_entry(
    ctx: OutputPumpContext,
    state: &SharedState,
    connection: &CancellationToken,
    core: ResourceId,
    output: tokio::sync::broadcast::Receiver<ResourceOutput>,
) {
    let reply = capture(&ctx).await;
    let (cut, cursor) = publish_native_bootstrap(&ctx.out_tx, reply)
        .await
        .expect("queue capture");
    let (gate, receive) = oneshot::channel();
    let client_id = ctx.client_id;
    let stream_id = ctx.stream_id;
    let bootstrap_id = ctx.initial_bootstrap_id;
    let snapshot_gate = SnapshotGate {
        terminal_id: core,
        wire_terminal_id: ctx.wire_terminal_id.clone(),
        terminal: ctx.terminal.clone(),
        gate,
        cut: Some(cut),
        native_cursor: Some(cursor),
    };
    let state_for_pump = state.clone();
    let connection_for_pump = connection.clone();
    let pump = tokio::task::spawn_local(async move {
        if let Some(fault) = run_output_pump(&ctx, receive, output).await {
            release_after_pump_fault(fault, &state_for_pump, client_id, &connection_for_pump);
        }
    });
    release_gates(
        state,
        connection,
        client_id,
        stream_id,
        bootstrap_id,
        vec![snapshot_gate],
    )
    .await;
    pump.await.expect("session pump");
}

async fn replacement_entry(
    mut ctx: OutputPumpContext,
    real: &TerminalHandle,
    output: tokio::sync::broadcast::Receiver<ResourceOutput>,
    final_drain: bool,
    initial_active: tokio::sync::mpsc::Sender<()>,
) {
    let barrier = ctx.terminal.native_publication.clone();
    ctx.terminal.native_publication = real.native_publication.clone();
    let reply = capture(&ctx).await;
    let (cut, cursor) = publish_native_bootstrap(&ctx.out_tx, reply)
        .await
        .expect("initial READY");
    let publication = activate_native_publication(
        real,
        ctx.client_id.0,
        ctx.wire_terminal_id.clone(),
        ctx.stream_id,
        ctx.initial_bootstrap_id,
        cursor,
    )
    .await
    .expect("initial activation");
    let mut start = OutputPumpStart {
        superseded: false,
        published_cut: cut,
        replay: Vec::new(),
        live: None,
    };
    start.adopt_native_publication(publication);
    ctx.terminal.native_publication = barrier;
    if final_drain {
        let drain = CancellationToken::new();
        drain.cancel();
        ctx.drain = Some(drain);
    }
    initial_active
        .send(())
        .await
        .expect("initial activation acknowledgement");
    assert!(run_started_output_pump(&ctx, start, output).await.is_none());
}

#[allow(
    clippy::too_many_arguments,
    reason = "test entry points share one real actor and independent pump ownership"
)]
async fn run_entry(
    entry: Entry,
    ctx: OutputPumpContext,
    real: TerminalHandle,
    handle: ResourceHandle,
    state: SharedState,
    core: ResourceId,
    connection: CancellationToken,
    stop: CancellationToken,
    initial_active: tokio::sync::mpsc::Sender<()>,
) {
    let output = handle.output.subscribe();
    match entry {
        Entry::Session => session_entry(ctx, &state, &connection, core, output).await,
        Entry::Replacement | Entry::FinalDrain => {
            replacement_entry(
                ctx,
                &real,
                output,
                matches!(entry, Entry::FinalDrain),
                initial_active,
            )
            .await;
        }
        Entry::Spawn => {
            let mut pumps = JoinSet::new();
            SpawnPublication {
                state: &state,
                out_tx: &ctx.out_tx,
                request_id: 1,
                client_id: ctx.client_id,
                core_terminal_id: core,
                wire_terminal_id: ctx.wire_terminal_id,
                handle,
                terminal: ctx.terminal,
                client_caps: ctx.client_caps,
                stream_id: ctx.stream_id,
                profile: ctx.profile,
                limits: ctx.limits,
                bind_instance: false,
                idempotency_key: None,
            }
            .publish(&mut pumps, &connection, false)
            .await;
            stop.cancelled().await;
            pumps.shutdown().await;
        }
        Entry::Resource => {
            let session =
                crate::runtime::apps::terminal::commands::attach_resource::AttachResourceSession {
                    state: &state,
                    out_tx: &ctx.out_tx,
                    connection_token: &connection,
                    terminal_id: &ctx.wire_terminal_id,
                    core,
                    handle,
                    terminal: ctx.terminal,
                    client_id: ctx.client_id,
                    stream_id: ctx.stream_id,
                    client_caps: ctx.client_caps,
                    stream_profile: ctx.profile,
                    bootstrap_limits: ctx.limits,
                };
            let mut generation = session
                .establish_generation(ctx.initial_bootstrap_id)
                .await
                .expect("establish ATTACH_RESOURCE");
            session
                .finish_bootstrap(&mut generation)
                .await
                .expect("finish ATTACH_RESOURCE");
            stop.cancelled().await;
            pump::stop_output(&state, ctx.client_id, core).await;
        }
    }
}

fn assert_recovery(
    replica: &Replica,
    retired: BootstrapId,
    successor: BootstrapId,
    replay_seq: u64,
) {
    let terminal_id = phux_protocol::ids::ResourceId::local(1);
    let published = replica
        .kernel
        .published(&terminal_id)
        .expect("real successor replica");
    assert_eq!(published.key().bootstrap_id, successor);
    assert_eq!(
        (published.geometry().cols, published.geometry().rows),
        (30, 8)
    );
    assert_eq!(published.last_seq(), replay_seq);
    let frames = &replica.frames;
    let ready = frames.iter().position(|f| matches!(f, FrameKind::BootstrapReady { bootstrap_id, .. } if *bootstrap_id == retired)).expect("old READY");
    let tombstones: Vec<_> = frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| match f {
            FrameKind::BootstrapTombstone {
                terminal_id: id,
                stream_id,
                bootstrap_id,
                last_valid_seq,
                ..
            } if *bootstrap_id == retired => {
                assert_eq!(id, &terminal_id);
                assert_eq!(*stream_id, two_pump_stream());
                Some((i, *last_valid_seq))
            }
            _ => None,
        })
        .collect();
    assert_eq!(tombstones.len(), 1, "one addressed retirement");
    let cut = frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::BootstrapBegin {
                bootstrap_id,
                base_seq,
                ..
            } if *bootstrap_id == retired => Some(*base_seq),
            _ => None,
        })
        .expect("retired checkpoint cut");
    assert_eq!(
        tombstones[0].1, cut,
        "unactivated READY retires at its checkpoint cut"
    );
    assert!(ready < tombstones[0].0);
    let begin = frames.iter().position(|f| matches!(f, FrameKind::BootstrapBegin { bootstrap_id, .. } if *bootstrap_id == successor)).expect("new BEGIN");
    let new_ready = frames.iter().position(|f| matches!(f, FrameKind::BootstrapReady { bootstrap_id, .. } if *bootstrap_id == successor)).expect("new READY");
    assert!(tombstones[0].0 < begin && begin < new_ready);
    assert!(
        frames[begin + 1..new_ready]
            .iter()
            .any(|f| matches!(f, FrameKind::BootstrapChunk { .. }))
    );
    assert!(frames.iter().all(|f| !matches!(f, FrameKind::ResourceOutput { bootstrap_id, .. } if *bootstrap_id == retired)), "no unfenced stale live output");
    let outputs = replica.output_sequences();
    assert_eq!(outputs, vec![replay_seq], "nonzero replay exactly once");
}

async fn exercise_ready_race(
    entry: Entry,
    real: &TerminalHandle,
    pty: &tokio::sync::mpsc::Sender<PtyEvent>,
    observed: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
    activations: &mut tokio::sync::mpsc::Receiver<NativePublicationRequest>,
    clients: &mut [Client],
    initially_active: &mut tokio::sync::mpsc::Receiver<()>,
) {
    let retired = entry.held_generation();
    if matches!(entry, Entry::Replacement) {
        trigger_replacement(real, initially_active).await;
    }
    let held = held_pair(activations, retired).await;
    ready_clients(clients, real, retired).await;
    inject(pty, observed, b"after-READY-before-resize\r\n").await;
    resize_and_acknowledge(real).await;
    if matches!(entry, Entry::Replacement) {
        overwrite_resize_storm(real, pty, observed).await;
    }
    activate_pair(real, held, true).await;
    let successor = next_bootstrap_id(retired);
    let held = held_pair(activations, successor).await;
    ready_clients(clients, real, successor).await;
    let replay = inject(pty, observed, b"successor-replay\r\n").await;
    activate_pair(real, held, false).await;
    recovered_clients(clients, retired, successor, replay).await;
    if !matches!(entry, Entry::FinalDrain) {
        live_clients(clients, pty, observed, replay).await;
    }
}

async fn recover(entry: Entry) {
    let mut bundle = TerminalActor::new(20, 5).expect("actor");
    let (pty, _writer) = bundle.actor.install_test_pty_channels();
    let real = bundle.handle.terminal().expect("terminal").clone();
    let token = bundle.token.clone();
    let mut observed = bundle.handle.output.subscribe();
    let (barrier, mut activations) = tokio::sync::mpsc::channel(8);
    let mut terminal = real.clone();
    terminal.native_publication = barrier;
    let handle = terminal
        .clone()
        .into_resource(None, bundle.handle.output.clone());
    let state = SharedState::new();
    let core = state.with_mut(|s| {
        let (_, core) = s.seed_session("supersession");
        s.register_resource_handle(core, handle.clone(), token.clone())
            .unwrap();
        assert_eq!(
            s.idspace.intern_resource(core),
            phux_protocol::ids::ResourceId::local(1)
        );
        core
    });
    let actor = tokio::task::spawn_local(bundle.actor.run());
    assert_eq!(inject(&pty, &mut observed, b"before-capture\r\n").await, 1);
    let stop = CancellationToken::new();
    let mut clients = Vec::new();
    let mut tasks = Vec::new();
    let (initial_active, mut initially_active) = tokio::sync::mpsc::channel(2);
    for _ in 0..2 {
        let client_id = state.with_mut(|s| s.clients_mut().new_client_id());
        let connection = CancellationToken::new();
        let (mut ctx, mailbox) = context(terminal.clone(), client_id);
        ctx.cancel = Some(stop.clone());
        tasks.push(tokio::task::spawn_local(run_entry(
            entry,
            ctx,
            real.clone(),
            handle.clone(),
            state.clone(),
            core,
            connection.clone(),
            stop.clone(),
            initial_active.clone(),
        )));
        clients.push(Client {
            id: client_id,
            connection,
            mailbox,
            replica: Replica::new(),
        });
    }
    exercise_ready_race(
        entry,
        &real,
        &pty,
        &mut observed,
        &mut activations,
        &mut clients,
        &mut initially_active,
    )
    .await;
    stop.cancel();
    for task in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("pump terminates")
            .expect("pump");
    }
    token.cancel();
    actor.await.expect("actor shutdown");
}

#[tokio::test(flavor = "current_thread")]
async fn session_recovers_to_an_activated_replica() {
    crate::runtime::test_support::local(recover(Entry::Session)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_recovers_to_an_activated_replica_without_reaping() {
    crate::runtime::test_support::local(recover(Entry::Spawn)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn attach_resource_recovers_to_an_activated_replica() {
    crate::runtime::test_support::local(recover(Entry::Resource)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn replacement_recovers_after_a_resize_storm_overwrites_every_original_resync() {
    crate::runtime::test_support::local(recover(Entry::Replacement)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn final_drain_waits_for_an_actually_activated_successor() {
    crate::runtime::test_support::local(recover(Entry::FinalDrain)).await;
}
