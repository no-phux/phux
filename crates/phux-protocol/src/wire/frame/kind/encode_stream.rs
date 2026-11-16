//! Payload writers for the bootstrap, history, bell, and error frames.

use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::encode_bootstrap_codec;

use super::{FrameKind, generation_fields, id_field};

impl FrameKind {
    /// Write a stream frame's payload. Bootstrap and history frames open
    /// with their generation (fields 1-3).
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per stream frame, each its field order"
    )]
    pub(super) fn encode_stream_payload(&self, enc: &mut Encoder<'_>) {
        match self {
            // Native state always means raw, byte-identical PTY continuation.
            Self::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                enc.write_field_with(field::bootstrap_begin::CODEC, |e| {
                    encode_bootstrap_codec(profile.codec(), e);
                });
                enc.write_u16_field(field::bootstrap_begin::COLS, *cols);
                enc.write_u16_field(field::bootstrap_begin::ROWS, *rows);
                let output_mode = profile.output_mode().as_wire();
                enc.write_u8_field(field::bootstrap_begin::OUTPUT_MODE, output_mode);
                enc.write_u64_field(field::bootstrap_begin::BASE_SEQ, *base_seq);
                if let Some(after_seq) = profile.resume_after_seq() {
                    enc.write_u64_field(field::bootstrap_begin::RESUME_AFTER_SEQ, after_seq);
                }
            }
            Self::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                enc.write_u32_field(field::bootstrap_chunk::CHUNK_SEQ, *chunk_seq);
                enc.write_field(field::bootstrap_chunk::PAYLOAD, payload);
            }
            Self::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                if let Some(cursor) = history_cursor {
                    enc.write_field(field::bootstrap_ready::HISTORY_CURSOR, cursor);
                }
            }
            Self::HistoryRequest {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                enc.write_field(field::history_request::CURSOR, cursor);
                enc.write_u32_field(field::history_request::MAX_BYTES, *max_bytes);
                enc.write_u32_field(field::history_request::MAX_ROWS, *max_rows);
            }
            Self::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                enc.write_field(field::history_page::CURSOR, cursor);
                if let Some(next) = next_cursor {
                    enc.write_field(field::history_page::NEXT_CURSOR, next);
                }
                enc.write_field(field::history_page::PAYLOAD, payload);
                enc.write_u64_field(field::history_page::PAGE_SEQ, *page_seq);
                enc.write_u32_field(field::history_page::ROWS, *rows);
            }
            Self::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                enc.write_u8_field(field::bootstrap_tombstone::REASON, reason.as_wire());
                let last = *last_valid_seq;
                enc.write_u64_field(field::bootstrap_tombstone::LAST_VALID_SEQ, last);
            }
            Self::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                enc.write_field(field::history_tombstone::CURSOR, cursor);
                enc.write_u8_field(field::history_tombstone::REASON, reason.as_wire());
            }
            Self::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                enc.write_field(field::history_rejected::CURSOR, cursor);
                enc.write_u8_field(field::history_rejected::REASON, reason.as_wire());
                let (bytes, rows) = (*required_bytes, *required_rows);
                enc.write_u32_field(field::history_rejected::REQUIRED_BYTES, bytes);
                enc.write_u32_field(field::history_rejected::REQUIRED_ROWS, rows);
            }
            Self::Bell { terminal_id } => id_field(enc, field::bell::TERMINAL_ID, terminal_id),
            Self::Error {
                request_id,
                code,
                message,
            } => {
                if let Some(id) = request_id {
                    enc.write_u32_field(field::error::REQUEST_ID, *id);
                }
                enc.write_u16_field(field::error::CODE, code.as_wire());
                enc.write_field(field::error::MESSAGE, message.as_bytes());
            }
            _ => unreachable!("encode_payload routes only stream frames here"),
        }
    }
}
