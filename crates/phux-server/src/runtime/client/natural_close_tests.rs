//! Natural-close wire ordering, using real terminal actors and tracked pumps.

use super::*;
use crate::runtime::attach::{OutputPumpContext, OutputPumpStart, run_started_output_pump};
use crate::terminal_actor::{PaneOutput, ResyncAudience, ResyncReason};
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::BootstrapId;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;

const FINAL_SCREEN: &[u8] = b"FINAL_SCREEN";
const GUARD: std::time::Duration = std::time::Duration::from_secs(3);

async fn pending_once<F: Future>(mut future: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "expected a blocked operation"
        );
        Poll::Ready(())
    })
    .await;
}

struct Fixture {
    state: SharedState,
    pane: phux_core::ResourceId,
    wire: WireResourceId,
    terminal: crate::terminal_actor::TerminalHandle,
    output: tokio::sync::broadcast::Sender<PaneOutput>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_seed(FINAL_SCREEN)
    }

    fn with_seed(seed: &[u8]) -> Self {
        let state = SharedState::new();
        let actor = crate::terminal_actor::TerminalActor::new_with_seed(80, 24, seed).unwrap();
        let output = actor.handle.output.clone();
        let terminal = actor.handle.terminal().unwrap().clone();
        let (pane, wire) = state.with_mut(|s| {
            let (_, _, pane) = s.seed_session("close-test");
            let wire =
                s.spawn_resource_actor(pane, actor.handle, actor.token.clone(), actor.actor.run());
            (pane, wire)
        });
        Self {
            state,
            pane,
            wire,
            terminal,
            output,
        }
    }

    fn consumer(
        &self,
        client: ClientId,
        capacity: usize,
    ) -> (OutputPumpContext, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(capacity);
        for nonce in 0..capacity as u64 {
            tx.try_send(Outbound::Frame(FrameKind::Pong { nonce }))
                .unwrap();
        }
        self.state
            .with_mut(|s| s.subscribe_terminal(client, self.pane, Some(tx.clone())));
        let ctx = OutputPumpContext {
            out_tx: tx,
            resize: self.terminal.resize.clone(),
            wire_terminal_id: self.wire.clone(),
            stream_id: StreamId::new(1).unwrap(),
            initial_bootstrap_id: BootstrapId::new(1).unwrap(),
            client_id: client,
            client_caps: ClientCapabilities::default(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            lag_label: "natural close test",
            stale_skip: false,
            cancel: None,
            drain: Some(CancellationToken::new()),
            snapshot: Some(self.terminal.snapshot.clone()),
            last_seq: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            terminal: self.terminal.clone(),
        };
        (ctx, rx)
    }

    fn track(&self, ctx: OutputPumpContext, pumps: &mut JoinSet<()>) {
        let output = self.output.subscribe();
        self.track_future(ctx.client_id, ctx.drain.clone(), pumps, async move {
            assert!(
                run_started_output_pump(&ctx, start(), output)
                    .await
                    .is_none()
            );
        });
    }

    fn track_future(
        &self,
        client: ClientId,
        drain: Option<CancellationToken>,
        pumps: &mut JoinSet<()>,
        future: impl Future<Output = ()> + 'static,
    ) {
        super::super::pump::spawn_tracked(
            &self.state,
            client,
            self.pane,
            Some(pumps),
            drain,
            future,
        );
    }

    fn reap(&self) -> ReapAndNotify {
        self.state
            .with_mut(|s| reap_exited_pane(s, self.pane, ExitOutcome::exited(0), None))
            .unwrap()
    }
}

fn start() -> OutputPumpStart {
    OutputPumpStart {
        published_cut: 0,
        replay: Vec::new(),
        live: None,
    }
}

/// Concatenate every chunk belonging to the generation that reached READY:
/// chunk boundaries are arbitrary, so searching individual chunks is unsound.
#[derive(Default)]
struct FinalOracle {
    payload: Vec<u8>,
    ready: bool,
    chunks: usize,
}

