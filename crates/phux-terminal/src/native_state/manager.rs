use super::{
    Arc, BootstrapLimits, CONTINUATION_LIMIT, CachedNativeHistoryRecord, Digest, EncodedSnapshot,
    GhosttyTerminal, HashMap, NativeCheckpointGeneration, NativeGenerationBounds,
    NativeGenerationCharge, NativeGenerationSeed, NativeManagedCapture, NativeManagerInitFailure,
    NativeRecordTable, NativeStateError, NativeTerminalManager, OpaqueHistoryCursor, Ordering,
    Sha256, SnapshotCaptureLimits, official_snapshot_available, requested_record_window,
};

#[cfg(test)]
use super::{MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS};

impl NativeTerminalManager {
    pub(crate) fn new(
        mut terminal: GhosttyTerminal<'static, 'static>,
        capacity: usize,
    ) -> Result<Self, NativeManagerInitFailure> {
        if !official_snapshot_available() {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::UnsupportedFeature,
                terminal,
            });
        }
        if capacity == 0 {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::LimitExceeded,
                terminal,
            });
        }
        if terminal
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .is_err()
        {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::ContinuationUnavailable,
                terminal,
            });
        }
        let mut generations = HashMap::new();
        let mut retired_generation_charges = Vec::new();
        if generations.try_reserve(capacity).is_err()
            || retired_generation_charges
                .try_reserve_exact(capacity)
                .is_err()
        {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::OutOfMemory,
                terminal,
            });
        }
        Ok(Self {
            terminal: Some(terminal),
            generations,
            retired_generation_charges,
            capacity,
            capture_active: false,
            next_generation: 1,
        })
    }

    /// The canonical terminal, or `None` while a capture holds it. Readers
    /// degrade (skip a paint, refuse a read) rather than abort the server
    /// holding every session.
    pub(crate) const fn try_terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        self.terminal.as_ref()
    }

    /// Apply VT bytes. The actor queues live output during a capture, so the
    /// `None` arm should be unreachable; it drops the write rather than
    /// abort (a resync repairs it).
    pub(crate) fn vt_write(&mut self, bytes: &[u8]) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.vt_write(bytes);
        } else {
            tracing::error!(
                bytes = bytes.len(),
                "vt_write while the canonical terminal is out on a prefix capture; \
                 dropping the write (the caller should have deferred it)"
            );
        }
    }

    pub(crate) fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        if let Some(terminal) = self.terminal.as_ref()
            && (terminal.cols()? != cols || terminal.rows()? != rows)
        {
            self.retire_all_generations();
        }
        self.terminal.as_mut().map_or_else(
            || {
                // Resize arms are gated during captures, so this is a caller
                // bug; refuse rather than abort (the request is re-applied).
                tracing::error!(
                    "resize while the canonical terminal is out on a prefix capture; refusing it"
                );
                Err(libghostty_vt::error::Error::InvalidValue)
            },
            |terminal| terminal.resize(cols, rows, cell_width_px, cell_height_px),
        )
    }

    #[cfg(test)]
    pub(crate) fn capture(
        &mut self,
        limits: BootstrapLimits,
    ) -> Result<NativeManagedCapture, NativeStateError> {
        self.begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
    }

    #[cfg(test)]
    pub(crate) fn capture_generation_bounded(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture, NativeStateError> {
        self.begin_generation_capture(limits, max_prefix_bytes, max_prefix_chunks)
    }

    pub(crate) fn begin_generation_capture(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture, NativeStateError> {
        if self.capture_active {
            return Err(NativeStateError::InvalidState);
        }
        if self.retained_generation_count()? >= self.capacity {
            return Err(NativeStateError::LimitExceeded);
        }
        let bounds =
            NativeGenerationBounds::for_capture(limits, max_prefix_bytes, max_prefix_chunks)?;
        let generation = self.next_generation;
        let next_generation = generation
            .checked_add(1)
            .ok_or(NativeStateError::LimitExceeded)?;
        let mut terminal = self.terminal.take().ok_or(NativeStateError::InvalidState)?;
        let snapshot = match EncodedSnapshot::capture(
            &mut terminal,
            SnapshotCaptureLimits {
                max_bytes: bounds.max_total_bytes,
                max_record_bytes: bounds.max_record_bytes,
                max_records: bounds.max_records,
                max_rows: bounds.max_rows,
            },
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.terminal = Some(terminal);
                return Err(error.into());
            }
        };
        self.next_generation = next_generation;
        self.capture_active = true;
        Ok(NativeManagedCapture {
            terminal,
            snapshot,
            next_prefix_record: 0,
            digest: Sha256::new(),
            generation,
            max_record_bytes: bounds.max_record_bytes,
            bounds,
            ready: false,
        })
    }

    pub(crate) fn finish_generation_capture(
        &mut self,
        capture: NativeManagedCapture,
    ) -> Result<(OpaqueHistoryCursor, NativeGenerationSeed), NativeStateError> {
        if !self.capture_active {
            return Err(NativeStateError::InvalidState);
        }
        match capture.detach_generation_ready() {
            Ok((terminal, cursor, seed)) => {
                self.terminal = Some(terminal);
                self.capture_active = false;
                Ok((cursor, seed))
            }
            Err(failure) => {
                self.terminal = Some(failure.terminal);
                self.capture_active = false;
                Err(failure.error)
            }
        }
    }

    pub(crate) fn abort_generation_capture(&mut self, capture: NativeManagedCapture) {
        self.terminal = Some(capture.into_terminal());
        self.capture_active = false;
    }

    pub(crate) fn has_generation(&self, cursor: &OpaqueHistoryCursor) -> bool {
        self.generations.contains_key(cursor)
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "seed is the owned generation payload"
    )]
    pub(crate) fn install_generation(
        &mut self,
        cursor: OpaqueHistoryCursor,
        seed: NativeGenerationSeed,
        bounds: NativeGenerationBounds,
        reserved_bytes: usize,
    ) -> Result<(), NativeStateError> {
        let required_reserved_bytes = bounds.required_reserved_bytes()?;
        if self.retained_generation_count()? >= self.capacity
            || self.generations.contains_key(&cursor)
            || bounds != seed.bounds
            || reserved_bytes < required_reserved_bytes
        {
            return Err(NativeStateError::LimitExceeded);
        }
        let charge = Arc::new(NativeGenerationCharge::default());
        let records = NativeRecordTable::from_snapshot(seed.bytes, seed.records, bounds, &charge)?;
        self.generations
            .try_reserve(1)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        self.generations.insert(
            cursor,
            NativeCheckpointGeneration {
                records,
                owners: 1,
                charge,
            },
        );
        Ok(())
    }

    pub(crate) fn retain_generation(
        &mut self,
        cursor: &OpaqueHistoryCursor,
    ) -> Result<(), NativeStateError> {
        let generation = self
            .generations
            .get_mut(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        generation.owners = generation
            .owners
            .checked_add(1)
            .ok_or(NativeStateError::LimitExceeded)?;
        Ok(())
    }

    pub(crate) fn history_record_at(
        &self,
        cursor: &OpaqueHistoryCursor,
        index: usize,
        requested_max_bytes: u32,
        requested_max_rows: u32,
    ) -> Result<CachedNativeHistoryRecord, NativeStateError> {
        let (requested_bytes, requested_rows) =
            requested_record_window(requested_max_bytes, requested_max_rows)?;
        let generation = self
            .generations
            .get(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        let record = generation
            .records
            .get(index)
            .ok_or(NativeStateError::InvalidHandle)?;
        record.for_request(requested_bytes, requested_rows)
    }

    pub(crate) fn release_generation(
        &mut self,
        cursor: &OpaqueHistoryCursor,
    ) -> Result<(), NativeStateError> {
        let remove = {
            let generation = self
                .generations
                .get_mut(cursor)
                .ok_or(NativeStateError::InvalidHandle)?;
            generation.owners = generation
                .owners
                .checked_sub(1)
                .ok_or(NativeStateError::InvalidHandle)?;
            generation.owners == 0
        };
        if remove {
            let generation = self
                .generations
                .remove(cursor)
                .ok_or(NativeStateError::InvalidHandle)?;
            if let Some(charge) = Self::released_generation_charge(generation) {
                self.retired_generation_charges.push(charge);
            }
        }
        Ok(())
    }

    pub(super) fn retire_all_generations(&mut self) {
        let generations = std::mem::take(&mut self.generations);
        for (_, generation) in generations {
            if let Some(charge) = Self::released_generation_charge(generation) {
                self.retired_generation_charges.push(charge);
            }
        }
        self.reap_retired_generation_charges();
    }

    pub(super) fn released_generation_charge(
        generation: NativeCheckpointGeneration,
    ) -> Option<Arc<NativeGenerationCharge>> {
        let charge = Arc::clone(&generation.charge);
        drop(generation);
        (charge.live_payloads.load(Ordering::Acquire) != 0).then_some(charge)
    }

    pub(super) fn reap_retired_generation_charges(&mut self) {
        self.retired_generation_charges
            .retain(|charge| charge.live_payloads.load(Ordering::Acquire) != 0);
    }

    pub(super) fn retained_generation_count(&mut self) -> Result<usize, NativeStateError> {
        self.reap_retired_generation_charges();
        self.generations
            .len()
            .checked_add(self.retired_generation_charges.len())
            .ok_or(NativeStateError::LimitExceeded)
    }
}
