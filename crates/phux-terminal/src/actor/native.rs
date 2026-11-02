//! Native-engine request handling for [`TerminalActor`]: bootstrap
//! capture, publication handoff, history paging, cursor-owner
//! bookkeeping, and live-output replay buffering.

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use super::{
    Bytes, CanonicalTerminal, FrameKind, HashSet, MAX_NATIVE_HISTORY_CLIENTS,
    MAX_NATIVE_REPLAY_BYTES, NATIVE_CAPTURE_LIFETIME, NATIVE_HISTORY_TTL, NativeBootstrapReply,
    NativeBootstrapRequest, NativeCursorKey, NativeCursorOwner, NativeHistoryReply,
    NativeHistoryRequest, NativePublicationGeneration, NativePublicationReply,
    NativePublicationRequest, PaneOutput, PendingNativeBootstrap, PendingNativeHistory, VecDeque,
    native_step_bytes, reserve_native_bytes, warn,
};
use super::{NativeActorRequest, TerminalActor};

/// Starting width of the native checkpoint scratch buffer (one standard
/// page); it grows to the engine's reported `required_bytes`.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) const INITIAL_NATIVE_SCRATCH_BYTES: usize = 64 * 1024;

/// Scratch width for a fresh bootstrap: one page, not the 64 MiB staging
/// budget or the engine's `u32::MAX` record bound.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) const fn initial_native_scratch_bytes(ceiling: usize) -> usize {
    if ceiling < INITIAL_NATIVE_SCRATCH_BYTES {
        ceiling
    } else {
        INITIAL_NATIVE_SCRATCH_BYTES
    }
}

/// Widen `scratch` to `required_bytes` without aborting on allocation
/// failure.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn grow_native_scratch(
    scratch: &mut Vec<u8>,
    required_bytes: usize,
) -> Result<(), crate::native_state::NativeStateError> {
    let mut grown = reserve_native_bytes(required_bytes)?;
    grown.resize(required_bytes, 0);
    *scratch = grown;
    Ok(())
}

