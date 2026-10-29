//! Focused protocol-0.7 bootstrap/profile wire contract tests.

#![allow(clippy::unwrap_used)]

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
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
    assert_round_trip, framed_tlv as framed, local_id_bytes as local_terminal, take_varint,
    tlv_field,
};

mod frame_shapes;
mod history;
mod payloads;
mod profile_selection;

const fn stream(raw: u64) -> StreamId {
    StreamId::new(raw).unwrap()
}

const fn bootstrap(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).unwrap()
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
