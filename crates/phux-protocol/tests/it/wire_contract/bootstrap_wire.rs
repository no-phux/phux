//! Focused protocol-0.7 bootstrap/profile wire contract tests.

#![allow(clippy::unwrap_used)]

use bytes::{Bytes, BytesMut};
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities, EngineCodec, EngineCodecSet,
    EngineFeature, EngineFeatureSet, MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES, OutputMode,
    ServerCapabilities, select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, HistoryRejectionReason, HistoryTombstoneReason, MAX_HISTORY_CURSOR_BYTES,
    MAX_HISTORY_PAGE_ROWS, TYPE_ATTACH_READY, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK,
    TYPE_BOOTSTRAP_READY, TYPE_BOOTSTRAP_TOMBSTONE, TYPE_HELLO_OK, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_REQUEST, TYPE_HISTORY_TOMBSTONE, TombstoneReason,
};

use crate::common::{
    assert_round_trip, decode_err, framed_tlv as framed, local_id_bytes as local_terminal,
    take_varint, tlv_field,
};

mod frame_shapes;
mod history;
mod payloads;
mod profile_selection;
mod unknown_codec;

const fn stream(raw: u64) -> StreamId {
    StreamId::new(raw).unwrap()
}

const fn bootstrap(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).unwrap()
}

/// The ids every history builder below shares: terminal 1, stream 2,
/// bootstrap 3.
const fn ids() -> (ResourceId, StreamId, BootstrapId) {
    (ResourceId::local(1), stream(2), bootstrap(3))
}

fn chunk(payload: Bytes) -> FrameKind {
    let (terminal_id, stream_id, bootstrap_id) = ids();
    FrameKind::BootstrapChunk {
        terminal_id,
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload,
    }
}

fn history_page(payload: Bytes, rows: u32) -> FrameKind {
    let (terminal_id, stream_id, bootstrap_id) = ids();
    FrameKind::HistoryPage {
        terminal_id,
        stream_id,
        bootstrap_id,
        page_seq: 1,
        cursor: Bytes::from_static(b"cursor"),
        next_cursor: None,
        payload,
        rows,
    }
}

fn history_request(cursor: &'static [u8], max_bytes: u32, max_rows: u32) -> FrameKind {
    let (terminal_id, stream_id, bootstrap_id) = ids();
    FrameKind::HistoryRequest {
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(cursor),
        max_bytes,
        max_rows,
    }
}

fn history_tombstone(reason: HistoryTombstoneReason) -> FrameKind {
    let (terminal_id, stream_id, bootstrap_id) = ids();
    FrameKind::HistoryTombstone {
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(b"cursor"),
        reason,
    }
}

fn history_rejected(
    reason: HistoryRejectionReason,
    required_bytes: u32,
    required_rows: u32,
) -> FrameKind {
    let (terminal_id, stream_id, bootstrap_id) = ids();
    FrameKind::HistoryRejected {
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(b"cursor"),
        reason,
        required_bytes,
        required_rows,
    }
}

fn encoded_field_ids(frame: &FrameKind) -> Vec<u64> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let mut offset = 5;
    let mut ids = Vec::new();
    while offset < encoded.len() {
        ids.push(take_varint(&encoded, &mut offset));
        assert_eq!(encoded[offset], 4);
        offset += 1;
        let length = usize::try_from(take_varint(&encoded, &mut offset)).unwrap();
        offset += length;
    }
    assert_eq!(offset, encoded.len());
    ids
}

fn encode_without_field(frame: &FrameKind, omitted_id: u64) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let type_byte = encoded[4];
    let mut offset = 5;
    let mut fields = Vec::new();
    while offset < encoded.len() {
        let field_start = offset;
        let id = take_varint(&encoded, &mut offset);
        assert_eq!(encoded[offset], 4);
        offset += 1;
        let length = usize::try_from(take_varint(&encoded, &mut offset)).unwrap();
        offset += length;
        if id != omitted_id {
            fields.extend_from_slice(&encoded[field_start..offset]);
        }
    }
    framed(type_byte, &fields)
}

// ---------------------------------------------------------------------------
// FRAME_COMPRESSED (docs/protocol.md#framing)
// ---------------------------------------------------------------------------
