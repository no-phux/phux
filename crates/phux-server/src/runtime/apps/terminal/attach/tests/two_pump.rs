//! Two pumps sharing one stream: a stale consumer resyncs alone.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// The stream every pump in the two-pump tests publishes on.
pub(super) fn two_pump_stream() -> StreamId {
    StreamId::new(1).expect("stream id")
}

/// The generation each test pump opens with.
pub(super) fn two_pump_initial_generation() -> BootstrapId {
    BootstrapId::new(1).expect("bootstrap id")
}

/// A running ATTACH output pump on a shared pane broadcast, with its
/// consumer's mailbox exposed.
pub(super) struct TwoPumpConsumer {
    pub(super) out_rx: tokio::sync::mpsc::Receiver<Outbound>,
    pub(super) task: tokio::task::JoinHandle<Option<PumpFault>>,
}

/// Start one synthesized-profile ATTACH pump for `client` whose published
/// bootstrap covers everything up to `published_cut`.
pub(super) fn spawn_two_pump_consumer(
    client: u64,
    published_cut: u64,
    mailbox: usize,
    output: &tokio::sync::broadcast::Sender<ResourceOutput>,
    resize: &ResizeSender,
) -> TwoPumpConsumer {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(mailbox);
    // This fixture supplies text resyncs directly, without actor capture.
    let mut client_caps = ClientCapabilities::default();
    client_caps.image_protocols = phux_protocol::caps::ImageProtocolSet::new();
    let ctx = OutputPumpContext {
        out_tx,
        resize: resize.clone(),
        wire_terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: two_pump_stream(),
        initial_bootstrap_id: two_pump_initial_generation(),
        client_id: ClientId(client),
        client_caps,
        profile: BootstrapStreamProfile::SynthesizedVtRaw,
        limits: BootstrapLimits::default(),
        lag_label: "two-pump test pump",
        stale_skip: true,
        cancel: None,
        drain: None,
        snapshot: None,
        last_seq: None,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        terminal: native_attach_handle()
            .0
            .terminal()
            .expect("terminal facet")
            .clone(),
    };
    let (gate_tx, gate_rx) = oneshot::channel();
    gate_tx
        .send(OutputPumpStart {
            published_cut,
            replay: Vec::new(),
            live: None,
        })
        .unwrap_or_else(|_| panic!("gate receiver alive"));
    let live = output.subscribe();
    let task = tokio::task::spawn_local(async move { run_output_pump(&ctx, gate_rx, live).await });
    TwoPumpConsumer { out_rx, task }
}

/// What one consumer's mirror saw, reduced to what these tests judge.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Seen {
    Output {
        generation: BootstrapId,
        seq: u64,
    },
    Begin {
        generation: BootstrapId,
        base_seq: u64,
    },
    Chunk,
    Ready {
        generation: BootstrapId,
    },
    Tombstone,
    Other(String),
}

impl Seen {
    fn of(outbound: Outbound) -> Self {
        let Outbound::Frame(frame) = outbound else {
            return Self::Other(format!("{outbound:?}"));
        };
        match frame {
            FrameKind::ResourceOutput {
                bootstrap_id, seq, ..
            } => Self::Output {
                generation: bootstrap_id,
                seq,
            },
            FrameKind::BootstrapBegin {
                bootstrap_id,
                base_seq,
                ..
            } => Self::Begin {
                generation: bootstrap_id,
                base_seq,
            },
            FrameKind::BootstrapChunk { .. } => Self::Chunk,
            FrameKind::BootstrapReady { bootstrap_id, .. } => Self::Ready {
                generation: bootstrap_id,
            },
            FrameKind::BootstrapTombstone { .. } => Self::Tombstone,
            other => Self::Other(format!("{other:?}")),
        }
    }
}

/// Generous: only a broken pump ever hits it.
const TWO_PUMP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Drain `consumer` until it has seen `count` frames.
pub(super) async fn frames_seen(consumer: &mut TwoPumpConsumer, count: usize) -> Vec<Seen> {
    let mut seen = Vec::with_capacity(count);
    while seen.len() < count {
        let outbound = tokio::time::timeout(TWO_PUMP_DEADLINE, consumer.out_rx.recv())
            .await
            .unwrap_or_else(|_| panic!("pump went quiet after {seen:?}"))
            .expect("pump mailbox closed");
        seen.push(Seen::of(outbound));
    }
    seen
}

/// Let every pump task on the `LocalSet` run until it parks again.
pub(super) async fn let_pumps_run() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// The consumer has nothing further queued: no late tombstone or
/// republish is sitting in its mailbox.
pub(super) async fn assert_quiet(consumer: &mut TwoPumpConsumer, who: &str) {
    let_pumps_run().await;
    if let Ok(outbound) = consumer.out_rx.try_recv() {
        panic!("{who} saw an unexpected frame: {:?}", Seen::of(outbound));
    }
}

