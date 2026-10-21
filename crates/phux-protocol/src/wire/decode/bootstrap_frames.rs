//! Bootstrap and history frame bodies, with their negotiated bounds.

use super::{
    DecodeError, Decoder, FrameKind, HistoryRejectionReason, HistoryTombstoneReason,
    MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_ROWS, TombstoneReason, decode_bootstrap_codec,
    decode_bootstrap_id, decode_bootstrap_stream_profile, decode_stream_id, decode_terminal_id,
    field, req,
};

impl Decoder<'_> {
    pub(super) fn decode_bootstrap_begin(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_begin as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut codec = None;
        let mut cols = None;
        let mut rows = None;
        let mut output_mode = None;
        let mut base_seq = None;
        let mut resume_after_seq = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CODEC => codec = Some(sub!(value, decode_bootstrap_codec)),
                f::COLS => cols = Some(sub!(value, Decoder::read_u16_be)),
                f::ROWS => rows = Some(sub!(value, Decoder::read_u16_be)),
                f::OUTPUT_MODE => output_mode = Some(sub!(value, Decoder::read_u8)),
                f::BASE_SEQ => base_seq = Some(sub!(value, Decoder::read_u64_be)),
                f::RESUME_AFTER_SEQ => {
                    if value.len() != 8 {
                        return Err(DecodeError::InvalidBootstrapProfile);
                    }
                    resume_after_seq = Some(sub!(value, Decoder::read_u64_be));
                }
                _ => {}
            }
        }
        let profile =
            decode_bootstrap_stream_profile(req(codec)?, req(output_mode)?, resume_after_seq)?;
        let base_seq = req(base_seq)?;
        if resume_after_seq.is_some_and(|after_seq| after_seq > base_seq) {
            return Err(DecodeError::InvalidBootstrapProfile);
        }
        let (cols, rows) = checked_bootstrap_dimensions(profile, cols, rows)?;
        Ok(FrameKind::BootstrapBegin {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            profile,
            cols,
            rows,
            base_seq,
        })
    }

    pub(super) fn decode_bootstrap_chunk(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_chunk as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut chunk_seq = None;
        let mut payload = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CHUNK_SEQ => chunk_seq = Some(sub!(value, Decoder::read_u32_be)),
                f::PAYLOAD => {
                    if value.len() > self.max_bootstrap_chunk_bytes as usize {
                        return Err(DecodeError::BootstrapLimitExceeded);
                    }
                    payload = Some(bytes::Bytes::copy_from_slice(value));
                }
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapChunk {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            chunk_seq: req(chunk_seq)?,
            payload: req(payload)?,
        })
    }

    pub(super) fn decode_bootstrap_ready(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_ready as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut history_cursor = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::HISTORY_CURSOR => history_cursor = Some(checked_history_cursor(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapReady {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            history_cursor,
        })
    }

    pub(super) fn decode_history_request(&mut self) -> Result<FrameKind, DecodeError> {
        use field::history_request as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut max_bytes = None;
        let mut max_rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CURSOR => cursor = Some(checked_history_cursor(value)?),
                f::MAX_BYTES => max_bytes = Some(sub!(value, Decoder::read_u32_be)),
                f::MAX_ROWS => max_rows = Some(sub!(value, Decoder::read_u32_be)),
                _ => {}
            }
        }
        let max_bytes = req(max_bytes)?;
        let max_rows = req(max_rows)?;
        Ok(FrameKind::HistoryRequest {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            cursor: req(cursor)?,
            max_bytes,
            max_rows,
        })
    }

    /// Borrow every field until all scalars validate, so a malformed frame
    /// cannot make us copy a payload first.
    pub(super) fn decode_history_page(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut page_seq = None;
        let mut cursor = None;
        let mut next_cursor = None;
        let mut payload = None;
        let mut rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::history_page::TERMINAL_ID => terminal_id = Some(value),
                field::history_page::STREAM_ID => stream_id = Some(value),
                field::history_page::BOOTSTRAP_ID => bootstrap_id = Some(value),
                field::history_page::CURSOR => cursor = Some(value),
                field::history_page::NEXT_CURSOR => next_cursor = Some(value),
                field::history_page::PAYLOAD => payload = Some(value),
                field::history_page::PAGE_SEQ => page_seq = Some(value),
                field::history_page::ROWS => rows = Some(value),
                _ => {}
            }
        }

        let stream_id = sub!(req(stream_id)?, decode_stream_id);
        let bootstrap_id = sub!(req(bootstrap_id)?, decode_bootstrap_id);
        let page_seq = checked_history_page_seq(page_seq)?;
        let rows = checked_history_page_rows(rows)?;

        let payload = req(payload)?;
        if payload.len() > self.max_history_page_bytes as usize {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        let cursor = req(cursor)?;
        if cursor.len() > MAX_HISTORY_CURSOR_BYTES {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        if next_cursor.is_some_and(|next| next.len() > MAX_HISTORY_CURSOR_BYTES) {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        let terminal_id = sub!(req(terminal_id)?, decode_terminal_id);

        Ok(FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor: bytes::Bytes::copy_from_slice(cursor),
            next_cursor: next_cursor.map(bytes::Bytes::copy_from_slice),
            payload: bytes::Bytes::copy_from_slice(payload),
            rows,
        })
    }

    pub(super) fn decode_bootstrap_tombstone(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_tombstone as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut reason = None;
        let mut last_valid_seq = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::REASON => {
                    let value = sub!(value, Decoder::read_u8);
                    reason =
                        Some(TombstoneReason::from_wire(value).unwrap_or(TombstoneReason::Unknown));
                }
                f::LAST_VALID_SEQ => last_valid_seq = Some(sub!(value, Decoder::read_u64_be)),
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapTombstone {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            reason: req(reason)?,
            last_valid_seq: req(last_valid_seq)?,
        })
    }

    pub(super) fn decode_history_tombstone(&mut self) -> Result<FrameKind, DecodeError> {
        use field::history_tombstone as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut reason = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CURSOR => cursor = Some(checked_history_cursor(value)?),
                f::REASON => {
                    let value = sub!(value, Decoder::read_u8);
                    reason = Some(
                        HistoryTombstoneReason::from_wire(value)
                            .unwrap_or(HistoryTombstoneReason::Unknown),
                    );
                }
                _ => {}
            }
        }
        Ok(FrameKind::HistoryTombstone {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            cursor: req(cursor)?,
            reason: req(reason)?,
        })
    }

    /// Validate a `HISTORY_REJECTED` body's required `required_bytes` retry
    /// hint against the limit negotiated for this connection.
    pub(super) fn checked_history_required_bytes(
        &self,
        required_bytes: Option<u32>,
    ) -> Result<u32, DecodeError> {
        let required_bytes = req(required_bytes)?;
        if required_bytes == 0 || required_bytes > self.max_history_page_bytes {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        Ok(required_bytes)
    }

    pub(super) fn decode_history_rejected(&mut self) -> Result<FrameKind, DecodeError> {
        use field::history_rejected as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut reason = None;
        let mut required_bytes = None;
        let mut required_rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CURSOR => cursor = Some(checked_history_cursor(value)?),
                f::REASON => {
                    let value = sub!(value, Decoder::read_u8);
                    reason = Some(
                        HistoryRejectionReason::from_wire(value)
                            .unwrap_or(HistoryRejectionReason::Unknown),
                    );
                }
                f::REQUIRED_BYTES => required_bytes = Some(sub!(value, Decoder::read_u32_be)),
                f::REQUIRED_ROWS => required_rows = Some(sub!(value, Decoder::read_u32_be)),
                _ => {}
            }
        }
        let required_bytes = self.checked_history_required_bytes(required_bytes)?;
        let required_rows = checked_history_required_rows(required_rows)?;
        Ok(FrameKind::HistoryRejected {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            cursor: req(cursor)?,
            reason: req(reason)?,
            required_bytes,
            required_rows,
        })
    }
}

