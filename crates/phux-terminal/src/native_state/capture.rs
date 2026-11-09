use super::{
    Digest, GhosttyTerminal, NativeCheckpointChunk, NativeCheckpointChunkKind,
    NativeGenerationSeed, NativeManagedCapture, NativeManagedCaptureFailure, NativeStateError,
    OpaqueHistoryCursor, PROGRESSIVE_CHECKPOINT_VERSION,
};

impl NativeManagedCapture {
    pub(crate) const fn max_record_bytes(&self) -> usize {
        self.max_record_bytes
    }

    pub(crate) fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        let range = self
            .snapshot
            .prefix_records
            .get(self.next_prefix_record)
            .ok_or(NativeStateError::InvalidState)?;
        let bytes = &self.snapshot.bytes[range.clone()];
        if buffer.len() < bytes.len() {
            return Err(NativeStateError::OutOfSpace {
                required_bytes: bytes.len(),
                required_rows: 0,
            });
        }
        buffer[..bytes.len()].copy_from_slice(bytes);
        self.digest.update(bytes);
        self.next_prefix_record += 1;
        self.ready = self.next_prefix_record == self.snapshot.prefix_records.len();
        Ok(NativeCheckpointChunk {
            kind: if self.ready {
                NativeCheckpointChunkKind::Ready
            } else {
                NativeCheckpointChunkKind::Record
            },
            codec_version: PROGRESSIVE_CHECKPOINT_VERSION,
            bytes: &buffer[..bytes.len()],
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
                terminal: self.terminal,
            });
        }
        let mut digest = self.digest;
        digest.update(self.generation.to_le_bytes());
        let cursor = digest.finalize().into();
        Ok((
            self.terminal,
            cursor,
            NativeGenerationSeed {
                bytes: self.snapshot.bytes,
                records: self.snapshot.history,
                bounds: self.bounds,
            },
        ))
    }

    pub(crate) fn into_terminal(self) -> GhosttyTerminal<'static, 'static> {
        self.terminal
    }
}