impl FinalOracle {
    fn observe(&mut self, frame: FrameKind, wire: &WireResourceId) -> bool {
        match frame {
            FrameKind::BootstrapBegin { .. } => {
                self.payload.clear();
                self.ready = false;
                self.chunks = 0;
            }
            FrameKind::BootstrapChunk { payload, .. } => {
                self.payload.extend_from_slice(&payload);
                self.chunks += 1;
            }
            FrameKind::BootstrapReady { .. } => self.ready = true,
            FrameKind::ResourceClosed { terminal_id, .. } => {
                assert_eq!(&terminal_id, wire);
                assert!(self.ready, "close overtook final READY");
                assert!(
                    self.payload
                        .windows(FINAL_SCREEN.len())
                        .any(|part| part == FINAL_SCREEN),
                    "close overtook the final screen"
                );
                return true;
            }
            _ => {}
        }
        false
    }
}

async fn through_close(rx: &mut mpsc::Receiver<Outbound>, wire: &WireResourceId) -> FinalOracle {
    tokio::time::timeout(GUARD, async {
        let mut oracle = FinalOracle::default();
        loop {
            let Outbound::Frame(frame) = rx.recv().await.expect("mailbox remains alive") else {
                panic!("unexpected outbound")
            };
            if oracle.observe(frame, wire) {
                return oracle;
            }
        }
    })
    .await
    .expect("natural close did not complete")
}

async fn finish_pumps(pumps: &mut JoinSet<()>) {
    tokio::time::timeout(GUARD, async {
        while let Some(result) = pumps.join_next().await {
            result.unwrap();
        }
    })
    .await
    .expect("output pumps did not terminate");
}

async fn close_with_capacity(capacity: usize, limits: BootstrapLimits) {
    let fixture = Fixture::new();
    let (mut ctx, mut rx) = fixture.consumer(ClientId(1), capacity);
    ctx.limits = limits;
    let mut pumps = JoinSet::new();
    fixture.track(ctx, &mut pumps);
    let close = announce_close(&fixture.state, fixture.reap(), false);
    tokio::pin!(close);
    pending_once(close.as_mut()).await;
    let (oracle, _) = tokio::join!(through_close(&mut rx, &fixture.wire), close);
    assert!(oracle.chunks > 0);
    finish_pumps(&mut pumps).await;
    assert!(rx.try_recv().is_err(), "terminal output followed close");
}

/// No Exit broadcast is needed: final capture must come from the live actor.
/// A fitting batch reserves the complete generation before publishing it.
#[tokio::test(flavor = "current_thread")]
async fn a_natural_close_captures_the_final_grid_before_close() {
    tokio::task::LocalSet::new()
        .run_until(close_with_capacity(8, BootstrapLimits::default()))
        .await;
}

/// A replacement larger than the mailbox streams several chunks; READY must
/// still fence close, including a final marker split across codec chunks.
#[tokio::test(flavor = "current_thread")]
async fn an_oversized_final_generation_precedes_close() {
    tokio::task::LocalSet::new()
        .run_until(close_with_capacity(
            1,
            BootstrapLimits::new(7, 1024).unwrap(),
        ))
        .await;
}