/// Copy a history cursor field value, refusing one longer than
/// [`MAX_HISTORY_CURSOR_BYTES`].
pub(super) fn checked_history_cursor(value: &[u8]) -> Result<bytes::Bytes, DecodeError> {
    if value.len() > MAX_HISTORY_CURSOR_BYTES {
        return Err(DecodeError::BootstrapLimitExceeded);
    }
    Ok(bytes::Bytes::copy_from_slice(value))
}

/// Validate `BOOTSTRAP_BEGIN` dimensions: a Terminal stream needs a non-zero
/// grid, and a gridless `AgentEventsJsonlV1` stream must carry `0 x 0`.
pub(super) fn checked_bootstrap_dimensions(
    profile: crate::caps::BootstrapStreamProfile,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<(u16, u16), DecodeError> {
    let cols = req(cols)?;
    let rows = req(rows)?;
    let gridless = matches!(
        profile,
        crate::caps::BootstrapStreamProfile::AgentEventsJsonlV1
            | crate::caps::BootstrapStreamProfile::AgentEventsJsonlResumeV1 { .. }
    );
    if gridless != (cols == 0 && rows == 0) {
        return Err(DecodeError::InvalidBootstrapProfile);
    }
    Ok((cols, rows))
}

/// Read a required non-zero `HISTORY_PAGE.page_seq`.
pub(super) fn checked_history_page_seq(value: Option<&[u8]>) -> Result<u64, DecodeError> {
    let page_seq = sub!(req(value)?, Decoder::read_u64_be);
    if page_seq == 0 {
        return Err(DecodeError::InvalidHistoryPageSequence);
    }
    Ok(page_seq)
}

/// Read a required `HISTORY_PAGE.rows`, at most [`MAX_HISTORY_PAGE_ROWS`].
pub(super) fn checked_history_page_rows(value: Option<&[u8]>) -> Result<u32, DecodeError> {
    let rows = sub!(req(value)?, Decoder::read_u32_be);
    if rows > MAX_HISTORY_PAGE_ROWS {
        return Err(DecodeError::HistoryRowLimitExceeded);
    }
    Ok(rows)
}

/// Validate a required `HISTORY_REJECTED.required_rows` retry hint.
pub(super) fn checked_history_required_rows(
    required_rows: Option<u32>,
) -> Result<u32, DecodeError> {
    let required_rows = req(required_rows)?;
    if required_rows == 0 || required_rows > MAX_HISTORY_PAGE_ROWS {
        return Err(DecodeError::HistoryRowLimitExceeded);
    }
    Ok(required_rows)
}