/// Wait for the gap resync request the pump behind `laggard` sends; on
/// timeout, report what the laggard was sent and whether its pump died.
pub(super) async fn resync_request_from(
    resize_rx: &mut phux_terminal::actor::ResizeReceiver,
    laggard: &mut TwoPumpConsumer,
) -> ResizeRequest {
    let_pumps_run().await;
    if let Ok(received) = tokio::time::timeout(TWO_PUMP_DEADLINE, resize_rx.recv()).await {
        return received.expect("resize mailbox closed");
    }
    let mut sent = Vec::new();
    while let Ok(outbound) = laggard.out_rx.try_recv() {
        sent.push(Seen::of(outbound));
    }
    panic!(
        "no resync request reached the actor; the laggard was sent {sent:?} \
         (pump task finished: {})",
        laggard.task.is_finished(),
    );
}

/// Answer a gap resync the way the actor does: one broadcast `Resync`,
/// addressed to exactly the pump the request named.
pub(super) fn answer_gap_resync(
    output: &tokio::sync::broadcast::Sender<ResourceOutput>,
    request: &ResizeRequest,
    base_seq: u64,
) {
    let audience = request.resync_for.map_or(ResyncAudience::Everyone, |pump| {
        ResyncAudience::Only(vec![pump].into())
    });
    output
        .send(ResourceOutput::Resync {
            cols: 80,
            rows: 24,
            reason: crate::resource::ResyncReason::OutboundGap,
            audience,
            base_seq,
            bytes: bytes::Bytes::from_static(b"snapshot"),
        })
        .expect("pumps subscribed");
}

pub(super) fn live(seq: u64, at: std::time::Instant) -> ResourceOutput {
    ResourceOutput::Live {
        seq,
        bytes: bytes::Bytes::from_static(b"x"),
        at,
    }
}

/// One consumer going stale re-bootstraps that consumer and nobody else
/// on the pane; a later reflow is still owed to both.
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: stale, targeted recovery, then a reflow"
)]
async fn a_stale_consumer_resyncs_without_republishing_its_neighbour() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (output, _seed) = tokio::sync::broadcast::channel::<ResourceOutput>(64);
            let (resize_tx, mut resize_rx) = ResizeSender::channel(8);
            let mut fresh = spawn_two_pump_consumer(1, 2, 64, &output, &resize_tx);
            let mut stale = spawn_two_pump_consumer(2, 1, 64, &output, &resize_tx);
            let initial = two_pump_initial_generation();
            let replacement = next_bootstrap_id(initial);

            // Staleness counts from the later of the read and the
            // generation's publication, so open the pumps and outlive the
            // budget before sending a backdated chunk.
            let_pumps_run().await;
            tokio::time::sleep(
                crate::runtime::pump::STALE_OUTPUT_BUDGET + std::time::Duration::from_millis(50),
            )
            .await;
            let read_a_second_ago = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .expect("monotonic clock has run for a second");
            output
                .send(live(2, read_a_second_ago))
                .expect("pumps subscribed");

            let request = resync_request_from(&mut resize_rx, &mut stale).await;
            assert!(request.resync_only && request.resync_clients);
            assert_eq!(
                request.resync_for,
                Some(ResyncTarget {
                    owner: 2,
                    stream_id: two_pump_stream(),
                    bootstrap_id: initial,
                }),
                "the resync names the stale pump, not the pane",
            );

            let now = std::time::Instant::now();
            output.send(live(3, now)).expect("pumps subscribed");
            answer_gap_resync(&output, &request, 3);
            output.send(live(4, now)).expect("pumps subscribed");

            assert_eq!(
                frames_seen(&mut fresh, 2).await,
                vec![
                    Seen::Output {
                        generation: initial,
                        seq: 3
                    },
                    Seen::Output {
                        generation: initial,
                        seq: 4
                    },
                ],
                "the fresh consumer keeps its generation straight through",
            );
            assert_eq!(
                frames_seen(&mut stale, 5).await,
                vec![
                    Seen::Tombstone,
                    Seen::Begin {
                        generation: replacement,
                        base_seq: 3
                    },
                    Seen::Chunk,
                    Seen::Ready {
                        generation: replacement
                    },
                    Seen::Output {
                        generation: replacement,
                        seq: 4
                    },
                ],
                "the stale consumer converges onto a fresh generation",
            );
            assert_quiet(&mut fresh, "the fresh consumer").await;
            assert_quiet(&mut stale, "the stale consumer").await;
            assert!(
                resize_rx.try_recv().is_err(),
                "nobody else asked the actor for a resync",
            );

            // A reflow changes the grid under everyone: still broadcast.
            output
                .send(ResourceOutput::Resync {
                    cols: 100,
                    rows: 30,
                    reason: crate::resource::ResyncReason::Resize,
                    audience: ResyncAudience::Everyone,
                    base_seq: 4,
                    bytes: bytes::Bytes::from_static(b"reflowed"),
                })
                .expect("pumps subscribed");
            assert_eq!(
                frames_seen(&mut fresh, 2).await,
                vec![
                    Seen::Tombstone,
                    Seen::Begin {
                        generation: replacement,
                        base_seq: 4
                    }
                ],
            );
            assert_eq!(
                frames_seen(&mut stale, 2).await,
                vec![
                    Seen::Tombstone,
                    Seen::Begin {
                        generation: next_bootstrap_id(replacement),
                        base_seq: 4
                    }
                ],
            );

            drop(output);
            for consumer in [fresh, stale] {
                let fault = consumer.task.await.expect("pump task");
                assert!(fault.is_none(), "pumps stop cleanly when the pane closes");
            }
        })
        .await;
}
