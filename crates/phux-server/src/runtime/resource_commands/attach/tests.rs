use super::*;

#[test]
fn bootstrap_chunks_obey_even_a_one_byte_budget() {
    let terminal = phux_protocol::ids::ResourceId::local(1);
    let stream = phux_protocol::ids::StreamId::new(1).unwrap();
    let generation = phux_protocol::ids::BootstrapId::new(1).unwrap();
    let cut = AgentSessionBootstrap {
        base_seq: 2,
        resume_after_seq: None,
        records: vec![
            Bytes::from_static(b"first\n"),
            Bytes::from_static(b"second\n"),
        ],
        dropped: 0,
        ended: false,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    };
    for budget in [1, 3, 7, 13, 64] {
        let limits = BootstrapLimits::new(budget, 1024).unwrap();
        let frames: Vec<_> =
            bootstrap_frames(&terminal, stream, generation, &cut, limits).collect();
        assert!(matches!(
            frames.first(),
            Some(FrameKind::BootstrapBegin { base_seq: 2, .. })
        ));
        assert!(matches!(
            frames.last(),
            Some(FrameKind::BootstrapReady { .. })
        ));
        let mut bytes = Vec::new();
        for (index, frame) in frames[1..frames.len() - 1].iter().enumerate() {
            let FrameKind::BootstrapChunk {
                chunk_seq, payload, ..
            } = frame
            else {
                panic!("unexpected frame: {frame:?}");
            };
            assert_eq!(*chunk_seq as usize, index);
            assert!(!payload.is_empty() && payload.len() <= budget as usize);
            bytes.extend_from_slice(payload);
        }
        assert_eq!(bytes, b"first\nsecond\n");
        if budget >= 7 {
            let chunks: Vec<_> = frames[1..frames.len() - 1]
                .iter()
                .map(|frame| match frame {
                    FrameKind::BootstrapChunk { payload, .. } => payload.as_ref(),
                    _ => unreachable!("checked chunk frames above"),
                })
                .collect();
            assert!(
                chunks.iter().all(|chunk| chunk.ends_with(b"\n")),
                "records that fit must remain whole for older consumers"
            );
        }
    }
}

#[test]
fn empty_agent_bootstrap_has_only_begin_and_ready() {
    let cut = AgentSessionBootstrap {
        base_seq: 0,
        resume_after_seq: None,
        records: Vec::new(),
        dropped: 0,
        ended: false,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    };
    let frames: Vec<_> = bootstrap_frames(
        &phux_protocol::ids::ResourceId::local(1),
        phux_protocol::ids::StreamId::new(1).unwrap(),
        phux_protocol::ids::BootstrapId::new(1).unwrap(),
        &cut,
        BootstrapLimits::new(1, 1).unwrap(),
    )
    .collect();
    assert_eq!(frames.len(), 2);
    assert!(matches!(frames[0], FrameKind::BootstrapBegin { .. }));
    assert!(matches!(frames[1], FrameKind::BootstrapReady { .. }));
}

/// A consumer whose mailbox stays full while the producer outruns the
/// output broadcast, attached through the production attach path.
struct Stalled {
    state: SharedState,
    client: ClientId,
    core: phux_core::ids::ResourceId,
    handle: crate::resource::ResourceHandle,
    out_rx: tokio::sync::mpsc::Receiver<Outbound>,
    initial: phux_protocol::ids::BootstrapId,
    _out_tx: tokio::sync::mpsc::Sender<Outbound>,
    _connection: CancellationToken,
}

const STALLED_MAILBOX: usize = 4;

async fn stalled_consumer(log_bytes: u32) -> Stalled {
    let state = SharedState::new();
    let client = ClientId(1);
    let core = phux_core::ids::ResourceId::default();
    let bundle = crate::resource::agent_session::AgentSessionActor::build(
        core,
        "claude",
        None,
        CancellationToken::new(),
        log_bytes,
    );
    tokio::task::spawn_local(bundle.actor.run());
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(STALLED_MAILBOX);
    let connection = CancellationToken::new();
    let result = attach_agent_session(
        &state,
        client,
        &phux_protocol::ids::ResourceId::local(9),
        core,
        &bundle.handle,
        &out_tx,
        BootstrapLimits::new(64 * 1024, 1024).unwrap(),
        &connection,
        None,
    )
    .await;
    assert_eq!(result, CommandResult::Ok);
    let Some(Outbound::Frame(FrameKind::BootstrapBegin { bootstrap_id, .. })) = out_rx.recv().await
    else {
        panic!("the attach opens with BEGIN");
    };
    assert!(matches!(
        out_rx.recv().await,
        Some(Outbound::Frame(FrameKind::BootstrapReady { .. }))
    ));
    Stalled {
        state,
        client,
        core,
        handle: bundle.handle,
        out_rx,
        initial: bootstrap_id,
        _out_tx: out_tx,
        _connection: connection,
    }
}

impl Stalled {
    /// Append two records per batch, unread, until the broadcast has lapped
    /// the consumer; the last record's seq.
    async fn outrun_the_broadcast(&self) -> u64 {
        let session = self.handle.agent_session().unwrap();
        let batches = crate::resource::output_broadcast_capacity() + STALLED_MAILBOX + 64;
        let mut last = 0;
        for _ in 0..batches {
            let (reply, rx) = oneshot::channel();
            session
                .append
                .send(crate::resource::agent_session::AppendRequest {
                    bytes: Bytes::from_static(
                        b"{\"type\":\"tool_start\"}\n{\"type\":\"tool_end\"}\n",
                    ),
                    reply,
                    publication: None,
                })
                .await
                .unwrap();
            last = rx.await.unwrap().unwrap().last_seq;
        }
        last
    }

