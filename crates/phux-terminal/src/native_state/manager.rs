use super::{
    Arc, BootstrapLimits, Bytes, CONTINUATION_LIMIT, CachedNativeHistoryRecord, CaptureEvent,
    CaptureOptions, ChargedNativePayload, Digest, GhosttyError, GhosttyTerminal, HashMap,
    HistoryProbe, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS, NativeCheckpointGeneration,
    NativeGenerationBounds, NativeGenerationCharge, NativeGenerationSeed, NativeManagedCapture,
    NativeManagerInitFailure, NativeRecordTable, NativeStateError, NativeTerminalManager,
    OpaqueHistoryCursor, Ordering, PhantomData, Sha256, invalidation_error,
    official_snapshot_available, requested_record_window,
};

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
    #[allow(
        dead_code,
        reason = "kept for capture-budget tests that are not yet ported"
    )]
    pub(crate) fn capture(
        &mut self,
        limits: BootstrapLimits,
    ) -> Result<NativeManagedCapture<'static>, NativeStateError> {
        self.begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
    }

    #[cfg(test)]
    pub(crate) fn capture_generation_bounded(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture<'static>, NativeStateError> {
        self.begin_generation_capture(limits, max_prefix_bytes, max_prefix_chunks)
    }

    pub(crate) fn begin_generation_capture(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture<'static>, NativeStateError> {
        if self.capture_active || max_prefix_chunks == 0 || max_prefix_bytes == 0 {
            return Err(NativeStateError::InvalidState);
        }
        let max_record_bytes = usize::try_from(limits.max_history_page_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .min(max_prefix_bytes)
            .max(1);
        let max_records = max_prefix_chunks.min(MAX_NATIVE_PREFIX_CHUNKS);
        let generation = self.next_generation;
        let next_generation = generation
            .checked_add(1)
            .ok_or(NativeStateError::LimitExceeded)?;
        let terminal = self.terminal.take().ok_or(NativeStateError::InvalidState)?;
        let capture = match terminal.into_snapshot_capture(CaptureOptions {
            max_record_bytes,
            max_pages: max_records,
        }) {
            Ok(capture) => capture,
            Err(failure) => {
                self.terminal = Some(failure.terminal);
                return Err(failure.error.into());
            }
        };
        self.next_generation = next_generation;
        self.capture_active = true;
        Ok(NativeManagedCapture {
            capture,
            digest: Sha256::new(),
            generation,
            max_record_bytes,
            bounds: NativeGenerationBounds {
                max_record_bytes,
                max_rows: usize::try_from(phux_protocol::MAX_HISTORY_PAGE_ROWS)
                    .map_err(|_| NativeStateError::LimitExceeded)?,
                max_records: max_records.saturating_add(1),
                max_total_bytes: MAX_NATIVE_PREFIX_BYTES,
            },
            ready: false,
            _marker: PhantomData,
        })
    }

    pub(crate) fn finish_generation_capture(
        &mut self,
        capture: NativeManagedCapture<'static>,
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

    pub(crate) fn abort_generation_capture(&mut self, capture: NativeManagedCapture<'static>) {
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
        let records = NativeRecordTable::new(bounds.max_records)?;
        self.generations
            .try_reserve(1)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        self.generations.insert(
            cursor,
            NativeCheckpointGeneration {
                records,
                capture: Some(seed.capture),
                failure: None,
                scratch: Vec::new(),
                pending: Vec::new(),
                retained_bytes: 0,
                bounds,
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
        &mut self,
        cursor: &OpaqueHistoryCursor,
        index: usize,
        requested_max_bytes: u32,
        requested_max_rows: u32,
    ) -> Result<CachedNativeHistoryRecord, NativeStateError> {
        let (requested_bytes, requested_rows) =
            requested_record_window(requested_max_bytes, requested_max_rows)?;
        self.capture_history_through(cursor, index)?;
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

    pub(super) fn capture_history_through(
        &mut self,
        cursor: &OpaqueHistoryCursor,
        index: usize,
    ) -> Result<(), NativeStateError> {
        {
            let generation = self
                .generations
                .get(cursor)
                .ok_or(NativeStateError::InvalidHandle)?;
            if generation.records.get(index).is_some() {
                return Ok(());
            }
            if let Some(failure) = generation.failure {
                return Err(failure);
            }
            if generation.capture.is_none() {
                return Err(NativeStateError::InvalidHandle);
            }
        }
        let terminal = self.terminal.as_mut().ok_or(NativeStateError::ImportBusy)?;
        let generation = self
            .generations
            .get_mut(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        Self::capture_one_history_step(terminal, generation)?;
        if generation.records.get(index).is_none() {
            if let Some(failure) = generation.failure {
                return Err(failure);
            }
            return Err(NativeStateError::ImportBusy);
        }
        Ok(())
    }

    pub(super) fn capture_one_history_step(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        generation: &mut NativeCheckpointGeneration,
    ) -> Result<(), NativeStateError> {
        let required = match Self::probe_history_capture(terminal, generation)? {
            HistoryProbe::Scan => return Ok(()),
            HistoryProbe::Event(event) => return Self::store_history_event(generation, event),
            HistoryProbe::Required(required) => required,
        };
        if required > generation.bounds.max_record_bytes {
            return Err(NativeStateError::LimitExceeded);
        }
        if generation.scratch.capacity() < required {
            generation
                .scratch
                .try_reserve_exact(required.saturating_sub(generation.scratch.len()))
                .map_err(|_| NativeStateError::OutOfMemory)?;
        }
        generation.scratch.resize(required, 0);
        let event = Self::read_history_capture(terminal, generation)?;
        let written = event.written();
        generation
            .pending
            .try_reserve(written)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        generation
            .pending
            .extend_from_slice(&generation.scratch[..written]);
        if generation
            .retained_bytes
            .checked_add(generation.pending.capacity())
            .and_then(|bytes| bytes.checked_add(generation.scratch.capacity()))
            .is_none_or(|bytes| bytes > generation.bounds.max_total_bytes)
        {
            return Err(NativeStateError::LimitExceeded);
        }
        Self::store_history_event(generation, event)
    }

    pub(super) fn probe_history_capture(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        generation: &mut NativeCheckpointGeneration,
    ) -> Result<HistoryProbe, NativeStateError> {
        let result = {
            let capture = generation
                .capture
                .as_mut()
                .ok_or(NativeStateError::InvalidHandle)?;
            capture.next(terminal, &mut [])
        };
        match result {
            Err(GhosttyError::OutOfSpace { required }) => Ok(HistoryProbe::Required(required)),
            Ok(CaptureEvent::Scan) => Ok(HistoryProbe::Scan),
            Ok(event) if event.written() == 0 => Ok(HistoryProbe::Event(event)),
            Ok(_) => Err(generation.spend(NativeStateError::InvalidState)),
            Err(error) => Err(generation.spend(error.into())),
        }
    }

    pub(super) fn read_history_capture(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        generation: &mut NativeCheckpointGeneration,
    ) -> Result<CaptureEvent, NativeStateError> {
        let result = {
            let NativeCheckpointGeneration {
                capture, scratch, ..
            } = generation;
            let capture = capture.as_mut().ok_or(NativeStateError::InvalidHandle)?;
            capture.next(terminal, scratch)
        };
        match result {
            Ok(event) => Ok(event),
            Err(GhosttyError::OutOfSpace { required }) => Err(NativeStateError::OutOfSpace {
                required_bytes: required,
                required_rows: 0,
            }),
            Err(error) => Err(generation.spend(error.into())),
        }
    }

    pub(super) fn store_history_event(
        generation: &mut NativeCheckpointGeneration,
        event: CaptureEvent,
    ) -> Result<(), NativeStateError> {
        match event {
            CaptureEvent::Scan | CaptureEvent::Record { .. } => Ok(()),
            CaptureEvent::HistoryPage { rows, .. } => {
                Self::store_history_record(generation, rows, false)
            }
            CaptureEvent::Finish { .. } => {
                Self::store_history_record(generation, 0, true)?;
                generation.capture = None;
                Ok(())
            }
            CaptureEvent::Invalidated(reason) => Err(generation.spend(invalidation_error(reason))),
            CaptureEvent::Ready { .. } => Err(generation.spend(NativeStateError::InvalidState)),
        }
    }

    pub(super) fn store_history_record(
        generation: &mut NativeCheckpointGeneration,
        rows: usize,
        finish: bool,
    ) -> Result<(), NativeStateError> {
        let next_bytes = generation
            .retained_bytes
            .checked_add(generation.pending.len())
            .filter(|bytes| *bytes <= generation.bounds.max_total_bytes)
            .ok_or(NativeStateError::LimitExceeded)?;
        if generation.pending.len() > generation.bounds.max_record_bytes
            || rows > generation.bounds.max_rows
            || generation.records.len >= generation.records.slots.len()
        {
            return Err(NativeStateError::LimitExceeded);
        }
        let payload = ChargedNativePayload::new(
            std::mem::take(&mut generation.pending).into_boxed_slice(),
            Arc::clone(&generation.charge),
        );
        generation.records.slots[generation.records.len] = Some(CachedNativeHistoryRecord {
            bytes: Bytes::from_owner(payload),
            rows,
            finish,
        });
        generation.records.len += 1;
        generation.retained_bytes = next_bytes;
        Ok(())
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
