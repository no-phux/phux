//! Real wire decoding and kernel import for publication acceptance fixtures.

use super::*;
use phux_client_core::engine::{CanonicalGeometry, ghostty::GhosttyAdapter};
use phux_client_core::session::{EffectBuffer, KernelInput, SessionKernel};
use phux_terminal::actor::{NativeHistoryRequest, TerminalHandle};

pub(super) struct Replica {
    pub(super) kernel: SessionKernel<GhosttyAdapter>,
    pub(super) frames: Vec<FrameKind>,
    effects: EffectBuffer,
}

impl Replica {
    pub(super) fn output_sequences(&self) -> Vec<u64> {
        self.frames
            .iter()
            .filter_map(|frame| match frame {
                FrameKind::ResourceOutput { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect()
    }

    pub(super) fn new() -> Self {
        Self {
            kernel: SessionKernel::new(
                GhosttyAdapter::new(BootstrapLimits::default()),
                native_profile(),
            ),
            frames: Vec::new(),
            effects: EffectBuffer::new(),
        }
    }

    pub(super) fn import(&mut self, frame: &FrameKind) {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        let (frame, tail) = FrameKind::decode(&encoded).expect("wire decode");
        assert!(tail.is_empty());
        let input = input(&frame);
        if let Some(input) = input {
            self.kernel
                .update(input, &mut self.effects)
                .expect("real kernel import");
        }
        self.frames.push(frame);
    }

    pub(super) fn drain(&mut self, mailbox: &mut tokio::sync::mpsc::Receiver<Outbound>) {
        while let Ok(outbound) = mailbox.try_recv() {
            let Outbound::Frame(frame) = outbound else {
                panic!("wire frame")
            };
            self.import(&frame);
        }
    }

    pub(super) async fn through_ready(
        &mut self,
        mailbox: &mut tokio::sync::mpsc::Receiver<Outbound>,
        generation: BootstrapId,
    ) {
        loop {
            let outbound = tokio::time::timeout(std::time::Duration::from_secs(10), mailbox.recv())
                .await
                .expect("pump READY deadline")
                .expect("pump alive");
            let Outbound::Frame(frame) = outbound else {
                panic!("wire frame")
            };
            let ready = matches!(&frame, FrameKind::BootstrapReady { bootstrap_id, .. } if *bootstrap_id == generation);
            self.import(&frame);
            if ready {
                return;
            }
        }
    }

    pub(super) async fn through_output(
        &mut self,
        mailbox: &mut tokio::sync::mpsc::Receiver<Outbound>,
        sequence: u64,
    ) {
        loop {
            let outbound = tokio::time::timeout(std::time::Duration::from_secs(10), mailbox.recv())
                .await
                .expect("live output deadline")
                .expect("pump alive");
            let Outbound::Frame(frame) = outbound else {
                panic!("wire frame")
            };
            let done = matches!(&frame, FrameKind::ResourceOutput { seq, .. } if *seq == sequence);
            self.import(&frame);
            if done {
                return;
            }
        }
    }

    pub(super) async fn finish_history(&mut self, terminal: &TerminalHandle, owner: u64) -> usize {
        use phux_client_core::session::{KernelEffect, KernelSend};
        let (outbound, _receiver) = tokio::sync::mpsc::channel(1);
        let mut pages = 0;
        loop {
            let request = self
                .effects
                .as_slice()
                .iter()
                .rev()
                .find_map(|effect| match effect {
                    KernelEffect::Send(KernelSend::HistoryRequest {
                        key,
                        cursor,
                        max_bytes,
                        max_rows,
                    }) => Some((
                        key.terminal_id.clone(),
                        key.stream_id,
                        key.bootstrap_id,
                        cursor.clone(),
                        *max_bytes,
                        *max_rows,
                    )),
                    _ => None,
                });
            self.effects.clear();
            let Some((terminal_id, stream_id, bootstrap_id, cursor, max_bytes, max_rows)) = request
            else {
                break;
            };
            let (reply, response) = oneshot::channel();
            terminal
                .native_history
                .send(NativeHistoryRequest {
                    permit: outbound.clone().reserve_owned().await.expect("permit"),
                    owner,
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor: cursor.into(),
                    max_bytes,
                    max_rows,
                    limits: BootstrapLimits::default(),
                    reply,
                })
                .await
                .expect("real history request");
            let frame = response
                .await
                .expect("history reply")
                .result
                .expect("history result");
            assert!(matches!(frame, FrameKind::HistoryPage { .. }));
            self.import(&frame);
            pages += 1;
            assert!(pages < 1024, "bounded progressive history");
        }
        assert_eq!(
            self.kernel
                .history_cache(&phux_protocol::ids::ResourceId::local(1))
                .expect("history cache")
                .status()
                .state,
            phux_client_core::history::HistoryLoadState::Complete
        );
        pages
    }
}

fn input(frame: &FrameKind) -> Option<KernelInput<'_>> {
    Some(match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => KernelInput::BootstrapBegin {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            profile: *profile,
            geometry: CanonicalGeometry::new(*cols, *rows).expect("geometry"),
            base_seq: *base_seq,
        },
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => KernelInput::BootstrapChunk {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            chunk_seq: *chunk_seq,
            payload,
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => KernelInput::BootstrapReady {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            history_cursor: history_cursor.as_deref(),
        },
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => KernelInput::Tombstone {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            reason: *reason,
            last_valid_seq: *last_valid_seq,
        },
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload,
            rows,
        } => KernelInput::HistoryPage {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            page_seq: *page_seq,
            rows: *rows,
            payload,
            cursor,
            next_cursor: next_cursor.as_deref(),
        },
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => KernelInput::ResourceOutput {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
            payload: bytes,
        },
        FrameKind::ResourceSpawned { .. } => return None,
        other => panic!("unexpected publication frame: {other:?}"),
    })
}
