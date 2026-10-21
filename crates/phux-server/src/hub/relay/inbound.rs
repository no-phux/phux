//! Dispatching one satellite frame and resolving its correlated replies and refusals.

use super::{
    CommandResult, DirectoryListingResult, ErrorCode, FrameKind, PathQueryResult, RelaySession,
    SpawnError, SpawnResult, debug, listing_refusal, path_refusal, warn,
};

impl RelaySession {
    /// Dispatch one frame from the satellite: resolve replies, re-tag and fan
    /// out streams.
    pub(crate) fn handle_inbound(&mut self, framed: &[u8]) -> Result<(), String> {
        let frame = FrameKind::decode_with_limits(framed, self.bootstrap_limits)
            .map_err(|err| format!("satellite {} sent an undecodable frame: {err:?}", self.host))?
            .0;
        if self.resolve_detach_reply(&frame)? {
            return Ok(());
        }
        match frame {
            FrameKind::CommandResult { request_id, result } => {
                self.resolve_pending(request_id, result);
            }
            FrameKind::ResourceSpawned { request_id, result } => {
                self.resolve_pending_spawn(request_id, result);
            }
            FrameKind::DirectoryListing { request_id, result } => {
                self.resolve_pending_listing(request_id, result);
            }
            FrameKind::PathResults { request_id, result } => {
                self.resolve_pending_path(request_id, result);
            }
            FrameKind::Error {
                request_id: Some(request_id),
                code,
                message,
            } => self.resolve_correlated_error(request_id, code, message),
            // An uncorrelated ERROR (an input the satellite could not write,
            // a frame it does not support) names no relayed request and no
            // consumer, so it is logged and the link stays up.
            FrameKind::Error {
                request_id: None,
                code,
                message,
            } => warn!(
                satellite = %self.host,
                ?code,
                %message,
                "satellite sent an uncorrelated error; keeping the link"
            ),
            FrameKind::Event { .. }
            | FrameKind::ResourceOutput { .. }
            | FrameKind::BootstrapBegin { .. }
            | FrameKind::BootstrapChunk { .. }
            | FrameKind::BootstrapReady { .. }
            | FrameKind::HistoryPage { .. }
            | FrameKind::BootstrapTombstone { .. }
            | FrameKind::HistoryTombstone { .. }
            | FrameKind::HistoryRejected { .. } => self.relay_stream_frame(frame)?,
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => self.relay_terminal_closed(&terminal_id, exit_status, reason, signal),
            FrameKind::Bell { terminal_id } => self.relay_bell(&terminal_id),
            FrameKind::MetadataChanged {
                scope, key, value, ..
            } => self.apply_mirrored_metadata(&scope, &key, value),
            FrameKind::MetadataValue { request_id, value } => {
                self.apply_mirrored_value(request_id, value);
            }
            // Other metadata frames are not part of the mirror; absorb them.
            FrameKind::GetMetadata { .. }
            | FrameKind::SetMetadata { .. }
            | FrameKind::DeleteMetadata { .. }
            | FrameKind::ListMetadata { .. }
            | FrameKind::SubscribeMetadata { .. }
            | FrameKind::MetadataKeys { .. } => {}
            other => {
                return Err(format!(
                    "satellite {} sent a direction-invalid frame after HELLO_OK: {other:?}",
                    self.host
                ));
            }
        }
        Ok(())
    }

    /// The old upstream generation stays fenced until a successful detach
    /// receipt; a refusal cannot authorize a fresh stream.
    pub(super) fn resolve_detach_reply(&mut self, frame: &FrameKind) -> Result<bool, String> {
        let (request_id, succeeded) = match frame {
            FrameKind::CommandResult { request_id, result } => {
                (*request_id, matches!(result, CommandResult::Ok))
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } => (*request_id, false),
            _ => return Ok(false),
        };
        let Some(pending) = self.pending_detaches.remove(&request_id) else {
            return Ok(false);
        };
        let terminal = pending.terminal;
        if !succeeded {
            return Err(format!(
                "satellite {} refused upstream detach barrier for terminal {terminal}",
                self.host
            ));
        }
        self.retire_bootstrap_flow(terminal);
        Ok(true)
    }

    /// Resolve a correlated `ERROR` against whichever request holds the id
    /// (a satellite may answer a spawn with a generic error).
    pub(super) fn resolve_correlated_error(
        &mut self,
        request_id: u32,
        code: ErrorCode,
        message: String,
    ) {
        if self.pending_mirror_gets.remove(&request_id).is_some() {
            debug!(
                satellite = %self.host,
                request_id,
                ?code,
                %message,
                "satellite refused a mirrored metadata read; leaving the key unset"
            );
            return;
        }
        // A code this build does not know (`UNREADABLE_REPLY` included)
        // says the satellite answered without saying what happened: the
        // request settles as an unknown outcome, never as a refusal a retry
        // could repeat.
        let unknown_outcome = matches!(code, ErrorCode::Unknown(_));
        let verdict = if unknown_outcome {
            "could not read the satellite's answer to"
        } else {
            "the satellite refused"
        };
        if self.pending_spawns.contains_key(&request_id) {
            let error = if unknown_outcome {
                SpawnError::Unknown(phux_protocol::wire::UnreadableResult::from_wire(Vec::new()))
            } else {
                SpawnError::SpawnFailed(format!("satellite refused the spawn: {code:?}: {message}"))
            };
            self.resolve_pending_spawn(request_id, SpawnResult::Err(error));
            return;
        }
        if let Some(pending) = self.pending_listings.remove(&request_id) {
            let refusal = listing_refusal(
                &pending.path,
                format!(
                    "{verdict} the listing on {}: {code:?}: {message}",
                    self.host
                ),
            );
            let _ = pending.reply.send(Err(refusal));
            return;
        }
        if let Some(pending) = self.pending_paths.remove(&request_id) {
            let _ = pending.reply.send(Err(path_refusal(
                &pending.root,
                format!(
                    "{verdict} the path query on {}: {code:?}: {message}",
                    self.host
                ),
            )));
            return;
        }
        self.resolve_pending(request_id, CommandResult::Error { code, message });
    }

    /// Resolve a listing id to its waiting consumer; paths pass through.
    pub(super) fn resolve_pending_listing(
        &mut self,
        request_id: u32,
        result: DirectoryListingResult,
    ) {
        let Some(pending) = self.pending_listings.remove(&request_id) else {
            debug!(
                satellite = %self.host,
                request_id,
                "satellite directory listing with no pending request; dropping"
            );
            return;
        };
        // A dropped receiver (consumer timed out / disconnected) is fine.
        let _ = pending.reply.send(result);
    }

    pub(super) fn resolve_pending_path(&mut self, request_id: u32, result: PathQueryResult) {
        if let Some(pending) = self.pending_paths.remove(&request_id) {
            let _ = pending.reply.send(result);
        } else {
            debug!(satellite = %self.host, request_id, "satellite path results with no pending request; dropping");
        }
    }
}
