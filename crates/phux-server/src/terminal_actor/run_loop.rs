//! The [`TerminalActor`] event loop (`run`) and the state-sync tick
//! emitter (`tick_emit`).

use super::{
    AgentDetector, Bytes, CanonicalTerminal, ClientId, ConsumerAckRequest, ConsumerDetachRequest,
    DEFAULT_TICK_INTERVAL, FrameKind, MAX_EMIT_INSTANTS, MAX_INPUT_COALESCE, MAX_PTY_COALESCE,
    MAX_PTY_COALESCE_BYTES, NativeOrPty, Outbound, PaneOutput, PaneUpgradeHandle, PtyEvent,
    RESIZE_RESYNC_DEBOUNCE, ResyncReason, SnapshotBytes, TerminalActor, debug, error, mpsc,
    recv_native_or_pty, tick, trace, warn,
};

impl TerminalActor {
    /// Run the actor's event loop until shutdown.
    ///
    /// Native prefix capture advances by one record between ingress turns.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "single select! loop; arms are short and inlined for locality"
    )]
    #[allow(
        clippy::cognitive_complexity,
        reason = "select! macro expansion inflates the score; arms are individually small and locality wins over decomposition"
    )]
    pub async fn run(mut self) {
        debug!(
            cols = self.cols,
            rows = self.rows,
            has_pty = self.pty.is_some(),
            "TerminalActor started",
        );

        // State-sync tick driver (phux-q0e.3 / phux-q0e.5). RTT-adaptive
        // cadence: starts at the `DEFAULT_TICK_INTERVAL` cold-start value and
        // is rebuilt toward each consumer's measured `RTT/2` (clamped to
        // [`MIN_TICK_INTERVAL`, `MAX_TICK_INTERVAL`]) as `FRAME_ACK`
        // round-trips land. The shared timer runs at the minimum desired
        // interval across consumers (see [`Self::adaptive_tick_interval`]).
        // `MissedTickBehavior::Delay` — if the actor falls behind under heavy
        // PTY traffic we want subsequent ticks spaced by the interval from
        // when they ran, not bunched up to "catch up" (which would defeat the
        // rate limit's purpose). `Burst` (the default) would spam emissions
        // when a long PTY chunk delays us past several tick boundaries.
        let mut tick_interval = DEFAULT_TICK_INTERVAL;
        let mut tick = tokio::time::interval(tick_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Eat the first immediate tick (Interval fires synchronously on
        // first poll). Without this, the very first iteration would
        // tick before any other branch has a chance to react.
        let _ = tick.tick().await;

        // Agent-state detector (ADR-0046). Constructed HERE, not in `build`,
        // for two reasons: `started` then anchors the startup grace window at
        // the moment the child actually begins painting, and no existing
        // constructor or test actor grows a detector it never asked for. Only
        // a PTY-backed actor with a wired sink and a non-empty rule set gets
        // one — everything else pays exactly nothing.
        let rules = crate::agent_detect::rules::global();
        if self.pty.is_some() && self.agent_state_sink.is_some() && !rules.is_empty() {
            self.agent_detect = Some(AgentDetector::new(rules, std::time::Instant::now()));
        }
        let mut detect_interval = crate::agent_detect::TICK_UNIDENTIFIED;
        let mut detect_tick = tokio::time::interval(detect_interval);
        detect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let _ = detect_tick.tick().await;

        // phux-8v1 drag fix: debounce timer for the post-resize client
        // resync. (Re)armed on each resync-requesting resize; when it
        // fires we broadcast ONE snapshot at the settled size. Init far
        // out — `resync_pending` is false until a resize arms it, and we
        // always `reset()` the deadline when arming, so the initial
        // instant is never observed.
        let resync_debounce = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::pin!(resync_debounce);
        let mut resync_pending = false;
        let mut resync_reason = ResyncReason::Resize;
        // Native control and PTY output are one outer select arm so the actor
        // never borrows either receiver twice. Preference swaps after every
        // selected ingress, but both sources remain enabled: a silent PTY can
        // never park bootstrap or consecutive history requests.
        let mut prefer_native = false;
        let mut native_step_due = false;

        loop {
            tokio::select! {
                biased;

                () = self.token.cancelled() => {
                    debug!("TerminalActor cancellation token fired");
                    self.shutdown_pty().await;
                    return;
                }

                // Bytes already encoded on the dedicated input lane. This
                // bounded mailbox is the production input path and shares the
                // actor's highest scheduling priority.
                Some(request) = self.encoded_input_rx.recv() => {
                    self.service_encoded_input(request);
                    for _ in 1..MAX_INPUT_COALESCE {
                        match self.encoded_input_rx.try_recv() {
                            Ok(next) => self.service_encoded_input(next),
                            Err(_) => break,
                        }
                    }
                }

                // Legacy inline input → PTY, retained for direct-drive tests
                // that intentionally construct the runtime without a lane.
                // Polled before the PTY-output arm (biased
                // order) so a queued keystroke is serviced this turn
                // rather than waiting behind an output burst — the fix for
                // load-correlated input starvation. Bounded by
                // `MAX_INPUT_COALESCE`: the arm fires on the first ready
                // event, then drains up to a capped batch via `try_recv`
                // so a paste the encoder expands cannot inflate one turn
                // without limit. The PTY-output arm's structural bound is
                // `MAX_PTY_COALESCE_BYTES`.
                Some(input) = self.input_rx.recv() => {
                    self.service_input(&input);
                    for _ in 1..MAX_INPUT_COALESCE {
                        match self.input_rx.try_recv() {
                            Ok(next) => self.service_input(&next),
                            // Empty (nothing more ready) or Disconnected —
                            // stop draining.
                            Err(_) => break,
                        }
                    }
                }

                () = std::future::ready(()),
                    if native_step_due && self.native_bootstrap_pending() =>
                {
                    self.cooperative_native_step();
                    native_step_due = false;
                }

                ingress = recv_native_or_pty(
                    &mut self.native_requests,
                    self.pty_rx.as_mut(),
                    prefer_native,
                ) => {
                    match ingress {
                        NativeOrPty::Native(req) => {
                            prefer_native = false;
                            self.handle_native_actor_request(req);
                            native_step_due = false;
                        }
                        NativeOrPty::Pty(evt) => {
                            prefer_native = true;
                            // PTY -> Terminal + broadcast. One bounded parse
                            // returns to this combined ingress arm so native
                            // control and live output alternate when both are
                            // continuously ready.
                            match evt {
                        Some(PtyEvent::Bytes(first)) => {
                            // Coalesce any chunks already queued behind this one
                            // into a single Terminal write + broadcast frame
                            // (phux-ahk burst path). A lone chunk takes the
                            // fast path below: its `Vec` moves into `Bytes`
                            // with no copy. Only a genuine burst (several
                            // reads queued) allocates a join buffer. The drain
                            // stops on the chunk-count cap, on EOF, or once the
                            // payload would cross `MAX_PTY_COALESCE_BYTES` — in
                            // the byte-cap case the crossing chunk is left
                            // queued for the next turn (mpsc has no peek, so the
                            // length is checked before `try_recv`).
                            let mut coalesced: Vec<u8> = Vec::new();
                            let mut saw_eof = false;
                            // `true` when the drain stopped because the next
                            // chunk would cross the byte cap (more output is
                            // likely queued) rather than because the queue
                            // emptied. Drives the post-broadcast yield so a
                            // sustained burst hands the scheduler a turn
                            // between bounded parses.
                            let mut hit_byte_cap = false;
                            for _ in 0..MAX_PTY_COALESCE {
                                // Length so far: the lone `first` chunk before
                                // any coalescing, else the join buffer. Stop
                                // before consuming a chunk that would push the
                                // payload past the byte cap so each `vt_write`
                                // is a bounded synchronous parse. The first
                                // chunk always lands; only coalescing is capped.
                                let current_len = if coalesced.is_empty() {
                                    first.len()
                                } else {
                                    coalesced.len()
                                };
                                if current_len >= MAX_PTY_COALESCE_BYTES {
                                    hit_byte_cap = true;
                                    break;
                                }
                                match self.pty_rx.as_mut().map(mpsc::UnboundedReceiver::try_recv) {
                                    Some(Ok(PtyEvent::Bytes(more))) => {
                                        if coalesced.is_empty() {
                                            coalesced.reserve(first.len() + more.len());
                                            coalesced.extend_from_slice(&first);
                                        }
                                        coalesced.extend_from_slice(&more);
                                    }
                                    // A queued EOF: flush the coalesced bytes
                                    // first, then handle EOF below.
                                    Some(Ok(PtyEvent::Eof)) => {
                                        saw_eof = true;
                                        break;
                                    }
                                    // Empty (nothing more ready) or the sender
                                    // dropped — stop draining. A dropped sender
                                    // surfaces as EOF on the next pump wakeup.
                                    _ => break,
                                }
                            }
                            let payload: Bytes = if coalesced.is_empty() {
                                Bytes::from(first)
                            } else {
                                Bytes::from(coalesced)
                            };
                            // Debug level deliberately (was trace): this is
                            // the pump's only witness line, and the lost-echo
                            // forensics (phux-dacb follow-up) need it inside
                            // the test capture's debug filter. Per-wakeup, so
                            // it costs one line per coalesced read, not per
                            // byte.
                            debug!(bytes = payload.len(), "vt_write: PTY chunk(s) -> Terminal");
                            let Some(seq) = self.raw_seq.checked_add(1) else {
                                error!("actor-global raw output sequence exhausted");
                                self.shutdown_pty().await;
                                return;
                            };
                            self.raw_seq = seq;
                            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                            let deferred = self.buffer_native_live_output(seq, &payload);
                            #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
                            let deferred = false;
                            if !deferred {
                                self.terminal.borrow_mut().vt_write(&payload);
                                self.answer_color_queries(&payload);
                                self.publish_input_snapshot();
                                self.terminal_dirty_since_tick = true;
                                self.agent_dirty_since_detect = true;
                                self.source_events_from_chunk(&payload);
                                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                                self.start_next_native_bootstrap();
                            }
                            let _ = self.output_tx.send(PaneOutput::Live {
                                seq,
                                bytes: payload,
                            });
                            native_step_due = self.native_bootstrap_pending();
                            if saw_eof {
                                self.handle_pty_eof();
                            } else if hit_byte_cap {
                                // A capped payload with more output queued:
                                // yield so the runtime re-polls (input arm
                                // first) and sibling LocalSet tasks advance,
                                // bounding the output arm at the thread level.
                                // The next loop turn coalesces the next
                                // bounded payload, so throughput is preserved.
                                tokio::task::yield_now().await;
                            }
                        }
                        Some(PtyEvent::Eof) | None => {
                            self.handle_pty_eof();
                        }
                    }
                            }
                        }
                    }



                Some(req) = self.snapshot_rx.recv(), if !self.native_bootstrap_pending() => {
                    let byte_limit = req
                        .max_frames
                        .checked_sub(2)
                        .map(|chunks| chunks.saturating_mul(req.chunk_bytes).min(req.max_bytes))
                        .ok_or(crate::grid::SynthesisError::LimitExceeded);
                    let snap = byte_limit.and_then(|max_bytes| {
                        self.synthesize_with_scrollback_bounded(req.scrollback, max_bytes)
                    });
                    if let Err(err) = &snap {
                        warn!(error = %err, "bounded snapshot synthesis failed");
                    }
                    let _ = req.reply.send(snap.map(|snapshot| (snapshot, self.raw_seq)));
                }

                Some(req) = self.set_default_colors_rx.recv(), if !self.native_bootstrap_pending() => {
                    let result = match &mut *self.terminal.borrow_mut() {
                        CanonicalTerminal::Plain(Some(terminal)) => {
                            Self::install_default_colors(terminal, req.colors)
                        }
                        CanonicalTerminal::Plain(None) => Ok(()),
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        CanonicalTerminal::Native(_) => {
                            warn!("default palette update deferred while native cuts are active");
                            Ok(())
                        }
                    };
                    if let Err(err) = result {
                        warn!(error = %err, "failed to install client default colors");
                    }
                    let _ = req.reply.send(());
                }

                Some(req) = self.screen_rx.recv(), if !self.native_bootstrap_pending() => {
                    let want_cells = req.cells;
                    let screen = self.screen_state(req.pane, req.scrollback, req.cells).unwrap_or_else(|err| {
                        warn!(error = %err, "screen projection failed; replying with empty");
                        phux_core::screen::ScreenState {
                            schema_version: phux_core::screen::SCHEMA_VERSION,
                            pane: req.pane,
                            cols: self.cols,
                            rows: self.rows,
                            cursor: None,
                            lines: Vec::new(),
                            scrollback: Vec::new(),
                            // Honour the request shape even on the error
                            // path: an empty cells vec, not a misleading
                            // `None`, when the caller asked for cells.
                            cells: want_cells.then(Vec::new),
                            ..phux_core::screen::ScreenState::default()
                        }
                    });
                    let _ = req.reply.send(screen);
                }

                Some(req) = self.upgrade_rx.recv(), if !self.native_bootstrap_pending() => {
                    // ADR-0032: hand the upgrade producer this pane's PTY
                    // descriptors + a full replay snapshot. Read-only; mirrors
                    // the snapshot/pwd paths.
                    let snap = self.synthesize_with_scrollback(Some(0)).unwrap_or_else(|err| {
                        warn!(error = %err, "upgrade snapshot synthesis failed; replying empty");
                        SnapshotBytes {
                            cols: self.cols,
                            rows: self.rows,
                            bytes: Vec::new(),
                            scrollback: Vec::new(),
                        }
                    });
                    let pty = self.pty.as_ref();
                    let master_fd = pty.and_then(|p| {
                        let master = p.master.lock().ok()?;
                        let fd = master.as_raw_fd()?;
                        // SAFETY: the master guard keeps `fd` open until `dup`
                        // returns an independently owned descriptor.
                        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
                        let duplicate = rustix::io::dup(borrowed).ok();
                        drop(master);
                        duplicate
                    });
                    let child_pid = pty
                        .and_then(|p| p.child.process_id())
                        .and_then(|id| i32::try_from(id).ok());
                    let cwd = pty
                        .and_then(|p| p.child.process_id())
                        .and_then(crate::cwd_query::process_cwd)
                        .map(|p| p.to_string_lossy().into_owned())
                        .or_else(|| {
                            let last = self.last_known_cwd.borrow().clone();
                            (!last.is_empty()).then_some(last)
                        });
                    let cell_px = (self.cell_px != (0, 0)).then_some(self.cell_px);
                    let title = (!self.last_title.is_empty()).then(|| self.last_title.clone());
                    let _ = req.reply.send(PaneUpgradeHandle {
                        master_fd,
                        child_pid,
                        cols: self.cols,
                        rows: self.rows,
                        cell_px,
                        title,
                        cwd,
                        vt_replay_bytes: snap.bytes,
                        scrollback_bytes: snap.scrollback,
                    });
                }

                Some(req) = self.pwd_rx.recv() => {
                    // Resolve the pane's live working directory by asking
                    // the kernel for the PTY child's CWD (the shell's
                    // directory *now*, after any `cd`). `None` when there
                    // is no PTY (no-PTY actor), the child has no pid, or
                    // the query is unsupported/denied — the caller then
                    // falls back to a non-inherited default.
                    let cwd = self
                        .pty
                        .as_ref()
                        .and_then(|p| p.child.process_id())
                        .and_then(crate::cwd_query::process_cwd)
                        .map(|p| p.to_string_lossy().into_owned());
                    let _ = req.reply.send(cwd);
                }

                Some(req) = self.resize_rx.recv(), if !self.native_bootstrap_pending() => {
                    // A `resync_only` request (from a lagged output pump)
                    // carries no geometry — skip the resize and only schedule
                    // the resync broadcast below.
                    let reflowed = if req.resync_only {
                        false
                    } else {
                        self.handle_resize(req.cols, req.rows, req.cell_px)
                    };
                    // phux-8v1: re-broadcast a full snapshot for live
                    // resizes so client mirrors reconverge after their
                    // independent reflow. Suppressed for the ATTACH-time
                    // resize (the handshake snapshot covers it). Debounced
                    // (RESIZE_RESYNC_DEBOUNCE) so a drag storm — or a burst of
                    // lag-resync requests — coalesces into a single snapshot
                    // rather than flooding the client.
                    //
                    // phux-a5xj: also suppressed when the geometry did not
                    // move. There is no independent reflow to reconverge from
                    // if nothing reflowed, and the resync is what rotates the
                    // bootstrap generation — so a client confirming the size
                    // it already asked for at spawn must not cost the pane the
                    // checkpoint it just published.
                    if req.resync_clients && (req.resync_only || reflowed) {
                        resync_pending = true;
                        resync_reason = if req.resync_only {
                            ResyncReason::OutboundGap
                        } else {
                            ResyncReason::Resize
                        };
                        resync_debounce
                            .as_mut()
                            .reset(tokio::time::Instant::now() + RESIZE_RESYNC_DEBOUNCE);
                    }
                }

                // phux-8v1: debounced resize resync — fires once the
                // resize storm settles (RESIZE_RESYNC_DEBOUNCE after the
                // last resync-requesting resize). Guarded by
                // `resync_pending` so the idle far-future timer never
                // fires spuriously.
                () = &mut resync_debounce, if resync_pending && !self.native_bootstrap_pending() => {
                    resync_pending = false;
                    self.broadcast_resync(resync_reason);
                }

                Some(req) = self.consumer_attach_rx.recv(), if !self.native_bootstrap_pending() => {
                    self.handle_consumer_attach(req);
                }

                Some(req) = self.consumer_detach_rx.recv() => {
                    let ConsumerDetachRequest { client_id, reply } = req;
                    self.unregister_consumer(client_id);
                    trace!(?client_id, "consumer detached: per-consumer RenderState freed");
                    // phux-q0e.5: losing a consumer can raise the minimum
                    // desired interval (e.g. the fastest peer left), so
                    // re-evaluate the shared cadence.
                    Self::rearm_tick(&mut tick, &mut tick_interval, self.adaptive_tick_interval());
                    let _ = reply.send(());
                }

                // ADR-0018 / phux-q0e.4: inbound FRAME_ACK. Clears the
                // per-consumer dirty cache so the next tick re-diffs
                // against the just-acked reference. Loss tolerance: a
                // dropped ack just means the next tick re-emits a larger
                // diff against the same older reference — no
                // retransmit machinery here.
                Some(req) = self.consumer_ack_rx.recv() => {
                    let ConsumerAckRequest {
                        client_id,
                        stream_id,
                        bootstrap_id,
                        seq,
                    } = req;
                    // phux-q0e.5: a fresh RTT sample may shift the adaptive
                    // cadence. Rebuild the shared tick only when the new
                    // minimum-desired interval moves beyond the deadband, so
                    // a steady RTT does not churn the scheduler.
                    if self.on_generation_frame_ack(client_id, stream_id, bootstrap_id, seq) {
                        Self::rearm_tick(&mut tick, &mut tick_interval, self.adaptive_tick_interval());
                    }
                }

                // Semantic event subscription request. Register the subscriber
                // and begin broadcasting matching events to their outbound mailbox.
                Some(req) = self.subscribe_to_events_rx.recv() => {
                    self.subscribe_to_events(req);
                }

                // Semantic event unsubscription request. Remove the subscriber
                // from the broadcast list. Silent no-op if already unsubscribed.
                Some(req) = self.unsubscribe_from_events_rx.recv() => {
                    self.unsubscribe_from_events(&req);
                }

                // Supervisory control (ADR-0033): lease-change broadcasts and
                // process signals. The lease itself lives in `ServerState`; the
                // actor is the emitter (it owns the subscriber list + lifecycle)
                // and the signal deliverer (it owns the PTY child pid).
                Some(req) = self.control_rx.recv() => {
                    self.handle_control_request(req);
                }

                // State-sync tick driver (phux-q0e.3, phux-ia4, ADR-0018).
                // Iterates each attached consumer, diffs the live terminal
                // against that consumer's own reference grid, and pushes a
                // `TerminalOutput` frame onto its outbound mailbox whenever
                // `synthesize_against_reference` returns non-empty bytes.
                _ = tick.tick(), if !self.native_bootstrap_pending() => {
                    // phux-y2t: close an output burst with an `idle` event
                    // when no PTY output arrived since the previous tick.
                    // This bookkeeping is independent of the state-sync
                    // emitter gate, so headless watchers settle raw panes too.
                    self.maybe_emit_idle();
                    self.tick_emit();
                    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                    self.expire_native_cursors();
                }

                // Agent-state detector (ADR-0046). This interval is the SOLE
                // driver: PTY bytes deliberately do NOT wake it. A chatty
                // agent spewing megabytes must cost zero extra detector work
                // — the whole design is a periodic re-derivation, not a
                // reaction to output. The cadence is adaptive (500 ms while
                // unidentified, 300 ms once identified, 100 ms while
                // confirming a working -> idle transition) and is re-armed
                // through the existing `rearm_tick`, whose deadband keeps a
                // steady cadence from churning the scheduler.
                _ = detect_tick.tick(),
                    if self.agent_detect.is_some() && !self.native_bootstrap_pending() =>
                {
                    if let Some(next) = self.detect_tick() {
                        Self::rearm_tick(&mut detect_tick, &mut detect_interval, next);
                    }
                }

                () = tokio::task::yield_now(),
                    if self.native_bootstrap_pending() && !native_step_due =>
                {
                    native_step_due = true;
                }

                else => break,
            }
        }
    }

    /// One tick of the state-sync emission driver (phux-q0e.3, phux-ia4).
    ///
    /// Walks every attached consumer in turn. For each:
    ///
    /// 1. Call [`SnapshotSynthesizer::synthesize_against_reference`] using
    ///    the actor's shared synthesizer and the consumer's *own*
    ///    reference grid. The reference is per-consumer and independent of
    ///    the shared `Terminal` dirty bits, so every consumer on a shared
    ///    pane gets its own correct diff this tick — even though
    ///    libghostty's `RenderState::update` consumes the shared dirty
    ///    state on the first read (the phux-ia4 fix). Synthesis errors are
    ///    logged and that consumer is skipped for this tick (no kill: a
    ///    transient FFI error on one consumer must not poison the others).
    /// 2. If the body is empty, skip — the viewport is byte-identical to
    ///    that consumer's reference (steady state between writes).
    /// 3. Stamp the per-consumer monotonic `seq` (starting at `1`,
    ///    incrementing per emission) and ship a `TerminalOutput` frame
    ///    via the per-consumer outbound mailbox.
    ///
    /// Emit-once (phux-ia4): `synthesize_against_reference` advances the
    /// consumer's reference before returning a non-empty body, so a given
    /// change is emitted exactly once and an unchanged terminal produces no
    /// re-emission on the next tick. This is the v0.1 reliable-transport
    /// model (proto.md §8); the loss-tolerance re-diff property is a future
    /// lossy-transport concern (ADR-0018) and is not wired here.
    #[allow(
        clippy::too_many_lines,
        reason = "single cohesive per-tick emission: the length is inline \
                  safety rationale (permit reservation, emit-once, \
                  backpressure) that splitting would scatter and endanger"
    )]
    pub(super) fn tick_emit(&mut self) {
        // Per-tick observation span (hot path, so debug level: the default
        // `phux=info` filter leaves it disabled and effectively free —
        // `tracing` skips a disabled span without evaluating its fields).
        // The correlation fields a trace reader greps for to localize
        // server-side lag: how many consumers this tick must serve and
        // whether the grid is dirty. `consumer_count` is read before the
        // gate so the span is consistent on the gated-off / idle-skip
        // return paths too; `emitted` + `total_out_bytes` are recorded at
        // the end of a productive tick.
        let tick_span = tracing::debug_span!(
            "tick_emit",
            consumer_count = self.consumer_states.len(),
            dirty = self.terminal_dirty_since_tick,
            // Filled in at the end of a productive tick via `record`; declared
            // `Empty` so they exist on the span for later assignment.
            emitted = tracing::field::Empty,
            total_out_bytes = tracing::field::Empty,
        )
        .entered();

        // Emission gate (phux-0q8 / phux-3uv / phux-ia4 / phux-fseo). The tick
        // emits only for a *tick-managed* consumer — one that negotiated
        // `OutputMode::StateSync` (`state.wants_state_sync`), or any consumer
        // when the global test gate forces it; the runtime suppresses its
        // broadcast pump for exactly those (see `ConsumerAttachOutcome`). A
        // raw consumer is served by the pump, so the tick stays silent for it
        // to avoid double-painting. `force_all_consumers` is captured here so
        // the loop below reads it without re-borrowing `self` while it holds
        // `&mut self.consumer_states`.
        let force_all_consumers = self.consumer_tick_emits;
        if !force_all_consumers && !self.consumer_states.values().any(|s| s.wants_state_sync) {
            // No tick-managed consumer: nothing to emit (dirty flag untouched).
            return;
        }

        // Idle short-circuit (phux-4l0). The per-consumer reference diff
        // walks + renders every viewport row into a throwaway `Vec<u8>`
        // for every consumer, every tick — pure waste when nothing has
        // changed. Take and reset the "mutated since last tick" flag here;
        // if the terminal is unchanged AND no consumer is awaiting its
        // first emission, skip the entire per-consumer loop.
        //
        // Correctness: a `Clean` terminal cannot have diverged from any
        // consumer's last-emitted reference (the reference advanced to the
        // terminal state on the prior emit, and nothing has mutated the
        // terminal since), so skipping is sound. Two carve-outs suppress the
        // short-circuit even on a `Clean` terminal:
        //
        // - `needs_initial_emit` preserves the phux-ia4 multi-consumer
        //   guarantee: a consumer registered *after* the last write sits on a
        //   clean terminal yet has never had a synthesis pass, so it must be
        //   walked once even though the global flag is clear.
        // - `behind` preserves the backpressure retry: a consumer skipped on
        //   a prior tick because its mailbox was full has a reference behind
        //   the live grid. The grid can stay `Clean` indefinitely, so without
        //   this the held-back delta would never be retried once the client
        //   drains (the wave-hunt/server-lifecycle backpressure leak).
        let mutated = self.terminal_dirty_since_tick;
        self.terminal_dirty_since_tick = false;
        if !mutated
            && !self.consumer_states.values().any(|s| {
                // phux-v45.8: a loss-tolerant consumer with un-acked frames in
                // flight must keep being walked even on a Clean terminal, so its
                // retransmit timer can fire and re-diff a suspected-lost frame
                // against the acked reference. `pending_refs` is empty for the
                // reliable emit-once path, so this adds nothing there.
                s.needs_initial_emit || s.behind || !s.pending_refs.is_empty()
            })
        {
            return;
        }

        // Borrow the terminal + shared synthesizer once per tick. The
        // synthesizer's `RenderState`/iterators are reused across
        // consumers; the per-consumer state lives in each `reference`.
        let terminal = self.terminal.borrow();
        let mut synth = self.synth.borrow_mut();
        // phux-ahk.2: render the grid ONCE for this tick (the consumer-
        // independent snapshot + per-row cell render + cursor/mode FFI +
        // epilogue/screen-toggle precompute). Each consumer below then only
        // DIFFS against the shared result via `diff_consumer`, so a pane with
        // N state-sync consumers renders once, not N times.
        let (tick_cols, tick_rows, tick_live_cm) = match synth.prepare_tick(&terminal) {
            Ok(t) => t,
            Err(err) => {
                warn!(error = %err, "state-sync tick: prepare_tick failed; skipping tick");
                return;
            }
        };
        // Consumers whose outbound mailbox is `Closed` (receiver dropped)
        // are reaped after the loop so a missed detach (phux-ddg) does not
        // leave a dead `ConsumerReference` to be re-rendered forever.
        let mut closed: Vec<ClientId> = Vec::new();
        // Per-tick emission tally recorded onto the tick span on the way out
        // (frames actually shipped + their total byte volume) — the headline
        // "frame N for terminal T was Y bytes" reconstruction signal.
        let mut emitted: u64 = 0;
        let mut total_out_bytes: usize = 0;
        for (client_id, state) in &mut self.consumer_states {
            // phux-fseo: serve only tick-managed consumers. A raw consumer
            // sharing this pane is served by the broadcast pump; emitting here
            // too would double-paint it, so skip it (reference left untouched
            // for a later mode flip).
            if !force_all_consumers && !state.wants_state_sync {
                continue;
            }
            if !*state.live_gate.borrow() {
                continue;
            }
            // Captured before the `behind` reset below: whether a prior tick
            // held this consumer's delta back for a full mailbox. The
            // loss-tolerant emit gate treats a drained-after-backpressure
            // consumer as "has new content to ship" (phux-v45.8).
            let was_behind = state.behind;
            // This consumer is being serviced this tick; it no longer needs
            // a forced first pass.
            state.needs_initial_emit = false;
            // Reserve an outbound permit BEFORE synthesizing
            // (phux-wave-hunt/server-lifecycle). `synthesize_against_reference`
            // commits the per-consumer reference to the just-rendered grid
            // *before* it returns the bytes (emit-once, grid.rs), so once we
            // synthesize the delta is the only copy and the reference has
            // moved past it. If the send then failed `Full` we would drop the
            // delta and never re-emit it (the next tick diffs against the
            // already-advanced reference), silently losing content and
            // diverging the client mirror forever.
            //
            // Reserving first inverts the ordering: a `Full` mailbox means we
            // skip this consumer entirely this tick WITHOUT synthesizing, so
            // the reference (and `next_seq`) stay put and the delta is
            // re-diffed intact on the next tick once the client drains. A
            // `Closed` mailbox reaps the entry (phux-ddg self-heal). Only when
            // we hold a permit — which guarantees the subsequent send cannot
            // fail — do we synthesize, advance the reference, and ship.
            let permit = match state.outbound.try_reserve() {
                Ok(permit) => permit,
                Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {
                    // Backpressure: the consumer mailbox is wedged. Skip
                    // without advancing the reference so no content is lost;
                    // the next tick retries the same delta. Mark `behind` so
                    // the idle short-circuit keeps walking this consumer even
                    // if the grid goes `Clean` before the client drains — the
                    // retry must not depend on a fresh write. At debug so a
                    // stall is visible at the recommended `phux=debug` level.
                    state.behind = true;
                    debug!(
                        ?client_id,
                        wire_terminal_id = state.wire_terminal_id,
                        "state-sync tick: consumer mailbox full; skipping (reference held, retries next tick)",
                    );
                    continue;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => {
                    // The receiver is gone. A `ConsumerDetachRequest` may have
                    // been dropped (best-effort `try_send` on a full detach
                    // mailbox, runtime.rs) so `unregister_consumer` never ran.
                    // Self-heal: reap the entry now so we stop re-rendering a
                    // dead consumer every tick (phux-ddg).
                    debug!(
                        ?client_id,
                        wire_terminal_id = state.wire_terminal_id,
                        "state-sync tick: consumer mailbox closed; reaping entry",
                    );
                    closed.push(*client_id);
                    continue;
                }
            };
            // We hold a permit: the mailbox has room, so this consumer is
            // about to be fully serviced this tick (a delta ships, or the
            // diff is empty and the reference is already at the live grid).
            // Either way it is no longer behind.
            state.behind = false;
            // Per-consumer synthesis span (debug; the per-tick CPU sink —
            // its duration is the key server-side lag signal). Carries the
            // consumer correlation fields; the diff size lands in
            // `synthesize_against_reference`'s own child span.
            let _synth_span = tracing::debug_span!(
                "synthesize",
                ?client_id,
                wire_terminal_id = state.wire_terminal_id,
            )
            .entered();
            // diff_consumer is infallible (the fallible render happened once in
            // `prepare_tick` above); it returns this consumer's delta bytes.
            // phux-v45.8: a loss-tolerant consumer re-diffs against its
            // last-ACKED reference (which does NOT advance on emit), so a
            // dropped/un-acked frame self-heals — its rows still differ from the
            // acked reference on the next emission. The reliable-transport
            // default stays on the emit-once `diff_consumer` path (reference
            // advances on emit), byte-for-byte unchanged.
            let bytes = if state.loss_tolerant {
                synth.diff_against_base(tick_cols, tick_rows, tick_live_cm, &state.acked_reference)
            } else {
                synth
                    .diff_consumer(tick_cols, tick_rows, tick_live_cm, &mut state.reference)
                    .bytes
            };
            if bytes.is_empty() {
                // Byte-identical to this consumer's reference; nothing to
                // send this tick. The reserved permit drops unused. A closed
                // mailbox was already reaped by the `try_reserve` arm above,
                // so no extra liveness probe is needed here.
                continue;
            }
            // phux-v45.8 loss-tolerant emit gate. Because a loss-tolerant diff
            // is against the last-acked (not last-emitted) reference, it stays
            // non-empty every tick while a frame is un-acked. Only actually
            // (re)ship when there is genuinely new content this tick
            // (`mutated`), a drained-after-backpressure delta to flush
            // (`was_behind`), or a retransmit is due for a still-un-acked frame
            // (suspected loss). Otherwise hold: re-shipping the same cumulative
            // delta every tick would flood the leg. The reserved permit drops.
            if state.loss_tolerant {
                let now = tokio::time::Instant::now();
                let retransmit_due = !state.pending_refs.is_empty()
                    && state.emit_instants.values().next_back().is_none_or(|last| {
                        now.saturating_duration_since(*last)
                            >= tick::retransmit_timeout(state.rtt.smoothed())
                    });
                if !mutated && !was_behind && !retransmit_due {
                    continue;
                }
            }
            let seq = state.next_seq;
            let out_bytes = bytes.len();
            // Wrapping_add for paranoia; `u64` will not realistically
            // roll over at 33 Hz, but the existing `runtime.rs` pump
            // uses the same idiom and we match it.
            state.next_seq = state.next_seq.wrapping_add(1);
            let frame = FrameKind::TerminalOutput {
                terminal_id: phux_protocol::ids::TerminalId::local(state.wire_terminal_id),
                stream_id: state.stream_id,
                bootstrap_id: state.bootstrap_id,
                seq,
                bytes: bytes.into(),
            };
            // Infallible: we hold a reserved permit, so this cannot block,
            // drop, or fail. This preserves the actor's single-poll-budget
            // invariant (the tick arm never yields the loop) while keeping
            // emit-once consistent — a synthesized delta always ships.
            permit.send(Outbound::Frame(frame));
            // Stamp the emit instant for this seq so the matching FRAME_ACK
            // can be turned into an RTT sample (phux-q0e.5). Recorded only
            // for shipped frames — empty/skipped ticks have no round-trip to
            // measure. Pruned on ack, so the map stays as small as the
            // in-flight window.
            state.emit_instants.insert(seq, tokio::time::Instant::now());
            // Defensive bound: ack-pruning keeps this map tiny for a
            // well-behaved consumer, but one that opts into state sync and
            // never sends FRAME_ACK (or a transport that drops acks) would
            // otherwise grow it one entry per emitted tick without bound
            // (~50/s at the 20ms floor cadence). Evict the oldest (lowest-seq)
            // samples past the cap; an unacked sample this stale is already
            // useless for RTT, so dropping it costs nothing and bounds the
            // map to a few KB per consumer. See `MAX_EMIT_INSTANTS`.
            while state.emit_instants.len() > MAX_EMIT_INSTANTS {
                state.emit_instants.pop_first();
            }
            // phux-v45.8: for a loss-tolerant consumer, snapshot the grid state
            // this `seq` shipped so a later cumulative `FRAME_ACK` can advance
            // the acked reference to exactly it. Bounded the same way as
            // `emit_instants` (oldest-evicted past the cap) so a wedged leg
            // cannot grow it without bound. Empty/untouched on the emit-once
            // path.
            if state.loss_tolerant {
                let snapshot = synth.snapshot_tick_reference(tick_cols, tick_rows, tick_live_cm);
                state.pending_refs.insert(seq, snapshot);
                while state.pending_refs.len() > MAX_EMIT_INSTANTS {
                    state.pending_refs.pop_first();
                }
            }
            emitted += 1;
            total_out_bytes += out_bytes;
            trace!(
                ?client_id,
                wire_terminal_id = state.wire_terminal_id,
                seq,
                out_bytes,
                "state-sync tick: TERMINAL_OUTPUT emitted",
            );
        }
        drop(synth);
        drop(terminal);
        // Record the per-tick emission tally on the tick span so a reader
        // can reconstruct "tick served N consumers, shipped M frames /
        // B bytes" without re-deriving it from the per-consumer trace lines.
        tick_span.record("emitted", emitted);
        tick_span.record("total_out_bytes", total_out_bytes);
        for client_id in closed {
            self.consumer_states.remove(&client_id);
        }
    }
}
