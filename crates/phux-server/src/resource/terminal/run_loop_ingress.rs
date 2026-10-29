use super::{
    Bytes, EncodedInputRequest, MAX_INPUT_COALESCE, MAX_PTY_COALESCE, MAX_PTY_COALESCE_BYTES,
    PaneOutput, PtyBurst, PtyEvent, PtyTurn, TerminalActor, TerminalInput, debug, error,
    join_pty_burst, mpsc,
};

impl TerminalActor {
    /// Service one encoded-input wakeup plus up to [`MAX_INPUT_COALESCE`]
    /// already-queued requests.
    pub(super) fn service_encoded_input_batch(&mut self, request: EncodedInputRequest) {
        self.service_encoded_input(request);
        for _ in 1..MAX_INPUT_COALESCE {
            match self.encoded_input_rx.try_recv() {
                Ok(next) => self.service_encoded_input(next),
                Err(_) => break,
            }
        }
    }

    /// Service one inline-input wakeup plus queued followers.
    pub(super) fn service_input_batch(&mut self, input: &TerminalInput) {
        self.service_input(input);
        for _ in 1..MAX_INPUT_COALESCE {
            match self.input_rx.try_recv() {
                Ok(next) => self.service_input(&next),
                Err(_) => break,
            }
        }
    }

    /// Ingest one PTY event: a bounded coalesced write and broadcast, or EOF.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub(super) async fn service_pty_event(&mut self, evt: Option<PtyEvent>) -> PtyTurn {
        let Some(PtyEvent::Bytes {
            chunk: first,
            read_at,
        }) = evt
        else {
            // `Some(PtyEvent::Eof)` or a dropped sender (`None`).
            self.handle_pty_eof();
            return PtyTurn::Continue;
        };
        phux_perf::echo_hop::record("main");
        crate::perf::PTY_QUEUE_WAIT.record_elapsed(read_at);
        // Server-side echo latency; slower than the ceiling means the
        // program did not echo.
        if let Some(input_at) = self.last_input_at.take() {
            let since_input = input_at.elapsed();
            if since_input < crate::perf::ECHO_SAMPLE_CEILING {
                crate::perf::ECHO_SERVER.record_duration(since_input);
            }
        }
        self.last_output_at.set(Some(std::time::Instant::now()));
        let burst = self.coalesce_pty_burst(first);
        crate::perf::PTY_BURST_BYTES.record_len(burst.payload.len());
        crate::perf::PTY_BURST_CHUNKS.record(burst.chunks);
        debug!(
            bytes = burst.payload.len(),
            "vt_write: PTY chunk(s) -> Terminal"
        );
        let Some(seq) = self.core.next_seq() else {
            error!("resource output sequence exhausted");
            self.shutdown_pty().await;
            return PtyTurn::Shutdown;
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let deferred = self.buffer_native_live_output(seq, &burst.payload);
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        let deferred = false;
        if !deferred {
            self.ingest_pty_payload(&burst.payload);
        }
        let _ = self.core.output_tx.send(PaneOutput::Live {
            seq,
            bytes: burst.payload,
            at: read_at,
        });
        let native_step_due = self.native_work_pending();
        if burst.saw_eof {
            self.handle_pty_eof();
        } else if burst.hit_byte_cap {
            // Capped with more queued: yield so input and sibling tasks run.
            let yield_started = std::time::Instant::now();
            tokio::task::yield_now().await;
            crate::perf::PTY_YIELD_WAIT.record_elapsed(yield_started);
        }
        PtyTurn::Stepped(native_step_due)
    }

    /// Coalesce chunks queued behind `first` into one write and broadcast.
    /// A lone chunk is copy-free; the drain stops at the chunk cap, EOF, or
    /// before a chunk that would cross `MAX_PTY_COALESCE_BYTES`.
    pub(super) fn coalesce_pty_burst(&mut self, first: Bytes) -> PtyBurst {
        let mut queued = std::mem::take(&mut self.pty_burst);
        let mut total = first.len();
        let mut saw_eof = false;
        let mut hit_byte_cap = false;
        for _ in 0..MAX_PTY_COALESCE {
            // The first chunk always lands; only coalescing is capped.
            if total >= MAX_PTY_COALESCE_BYTES {
                hit_byte_cap = true;
                break;
            }
            match self.pty_rx.as_mut().map(mpsc::Receiver::try_recv) {
                Some(Ok(PtyEvent::Bytes { chunk: more, .. })) => {
                    total += more.len();
                    queued.push(more);
                }
                // Flush coalesced bytes, then handle EOF.
                Some(Ok(PtyEvent::Eof)) => {
                    saw_eof = true;
                    break;
                }
                // Empty or disconnected (EOF arrives on the next wakeup).
                _ => break,
            }
        }
        let chunks = 1 + queued.len() as u64;
        let payload = join_pty_burst(first, &queued, total);
        queued.clear();
        self.pty_burst = queued;
        PtyBurst {
            payload,
            chunks,
            saw_eof,
            hit_byte_cap,
        }
    }

    /// Write one coalesced payload into the canonical `Terminal` and fan out
    /// everything derived from it (encoder snapshot, dirty bits, semantic
    /// events, native bootstrap advance). The engine answers queries itself,
    /// in byte order, through `on_pty_write`. The encoder snapshot is
    /// recaptured every chunk (a handful of mode reads) and published only
    /// when it changed.
    pub(super) fn ingest_pty_payload(&mut self, payload: &Bytes) {
        // Three samples, three clock reads. Drop guards around the same
        // intervals read the clock again on the way out and fold that probe
        // into `pty.vt_apply`.
        let started = std::time::Instant::now();
        self.terminal.borrow_mut().vt_write(payload);
        let parsed = std::time::Instant::now();
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
        self.source_events();
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.start_next_native_bootstrap();
        let done = std::time::Instant::now();
        crate::perf::PTY_VT_PARSE.record_duration(parsed.saturating_duration_since(started));
        crate::perf::PTY_POST_APPLY.record_duration(done.saturating_duration_since(parsed));
        crate::perf::PTY_VT_APPLY.record_duration(done.saturating_duration_since(started));
    }
}