    /// Read the mailbox until the consumer holds everything through `last`.
    async fn drain_through(&mut self, last: u64) -> Vec<FrameKind> {
        let mut frames = Vec::new();
        let (mut covered, mut cut) = (0, 0);
        while covered < last {
            let next = tokio::time::timeout(std::time::Duration::from_secs(10), self.out_rx.recv());
            let Some(Outbound::Frame(frame)) = next.await.expect("the consumer converges") else {
                panic!("the pump ended before the consumer caught up");
            };
            match &frame {
                FrameKind::ResourceOutput { seq, .. } => covered = *seq,
                FrameKind::BootstrapBegin { base_seq, .. } => cut = *base_seq,
                FrameKind::BootstrapReady { .. } => covered = cut,
                _ => {}
            }
            frames.push(frame);
        }
        frames
    }
}

fn seqs_of(payload: &[u8]) -> Vec<u64> {
    payload
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let record: serde_json::Value = serde_json::from_slice(line).unwrap();
            record["seq"].as_u64().unwrap()
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn a_lagged_consumer_is_replayed_the_skipped_records_in_order() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut stalled = stalled_consumer(4 * 1024 * 1024).await;
            let last = stalled.outrun_the_broadcast().await;
            let frames = stalled.drain_through(last).await;
            let mut delivered = Vec::new();
            let mut replayed = 0;
            for frame in frames {
                let FrameKind::ResourceOutput {
                    bootstrap_id,
                    seq,
                    bytes,
                    ..
                } = frame
                else {
                    panic!("a recoverable lag stays in its generation: {frame:?}");
                };
                assert_eq!(bootstrap_id, stalled.initial);
                let seqs = seqs_of(&bytes);
                assert_eq!(
                    seqs.last(),
                    Some(&seq),
                    "the envelope names its last record"
                );
                replayed += usize::from(seqs.len() == 1);
                delivered.extend(seqs);
            }
            assert!(
                replayed > 0,
                "the consumer never lagged; the test proves nothing"
            );
            assert_eq!(
                delivered,
                (1..=last).collect::<Vec<_>>(),
                "no gap, no duplicate"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_consumer_lagged_past_eviction_gets_a_tombstone_and_a_full_replacement() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut stalled = stalled_consumer(1024).await;
            let last = stalled.outrun_the_broadcast().await;
            let frames = stalled.drain_through(last).await;
            let tombstone_at = frames
                .iter()
                .position(|frame| matches!(frame, FrameKind::BootstrapTombstone { .. }))
                .expect("an evicted window is an explicit gap");
            let mut delivered = Vec::new();
            for frame in &frames[..tombstone_at] {
                let FrameKind::ResourceOutput {
                    bootstrap_id,
                    bytes,
                    ..
                } = frame
                else {
                    panic!("only live output precedes the gap: {frame:?}");
                };
                assert_eq!(*bootstrap_id, stalled.initial);
                delivered.extend(seqs_of(bytes));
            }
            assert_eq!(delivered, (1..=delivered.len() as u64).collect::<Vec<_>>());
            let FrameKind::BootstrapTombstone {
                bootstrap_id,
                reason,
                last_valid_seq,
                ..
            } = &frames[tombstone_at]
            else {
                unreachable!()
            };
            assert_eq!(*bootstrap_id, stalled.initial);
            assert_eq!(
                *reason,
                phux_protocol::wire::frame::TombstoneReason::OutboundGap
            );
            assert_eq!(*last_valid_seq, delivered.last().copied().unwrap_or(0));

            let FrameKind::BootstrapBegin {
                bootstrap_id: replacement,
                profile,
                base_seq,
                ..
            } = &frames[tombstone_at + 1]
            else {
                panic!(
                    "the replacement follows its tombstone: {:?}",
                    frames[tombstone_at + 1]
                );
            };
            assert_ne!(*replacement, stalled.initial);
            assert_eq!(*profile, BootstrapStreamProfile::AgentEventsJsonlV1);
            let mut retained = Vec::new();
            let mut live = Vec::new();
            for frame in &frames[tombstone_at + 2..] {
                match frame {
                    FrameKind::BootstrapChunk {
                        bootstrap_id,
                        payload,
                        ..
                    } => {
                        assert_eq!(bootstrap_id, replacement);
                        retained.extend(seqs_of(payload));
                    }
                    FrameKind::BootstrapReady { bootstrap_id, .. } => {
                        assert_eq!(bootstrap_id, replacement);
                    }
                    FrameKind::ResourceOutput {
                        bootstrap_id,
                        bytes,
                        ..
                    } => {
                        assert_eq!(bootstrap_id, replacement);
                        live.extend(seqs_of(bytes));
                    }
                    other => panic!("unexpected frame after the replacement: {other:?}"),
                }
            }
            assert!(
                retained
                    .first()
                    .is_some_and(|first| *first > last_valid_seq + 1),
                "the replacement starts after the evicted records: {retained:?}"
            );
            assert_eq!(retained.last(), Some(base_seq));
            let resumed: Vec<u64> = retained.iter().chain(&live).copied().collect();
            assert_eq!(resumed, (resumed[0]..=last).collect::<Vec<_>>());

            // A later re-attach tombstones the replacement, not the retired one.
            let (_, _, _, prior) = stalled.state.with_mut(|s| {
                s.replace_attach_terminal_pump(
                    stalled.client,
                    stalled.core,
                    phux_protocol::ids::BootstrapId::new(u64::MAX).unwrap(),
                )
            });
            assert_eq!(prior.map(|(id, ..)| id), Some(*replacement));
        })
        .await;
}