/// Poll the real pump into a blocked ordinary gap publication, then request
/// natural close. Its old replacement must finish before the fresh EOF cut.
#[tokio::test(flavor = "current_thread")]
async fn close_waits_for_a_pump_parked_on_a_prior_gap_generation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = Fixture::new();
            let (ctx, mut rx) = fixture.consumer(ClientId(1), 8);
            let client = ctx.client_id;
            let drain = ctx.drain.clone();
            let (output, _) = tokio::sync::broadcast::channel(8);
            let live = output.subscribe();
            output
                .send(PaneOutput::Live {
                    seq: 1,
                    bytes: bytes::Bytes::from_static(b"GAPPED"),
                    at: std::time::Instant::now(),
                })
                .unwrap();
            output
                .send(PaneOutput::Resync {
                    cols: 80,
                    rows: 24,
                    bytes: bytes::Bytes::from_static(b"OLD_SCREEN"),
                    reason: ResyncReason::OutboundGap,
                    audience: ResyncAudience::Everyone,
                    base_seq: 1,
                })
                .unwrap();
            let pump = async move {
                assert!(run_started_output_pump(&ctx, start(), live).await.is_none());
            };
            let mut pump = Box::pin(pump);
            pending_once(pump.as_mut()).await;
            let mut pumps = JoinSet::new();
            fixture.track_future(client, drain, &mut pumps, pump);
            let close = announce_close(&fixture.state, fixture.reap(), false);
            tokio::pin!(close);
            pending_once(close.as_mut()).await;
            let (oracle, _) = tokio::join!(through_close(&mut rx, &fixture.wire), close);
            assert!(
                !oracle
                    .payload
                    .windows(b"OLD_SCREEN".len())
                    .any(|part| part == b"OLD_SCREEN"),
                "old gap bootstrap was mistaken for final capture"
            );
            finish_pumps(&mut pumps).await;
            assert!(rx.try_recv().is_err());
        })
        .await;
}

/// Closing one reader must not await an unrelated reader's full mailbox.
#[tokio::test(flavor = "current_thread")]
async fn a_healthy_subscriber_receives_final_close_while_another_is_blocked() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = Fixture::new();
            let (slow, mut slow_rx) = fixture.consumer(ClientId(1), 1);
            let (fast, mut fast_rx) = fixture.consumer(ClientId(2), 8);
            let mut pumps = JoinSet::new();
            fixture.track(slow, &mut pumps);
            fixture.track(fast, &mut pumps);
            let close = announce_close(&fixture.state, fixture.reap(), false);
            tokio::pin!(close);
            tokio::select! {
                biased;
                _ = &mut close => panic!("close completed while the slow mailbox was still full"),
                _ = through_close(&mut fast_rx, &fixture.wire) => {}
            }
            pending_once(close.as_mut()).await;
            assert!(matches!(
                slow_rx.try_recv(),
                Ok(Outbound::Frame(FrameKind::Pong { nonce: 0 }))
            ));
            let (_, _) = tokio::join!(through_close(&mut slow_rx, &fixture.wire), close);
            finish_pumps(&mut pumps).await;
            assert!(fast_rx.try_recv().is_err());
            assert!(slow_rx.try_recv().is_err());
        })
        .await;
}