impl TerminalActor {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn handle_native_bootstrap(&mut self, req: NativeBootstrapRequest) {
        let owner = req.owner;
        let stream_id = req.stream_id;
        self.invalidate_native_owner(
            owner,
            stream_id,
            phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
        );
        if let Some(pending) = self.pending_native_bootstrap.as_mut() {
            if pending.limits == req.limits
                && pending.capture_bytes
                    == req
                        .max_bytes
                        .saturating_sub(phux_protocol::wire::frame::MAX_HISTORY_CURSOR_BYTES)
                && pending.max_chunks
                    == req
                        .max_frames
                        .saturating_sub(crate::native_state::NATIVE_BOOTSTRAP_FRAMING_FRAMES)
                && pending.waiters.len() < MAX_NATIVE_HISTORY_CLIENTS
            {
                pending
                    .waiters
                    .retain(|waiter| keeps_native_bootstrap_waiter(waiter, owner, Some(stream_id)));
                pending.waiters.push(req);
                return;
            }
            self.native_bootstrap_backlog
                .retain(|waiter| keeps_native_bootstrap_waiter(waiter, owner, Some(stream_id)));
            self.native_bootstrap_backlog.push_back(req);
            return;
        }
        self.start_native_bootstrap(req);
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn start_native_bootstrap(&mut self, req: NativeBootstrapRequest) {
        let _timer = phux_resource::perf::BOOTSTRAP_NATIVE_BEGIN.timer();
        let limits = req.limits;
        let chunk_bytes = match usize::try_from(limits.max_chunk_bytes()) {
            Ok(size) if size != 0 => size.min(req.max_bytes),
            _ => {
                let _ = req
                    .reply
                    .send(Err(crate::native_state::NativeStateError::LimitExceeded));
                return;
            }
        };
        let Some(max_chunks) = req
            .max_frames
            .checked_sub(crate::native_state::NATIVE_BOOTSTRAP_FRAMING_FRAMES)
        else {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            return;
        };
        let Some(capture_bytes) = req
            .max_bytes
            .checked_sub(phux_protocol::wire::frame::MAX_HISTORY_CURSOR_BYTES)
        else {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            return;
        };
        if max_chunks == 0 || capture_bytes == 0 {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            return;
        }
        let capture = {
            let mut host = self.terminal.borrow_mut();
            match host.native_manager().and_then(|manager| {
                manager.begin_generation_capture(limits, capture_bytes, max_chunks)
            }) {
                Ok(capture) => capture,
                Err(error) => {
                    let _ = req.reply.send(Err(error));
                    return;
                }
            }
        };
        // The scratch holds one record at a time: start at one page and let
        // the engine's `required_bytes` grow it, under the negotiated ceiling.
        let scratch_ceiling = match native_step_bytes(capture_bytes, 0, capture.max_record_bytes())
        {
            Ok(bytes) => bytes,
            Err(error) => {
                if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
                    manager.abort_generation_capture(capture);
                }
                let _ = req.reply.send(Err(error));
                return;
            }
        };
        let scratch_bytes = initial_native_scratch_bytes(scratch_ceiling);
        let mut scratch = match reserve_native_bytes(scratch_bytes) {
            Ok(scratch) => scratch,
            Err(error) => {
                if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
                    manager.abort_generation_capture(capture);
                }
                let _ = req.reply.send(Err(error));
                return;
            }
        };
        scratch.resize(scratch_bytes, 0);
        self.pending_native_bootstrap = Some(PendingNativeBootstrap {
            capture: Some(capture),
            waiters: vec![req],
            records: Vec::new(),
            retained_bytes: 0,
            capture_bytes,
            scratch,
            max_chunks,
            chunk_bytes,
            base_seq: self.core.seq(),
            chunk_count: 0,
            limits,
            replay: VecDeque::new(),
            replay_bytes: 0,
            started_at: tokio::time::Instant::now(),
        });
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[allow(
        clippy::too_many_lines,
        reason = "one cooperative step of the bootstrap state machine: each arm advances the same `pending` value to the next state and they share the buffer-growth retry, so splitting on an arm boundary would thread that scratch buffer and the pending record through a second signature for no reader benefit"
    )]
    pub(super) fn step_native_bootstrap(&mut self) {
        let Some(mut pending) = self.pending_native_bootstrap.take() else {
            return;
        };
        let _timer = phux_resource::perf::BOOTSTRAP_NATIVE_STEP.timer();
        if pending.started_at.elapsed() > NATIVE_CAPTURE_LIFETIME {
            self.fail_native_bootstrap(
                pending,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        }
        let step_ceiling = match native_step_bytes(
            pending.capture_bytes,
            pending.retained_bytes,
            pending.capture.as_ref().map_or(
                0,
                crate::native_state::NativeManagedCapture::max_record_bytes,
            ),
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.fail_native_bootstrap(pending, error);
                return;
            }
        };
        let scratch_len = pending.scratch.len();
        let step_bytes = step_ceiling.min(scratch_len);
        let (ready, record_len) = match pending
            .capture
            .as_mut()
            .ok_or(crate::native_state::NativeStateError::InvalidState)
            .and_then(|capture| capture.step(&mut pending.scratch[..step_bytes]))
        {
            Ok(event) => (
                matches!(
                    event.kind,
                    crate::native_state::NativeCheckpointChunkKind::Ready
                ),
                event.bytes.len(),
            ),
            // Grow to the reported width and retry next turn (`next` does not
            // consume a record on `OutOfSpace`).
            Err(crate::native_state::NativeStateError::OutOfSpace {
                required_bytes,
                required_rows: 0,
            }) if required_bytes > scratch_len && required_bytes <= step_ceiling => {
                match grow_native_scratch(&mut pending.scratch, required_bytes) {
                    Ok(()) => self.pending_native_bootstrap = Some(pending),
                    Err(error) => self.fail_native_bootstrap(pending, error),
                }
                return;
            }
            Err(error) => {
                self.fail_native_bootstrap(pending, error);
                return;
            }
        };
        let fragments = record_len.div_ceil(pending.chunk_bytes);
        let Some(chunk_count) = pending.chunk_count.checked_add(fragments) else {
            self.fail_native_bootstrap(
                pending,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        };
        if chunk_count > pending.max_chunks {
            self.fail_native_bootstrap(
                pending,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        }
        let mut payload = match reserve_native_bytes(record_len) {
            Ok(payload) => payload,
            Err(error) => {
                self.fail_native_bootstrap(pending, error);
                return;
            }
        };
        payload.extend_from_slice(&pending.scratch[..record_len]);
        let Some(retained_bytes) = pending
            .retained_bytes
            .checked_add(payload.capacity())
            .filter(|bytes| *bytes <= pending.capture_bytes)
        else {
            self.fail_native_bootstrap(
                pending,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        };
        pending.records.push(Bytes::from(payload));
        pending.retained_bytes = retained_bytes;
        pending.chunk_count = chunk_count;
        if !ready {
            self.pending_native_bootstrap = Some(pending);
            return;
        }

        let completed = {
            let Some(capture) = pending.capture.take() else {
                self.fail_native_bootstrap(
                    pending,
                    crate::native_state::NativeStateError::InvalidState,
                );
                return;
            };
            let mut host = self.terminal.borrow_mut();
            match &mut *host {
                CanonicalTerminal::Native(manager) => manager.finish_generation_capture(capture),
                CanonicalTerminal::Plain(_) => {
                    Err(crate::native_state::NativeStateError::InvalidState)
                }
            }
        };
        let (cursor, seed) = match completed {
            Ok(completed) => completed,
            Err(error) => {
                self.apply_native_replay(&pending.replay);
                for waiter in pending.waiters {
                    let _ = waiter.reply.send(Err(error));
                }
                self.start_next_native_bootstrap();
                return;
            }
        };
        self.apply_native_replay(&pending.replay);
        self.finish_native_bootstrap(pending, cursor, seed);
        self.start_next_native_bootstrap();
    }

    pub(super) fn handle_native_actor_request(&mut self, req: NativeActorRequest) {
        match req {
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::Bootstrap(req) => self.handle_native_bootstrap(req),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::Publication(req) => self.handle_native_publication(req),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::History(req) => self.handle_native_history(req),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::Release(req) => self.release_native_owner(req.owner),
            #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
            NativeActorRequest::Disabled => unreachable!("disabled native receiver"),
        }
    }

    pub(super) const fn native_bootstrap_pending(&self) -> bool {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            self.pending_native_bootstrap.is_some()
        }
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        {
            false
        }
    }

    pub(super) const fn native_work_pending(&self) -> bool {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            self.pending_native_bootstrap.is_some() || self.pending_native_history.is_some()
        }
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        {
            false
        }
    }

    pub(super) fn cooperative_native_step(&mut self) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if self.pending_native_bootstrap.is_some() {
            self.step_native_bootstrap();
        } else {
            self.step_native_history();
        }
    }
}

mod cursors;
mod finish;
mod history;

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use cursors::*;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use finish::*;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use history::*;
