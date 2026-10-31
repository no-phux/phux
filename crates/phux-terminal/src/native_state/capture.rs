use super::{
    CaptureEvent, Digest, GhosttyTerminal, NativeCheckpointChunk, NativeCheckpointChunkKind,
    NativeGenerationSeed, NativeManagedCapture, NativeManagedCaptureFailure, NativeStateError,
    OpaqueHistoryCursor, PROGRESSIVE_CHECKPOINT_VERSION,
};

impl NativeManagedCapture<'_> {
    pub(crate) const fn max_record_bytes(&self) -> usize {
        self.max_record_bytes
    }

    pub(crate) fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        if self.ready {
            return Err(NativeStateError::InvalidState);
        }
        let event = self.capture.next(buffer)?;
        self.digest.update(&buffer[..event.written()]);
        let kind = match event {
            CaptureEvent::Ready { .. } => {
                self.ready = true;
                NativeCheckpointChunkKind::Ready
            }
            CaptureEvent::Record { .. } => NativeCheckpointChunkKind::Record,
            _ => return Err(NativeStateError::InvalidState),
        };
        Ok(NativeCheckpointChunk {
            kind,
            codec_version: PROGRESSIVE_CHECKPOINT_VERSION,
            bytes: &buffer[..event.written()],
        })
    }

    pub(super) fn detach_generation_ready(
        self,
    ) -> Result<
        (
            GhosttyTerminal<'static, 'static>,
            OpaqueHistoryCursor,
            NativeGenerationSeed,
        ),
        NativeManagedCaptureFailure,
    > {
        if !self.ready {
            return Err(NativeManagedCaptureFailure {
                error: NativeStateError::InvalidState,
                terminal: self.capture.into_terminal(),
            });
        }
        let mut digest = self.digest;
        digest.update(self.generation.to_le_bytes());
        let cursor = digest.finalize().into();
        // O(1): the engine registers a history lease and encodes nothing.
        // Each HISTORY_REQUEST later encodes one page from the live cut.
        let (terminal, capture) =
            self.capture
                .detach()
                .map_err(|failure| NativeManagedCaptureFailure {
                    error: failure.error.into(),
                    terminal: failure.terminal,
                })?;
        Ok((
            terminal,
            cursor,
            NativeGenerationSeed {
                capture,
                bounds: self.bounds,
            },
        ))
    }

    pub(crate) fn into_terminal(self) -> GhosttyTerminal<'static, 'static> {
        self.capture.into_terminal()
    }
}
