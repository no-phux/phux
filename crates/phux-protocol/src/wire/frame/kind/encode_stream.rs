//! Payload writers for the bootstrap, history, bell, and error frames.

use crate::caps::{BootstrapCodec, BootstrapStreamProfile, OutputMode};
use crate::ids::{BootstrapId, ResourceId, StreamId};
use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::{
    ErrorCode, HistoryRejectionReason, HistoryTombstoneReason, TombstoneReason,
    encode_bootstrap_codec,
};

use super::{FrameKind, generation_fields, id_field, u8_field, u16_field, u32_field, u64_field};

impl FrameKind {
    /// Write the profile half of `BOOTSTRAP_BEGIN`: codec, geometry, cut,
    /// and optional agent resume anchor. Native state always means raw,
    /// byte-identical PTY continuation.
    pub(super) fn encode_bootstrap_begin_profile(
        enc: &mut Encoder<'_>,
        profile: BootstrapStreamProfile,
        cols: u16,
        rows: u16,
        base_seq: u64,
    ) {
        let (codec, output_mode) = match profile {
            BootstrapStreamProfile::NativeState { codec } => {
                (BootstrapCodec::Native(codec), OutputMode::Raw)
            }
            BootstrapStreamProfile::SynthesizedVtRaw => {
                (BootstrapCodec::SynthesizedVtV1, OutputMode::Raw)
            }
            BootstrapStreamProfile::SynthesizedVtStateSync => {
                (BootstrapCodec::SynthesizedVtV1, OutputMode::StateSync)
            }
            BootstrapStreamProfile::AgentEventsJsonlV1
            | BootstrapStreamProfile::AgentEventsJsonlResumeV1 { .. } => {
                (BootstrapCodec::AgentEventsJsonlV1, OutputMode::Raw)
            }
        };
        enc.write_field_with(field::bootstrap_begin::CODEC, |e| {
            encode_bootstrap_codec(codec, e);
        });
        u16_field(enc, field::bootstrap_begin::COLS, cols);
        u16_field(enc, field::bootstrap_begin::ROWS, rows);
        u8_field(
            enc,
            field::bootstrap_begin::OUTPUT_MODE,
            output_mode.as_wire(),
        );
        u64_field(enc, field::bootstrap_begin::BASE_SEQ, base_seq);
        if let BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq } = profile {
            u64_field(enc, field::bootstrap_begin::RESUME_AFTER_SEQ, after_seq);
        }
    }

    /// Write the `BOOTSTRAP_CHUNK` payload.
    pub(super) fn encode_bootstrap_chunk(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        u32_field(enc, field::bootstrap_chunk::CHUNK_SEQ, chunk_seq);
        enc.write_field(field::bootstrap_chunk::PAYLOAD, payload);
    }

    /// Write the `BOOTSTRAP_READY` payload.
    pub(super) fn encode_bootstrap_ready(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        history_cursor: Option<&[u8]>,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        if let Some(cursor) = history_cursor {
            enc.write_field(field::bootstrap_ready::HISTORY_CURSOR, cursor);
        }
    }

    /// Write the `HISTORY_REQUEST` payload.
    pub(super) fn encode_history_request(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        max_bytes: u32,
        max_rows: u32,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        enc.write_field(field::history_request::CURSOR, cursor);
        u32_field(enc, field::history_request::MAX_BYTES, max_bytes);
        u32_field(enc, field::history_request::MAX_ROWS, max_rows);
    }

    /// Write the page half of `HISTORY_PAGE`: cursors, payload, and counts.
    pub(super) fn encode_history_page_body(
        enc: &mut Encoder<'_>,
        cursor: &[u8],
        next_cursor: Option<&[u8]>,
        payload: &[u8],
        page_seq: u64,
        rows: u32,
    ) {
        enc.write_field(field::history_page::CURSOR, cursor);
        if let Some(next) = next_cursor {
            enc.write_field(field::history_page::NEXT_CURSOR, next);
        }
        enc.write_field(field::history_page::PAYLOAD, payload);
        u64_field(enc, field::history_page::PAGE_SEQ, page_seq);
        u32_field(enc, field::history_page::ROWS, rows);
    }

    /// Write the `BOOTSTRAP_TOMBSTONE` payload.
    pub(super) fn encode_bootstrap_tombstone(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        reason: TombstoneReason,
        last_valid_seq: u64,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        u8_field(enc, field::bootstrap_tombstone::REASON, reason.as_wire());
        u64_field(
            enc,
            field::bootstrap_tombstone::LAST_VALID_SEQ,
            last_valid_seq,
        );
    }

    /// Write the `HISTORY_TOMBSTONE` payload.
    pub(super) fn encode_history_tombstone(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        reason: HistoryTombstoneReason,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        enc.write_field(field::history_tombstone::CURSOR, cursor);
        u8_field(enc, field::history_tombstone::REASON, reason.as_wire());
    }

    /// Write why `HISTORY_REJECTED` refused the cursor, and what it would need.
    pub(super) fn encode_history_rejected_reason(
        enc: &mut Encoder<'_>,
        cursor: &[u8],
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    ) {
        enc.write_field(field::history_rejected::CURSOR, cursor);
        u8_field(enc, field::history_rejected::REASON, reason.as_wire());
        u32_field(enc, field::history_rejected::REQUIRED_BYTES, required_bytes);
        u32_field(enc, field::history_rejected::REQUIRED_ROWS, required_rows);
    }

    /// Write the `BELL` payload.
    pub(super) fn encode_bell(enc: &mut Encoder<'_>, terminal_id: &ResourceId) {
        id_field(enc, field::bell::TERMINAL_ID, terminal_id);
    }

    /// Write the `ERROR` payload.
    pub(super) fn encode_error(
        enc: &mut Encoder<'_>,
        request_id: Option<u32>,
        code: ErrorCode,
        message: &str,
    ) {
        // Optional request_id: absent field = None.
        if let Some(id) = request_id {
            u32_field(enc, field::error::REQUEST_ID, id);
        }
        u16_field(enc, field::error::CODE, code.as_wire());
        enc.write_field(field::error::MESSAGE, message.as_bytes());
    }
}
