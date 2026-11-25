//! Natural close racing an `ATTACH_RESOURCE` initial publication, through a
//! real terminal actor, the real exact source reservation and the real pump.

use super::*;
use crate::runtime::apps::StreamHost;
use crate::runtime::apps::terminal::capture::attach_resource::AttachResourceSession;
use phux_terminal::actor::PtyEvent;
use phux_terminal_api::actor::SnapshotRequest;

/// Printed after the initial cut was taken, before its publication finished.
const LATE: &[u8] = b"LATE_OUTPUT";

fn contains(payload: &[u8], needle: &[u8]) -> bool {
    payload.windows(needle.len()).any(|part| part == needle)
}

/// One consumer-visible bootstrap generation, in mailbox order.
struct Generation {
    bootstrap_id: BootstrapId,
    payload: Vec<u8>,
    ready: bool,
}

/// Everything one client saw through its `RESOURCE_CLOSED`.
struct Observed {
    generations: Vec<Generation>,
    closes: Vec<(Option<i32>, Option<i32>)>,
}

async fn observe_through_close(
    rx: &mut mpsc::Receiver<Outbound>,
    wire: &WireResourceId,
) -> Observed {
    let mut seen = Observed {
        generations: Vec::new(),
        closes: Vec::new(),
    };
    tokio::time::timeout(GUARD, async {
        loop {
            let Outbound::Frame(frame) = rx.recv().await.expect("mailbox remains alive") else {
                panic!("unexpected outbound")
            };
            match frame {
                FrameKind::BootstrapBegin { bootstrap_id, .. } => {
                    seen.generations.push(Generation {
                        bootstrap_id,
                        payload: Vec::new(),
                        ready: false,
                    });
                }
                FrameKind::BootstrapChunk { payload, .. } => seen
                    .generations
                    .last_mut()
                    .expect("chunk inside a generation")
                    .payload
                    .extend_from_slice(&payload),
                FrameKind::BootstrapReady { bootstrap_id, .. } => {
                    let current = seen
                        .generations
                        .last_mut()
                        .expect("READY inside a generation");
                    assert_eq!(current.bootstrap_id, bootstrap_id);
                    current.ready = true;
                }
                FrameKind::ResourceClosed {
                    terminal_id,
                    exit_status,
                    signal,
                    ..
                } => {
                    assert_eq!(&terminal_id, wire);
                    seen.closes.push((exit_status, signal));
                    return;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("natural close did not complete");
    seen
}

/// Forward every snapshot capture to the real actor. The first (the initial
/// publication's cut) answers only after the process printed [`LATE`], so
/// that output is strictly behind the initial cut and ahead of final capture.
fn forward_snapshots(
    real: mpsc::Sender<SnapshotRequest>,
    mut requests: mpsc::Receiver<SnapshotRequest>,
    pty: mpsc::Sender<PtyEvent>,
    mut observed: tokio::sync::broadcast::Receiver<ResourceOutput>,
) {
    tokio::task::spawn_local(async move {
        let mut first = true;
        while let Some(mut request) = requests.recv().await {
            let (reply, response) = oneshot::channel();
            let caller = std::mem::replace(&mut request.reply, reply);
            if real.send(request).await.is_err() {
                return;
            }
            let Ok(result) = response.await else { return };
            if std::mem::take(&mut first) {
                pty.send(PtyEvent::Bytes {
                    chunk: bytes::Bytes::from_static(LATE),
                    read_at: std::time::Instant::now(),
                })
                .await
                .expect("PTY event");
                loop {
                    let event = observed.recv().await.expect("actor output");
                    if matches!(&event, ResourceOutput::Live { bytes, .. } if bytes.as_ref() == LATE)
                    {
                        break;
                    }
                }
            }
            let _ = caller.send(result);
        }
    });
}

/// `emitter_before_close` selects whether the real raw emitter was admitted
/// before natural close began or is created by the source only afterwards.
async fn close_races_initial_publication(emitter_before_close: bool) {
    let state = SharedState::new();
    let mut actor = seeded_terminal(80, 24, FINAL_SCREEN);
    let (pty, _writer) = actor.actor.install_test_pty_channels();
    let engine = actor.handle.terminal().unwrap().clone();
    let handle = actor.handle.clone();
    let observed = actor.handle.output.subscribe();
    let (pane, wire) = state.with_mut(|s| {
        let (_, pane) = s.seed_session("close-publication");
        let wire = s
            .spawn_resource_actor(pane, actor.handle, actor.token.clone(), actor.actor.run())
            .unwrap();
        (pane, wire)
    });
    let (barrier, requests) = mpsc::channel(4);
    forward_snapshots(engine.snapshot.clone(), requests, pty, observed);
    let mut terminal = engine.clone();
    terminal.snapshot = barrier;
    let fixture = Fixture {
        state,
        pane,
        wire,
        terminal,
        output: handle.output.clone(),
    };

    let client = ClientId(1);
    let (tx, mut rx) = mpsc::channel(64);
    fixture
        .state
        .with_mut(|s| s.subscribe_resource(client, fixture.pane, Some(tx.clone())));
    let connection = CancellationToken::new();
    let reservation = StreamHost::new(&fixture.state, client)
        .reserve_next(fixture.pane, &connection)
        .expect("exact source reservation");
    let initial = reservation.lease.bootstrap;
    let session = AttachResourceSession {
        out_tx: &tx,
        terminal_id: &fixture.wire,
        handle,
        terminal: fixture.terminal.clone(),
        client_id: client,
        stream_id: StreamId::new(1).unwrap(),
        client_caps: ClientCapabilities::default(),
        stream_profile: BootstrapStreamProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    let mut establish = Box::pin(session.establish_generation(reservation));
    let admitted = if emitter_before_close {
        Some(establish.as_mut().await.expect("establish before close"))
    } else {
        None
    };

    let exit = ExitOutcome::exited(0);
    let reap = fixture.reap();
    assert!(
        fixture
            .state
            .with(|s| s.registry().resource(fixture.pane).is_none()),
        "close removed the registry entry before the source finished"
    );
    let close = announce_close(&fixture.state, reap, false);
    tokio::pin!(close);
    pending_once(close.as_mut()).await;

    let generation = match admitted {
        Some(generation) => generation,
        None => establish.await.expect("establish after close began"),
    };
    session
        .publish_generation(generation)
        .await
        .expect("initial publication survives registry removal");
    // Publication handed close ownership to the emitter, which has not run.
    pending_once(close.as_mut()).await;

    let (seen, _) = tokio::join!(observe_through_close(&mut rx, &fixture.wire), close);
    assert_eq!(
        seen.closes,
        vec![(exit.status, exit.signal)],
        "exactly the reaped exit, once"
    );
    let first = seen.generations.first().expect("initial publication");
    assert_eq!(first.bootstrap_id, initial);
    assert!(first.ready && contains(&first.payload, FINAL_SCREEN));
    assert!(
        !contains(&first.payload, LATE),
        "LATE is behind the initial cut"
    );
    assert!(
        seen.generations.len() >= 2,
        "the emitter published a final capture"
    );
    let last = seen.generations.last().unwrap();
    assert!(last.ready, "close overtook the final READY");
    assert!(
        contains(&last.payload, FINAL_SCREEN) && contains(&last.payload, LATE),
        "final output after the initial cut was lost"
    );
    assert!(!connection.is_cancelled(), "natural close is not a fault");
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err(), "terminal output followed close");
}

#[tokio::test(flavor = "current_thread")]
async fn natural_close_drains_an_emitter_admitted_before_initial_publication() {
    crate::runtime::test_support::local(Box::pin(close_races_initial_publication(true))).await;
}

#[tokio::test(flavor = "current_thread")]
async fn natural_close_drains_an_emitter_created_after_close_began() {
    crate::runtime::test_support::local(Box::pin(close_races_initial_publication(false))).await;
}