/// `DETACH_RESOURCE` can name a wire already retired by reap. Its returned ack
/// fences a blocked final publisher, which must never add terminal frames.
#[tokio::test(flavor = "current_thread")]
async fn detaching_a_reaped_wire_aborts_final_publication_before_ack() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = Fixture::new();
            let client = ClientId(1);
            let (mut ctx, mut rx) = fixture.consumer(client, 1);
            let (snapshot, mut requests) = mpsc::channel(1);
            ctx.snapshot = Some(snapshot);
            let drain = ctx.drain.clone();
            drain.as_ref().unwrap().cancel();
            let output = fixture.output.subscribe();
            let pump = async move {
                assert!(
                    run_started_output_pump(&ctx, start(), output)
                        .await
                        .is_none()
                );
            };
            let mut pump = Box::pin(pump);
            // The first poll reaches the capture reply barrier.
            pending_once(pump.as_mut()).await;
            let request = requests.try_recv().expect("pump reached final capture");
            assert_eq!(request.scrollback, None);
            request
                .reply
                .send(Ok((
                    crate::grid::SnapshotBytes {
                        cols: 80,
                        rows: 24,
                        bytes: FINAL_SCREEN.to_vec(),
                        scrollback: Vec::new(),
                    },
                    0,
                )))
                .unwrap();
            // With a supplied capture, the only remaining wait is publishing
            // into the full mailbox: DETACH must stop that actual operation.
            pending_once(pump.as_mut()).await;
            assert!(requests.try_recv().is_err());
            let mut pumps = JoinSet::new();
            fixture.track_future(client, drain, &mut pumps, pump);
            let reap = fixture.reap();
            assert!(
                fixture
                    .state
                    .with(|s| s.terminal_from_wire(&fixture.wire).is_none())
            );
            let close = announce_close(&fixture.state, reap, false);
            tokio::pin!(close);
            pending_once(close.as_mut()).await;
            let result = tokio::time::timeout(
                GUARD,
                handle_detach_terminal(&fixture.state, client, &fixture.wire),
            )
            .await
            .expect("DETACH remained blocked on the retired wire");
            assert!(matches!(result, CommandResult::Ok));
            tokio::time::timeout(GUARD, async {
                while let Some(result) = pumps.join_next().await {
                    assert!(result.is_ok() || result.is_err_and(|error| error.is_cancelled()));
                }
            })
            .await
            .expect("detached output task did not terminate");
            assert!(matches!(
                rx.try_recv(),
                Ok(Outbound::Frame(FrameKind::Pong { .. }))
            ));
            let ((), _) = tokio::join!(
                async {
                    let Outbound::Frame(FrameKind::ResourceClosed { .. }) =
                        rx.recv().await.unwrap()
                    else {
                        panic!("publisher emitted terminal frames after DETACH ack")
                    };
                },
                close
            );
            assert!(
                rx.try_recv().is_err(),
                "output followed DETACH acknowledgement"
            );
        })
        .await;
}

/// Native READY is a validated engine checkpoint, not merely a frame
/// label: decode its opaque payload and recover the actor's final title.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn a_native_final_checkpoint_is_decodable_before_close() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = Fixture::with_seed(b"FINAL_SCREEN\x1b]2;final-close-title\x07");
            let (mut ctx, mut rx) = fixture.consumer(ClientId(1), 1);
            ctx.profile = BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
            };
            let mut pumps = JoinSet::new();
            fixture.track(ctx, &mut pumps);
            let close = announce_close(&fixture.state, fixture.reap(), false);
            tokio::pin!(close);
            pending_once(close.as_mut()).await;
            let receive = tokio::time::timeout(GUARD, async {
                let mut payload = Vec::new();
                let mut validated_ready = false;
                loop {
                    let Outbound::Frame(frame) = rx.recv().await.unwrap() else {
                        panic!("unexpected outbound")
                    };
                    match frame {
                        FrameKind::BootstrapBegin { profile, .. } => {
                            assert!(matches!(
                                profile,
                                BootstrapStreamProfile::NativeState { .. }
                            ));
                            payload.clear();
                            validated_ready = false;
                        }
                        FrameKind::BootstrapChunk { payload: chunk, .. } => {
                            payload.extend_from_slice(&chunk);
                        }
                        FrameKind::BootstrapReady { .. } => {
                            assert!(!payload.is_empty(), "native READY had no checkpoint");
                            let mut input = std::io::Cursor::new(&payload);
                            let decoder = libghostty_vt::snapshot::Decoder::new(&mut input)
                                .expect("valid native envelope");
                            let checkpoint = decoder.ready().expect("validated native READY");
                            assert_eq!(checkpoint.terminal().title().unwrap(), "final-close-title");
                            validated_ready = true;
                        }
                        FrameKind::ResourceClosed { terminal_id, .. } => {
                            assert_eq!(terminal_id, fixture.wire);
                            assert!(
                                validated_ready,
                                "close overtook validated native checkpoint"
                            );
                            break;
                        }
                        _ => {}
                    }
                }
            });
            let (received, _) = tokio::join!(receive, close);
            received.expect("native final close never completed");
            finish_pumps(&mut pumps).await;
            assert!(rx.try_recv().is_err(), "native frames followed close");
        })
        .await;
}
