//! [`FrameKind`] — the decoded wire frame — with its type-byte table and
//! its encode/decode entry points.

use bytes::BytesMut;

use crate::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities, Compression,
    ServerCapabilities,
};
use crate::ids::{
    BootstrapId, ClientId, GroupId, RESOURCE_ID_TAG_LOCAL, ResourceId, SatelliteHost, StreamId,
};
use crate::input::InputEvent;
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::PasteEvent;
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;
use crate::wire::framing::LENGTH_PREFIX_LEN;
use crate::wire::info::SessionSnapshot;

use super::directory::DirectoryListingResult;
use super::path::PathQueryResult;
use super::{
    ActorRef, AgentEvent, AttachTarget, CloseReason, Command, CommandResult, DetachReason,
    ErrorCode, EventStamp, HistoryRejectionReason, HistoryTombstoneReason, MAX_FRAME_LEN,
    MoveResult, Scope, SpawnResource, SpawnResult, TYPE_ATTACH, TYPE_ATTACH_READY, TYPE_ATTACHED,
    TYPE_BELL, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY,
    TYPE_BOOTSTRAP_TOMBSTONE, TYPE_COMMAND, TYPE_COMMAND_RESULT, TYPE_DELETE_METADATA, TYPE_DETACH,
    TYPE_DETACHED, TYPE_DIRECTORY_LISTING, TYPE_ERROR, TYPE_EVENT, TYPE_FRAME_ACK,
    TYPE_FRAME_COMPRESSED, TYPE_GET_METADATA, TYPE_HELLO, TYPE_HELLO_OK, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_REQUEST, TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_FOCUS,
    TYPE_INPUT_KEY, TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE, TYPE_INPUT_TERMINAL_REPLY,
    TYPE_LIST_DIRECTORY, TYPE_LIST_METADATA, TYPE_METADATA_CHANGED, TYPE_METADATA_KEYS,
    TYPE_METADATA_VALUE, TYPE_MOVE_RESOURCE, TYPE_PATH_QUERY, TYPE_PATH_RESULTS, TYPE_PING,
    TYPE_PONG, TYPE_RESIZE_TERMINAL, TYPE_RESOURCE_CLOSED, TYPE_RESOURCE_MOVED,
    TYPE_RESOURCE_OUTPUT, TYPE_RESOURCE_SPAWNED, TYPE_SET_METADATA, TYPE_SPAWN_RESOURCE,
    TYPE_SUBSCRIBE_EVENTS, TYPE_SUBSCRIBE_METADATA, TYPE_VIEWPORT_RESIZE, TombstoneReason,
    ViewportInfo, encode_actor_ref, encode_terminal_id,
};

mod encode_handshake;
mod encode_metadata;
mod encode_resource;
mod encode_stream;
mod payload;
#[macro_use]
mod connection_variants;
#[macro_use]
mod resource_variants;

/// Declares [`FrameKind`] around the variant list the family macros build.
macro_rules! frame_kind_enum {
    ($($variants:tt)*) => {
/// Decoded wire frame (`docs/spec/proto.md` §7).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FrameKind {
    $($variants)*
}
    };
}

// One file per variant family keeps each source file small; each family
// macro appends its variants, in declaration order, and hands the list on.
connection_frames!(resource_frames!(frame_kind_enum!()));

impl InputEvent {
    /// Wrap this event in the matching `INPUT_*` [`FrameKind`] addressed to
    /// `terminal_id`. Lives here so `crate::input` stays a leaf.
    #[must_use]
    pub fn into_frame(self, terminal_id: ResourceId) -> FrameKind {
        match self {
            Self::Key(event) => FrameKind::InputKey { terminal_id, event },
            Self::Mouse(event) => FrameKind::InputMouse { terminal_id, event },
            Self::Focus(event) => FrameKind::InputFocus { terminal_id, event },
            Self::Paste(event) => FrameKind::InputPaste { terminal_id, event },
        }
    }
}

impl FrameKind {
    /// Type discriminant from `docs/spec/proto.md` §7.
    #[must_use]
    pub const fn type_byte(&self) -> u8 {
        match self {
            Self::Hello { .. } => TYPE_HELLO,
            Self::HelloOk { .. } => TYPE_HELLO_OK,
            Self::Ping { .. } => TYPE_PING,
            Self::Pong { .. } => TYPE_PONG,
            Self::ResourceOutput { .. } => TYPE_RESOURCE_OUTPUT,
            Self::Attach { .. } => TYPE_ATTACH,
            Self::Detach => TYPE_DETACH,
            Self::InputKey { .. } => TYPE_INPUT_KEY,
            Self::InputMouse { .. } => TYPE_INPUT_MOUSE,
            Self::InputFocus { .. } => TYPE_INPUT_FOCUS,
            Self::InputPaste { .. } => TYPE_INPUT_PASTE,
            Self::InputTerminalReply { .. } => TYPE_INPUT_TERMINAL_REPLY,
            Self::FrameAck { .. } => TYPE_FRAME_ACK,
            Self::ViewportResize { .. } => TYPE_VIEWPORT_RESIZE,
            Self::Attached { .. } => TYPE_ATTACHED,
            Self::AttachReady { .. } => TYPE_ATTACH_READY,
            Self::Detached { .. } => TYPE_DETACHED,
            Self::HistoryRequest { .. } => TYPE_HISTORY_REQUEST,
            Self::BootstrapBegin { .. } => TYPE_BOOTSTRAP_BEGIN,
            Self::BootstrapChunk { .. } => TYPE_BOOTSTRAP_CHUNK,
            Self::BootstrapReady { .. } => TYPE_BOOTSTRAP_READY,
            Self::HistoryPage { .. } => TYPE_HISTORY_PAGE,
            Self::BootstrapTombstone { .. } => TYPE_BOOTSTRAP_TOMBSTONE,
            Self::HistoryTombstone { .. } => TYPE_HISTORY_TOMBSTONE,
            Self::HistoryRejected { .. } => TYPE_HISTORY_REJECTED,
            Self::Bell { .. } => TYPE_BELL,
            Self::Error { .. } => TYPE_ERROR,
            Self::GetMetadata { .. } => TYPE_GET_METADATA,
            Self::SetMetadata { .. } => TYPE_SET_METADATA,
            Self::DeleteMetadata { .. } => TYPE_DELETE_METADATA,
            Self::ListMetadata { .. } => TYPE_LIST_METADATA,
            Self::SubscribeMetadata { .. } => TYPE_SUBSCRIBE_METADATA,
            Self::MetadataChanged { .. } => TYPE_METADATA_CHANGED,
            Self::MetadataValue { .. } => TYPE_METADATA_VALUE,
            Self::MetadataKeys { .. } => TYPE_METADATA_KEYS,
            Self::ListDirectory { .. } => TYPE_LIST_DIRECTORY,
            Self::DirectoryListing { .. } => TYPE_DIRECTORY_LISTING,
            Self::PathQuery { .. } => TYPE_PATH_QUERY,
            Self::PathResults { .. } => TYPE_PATH_RESULTS,
            Self::SpawnResource { .. } => TYPE_SPAWN_RESOURCE,
            Self::MoveResource { .. } => TYPE_MOVE_RESOURCE,
            Self::ResourceMoved { .. } => TYPE_RESOURCE_MOVED,
            Self::ResourceSpawned { .. } => TYPE_RESOURCE_SPAWNED,
            Self::ResourceClosed { .. } => TYPE_RESOURCE_CLOSED,
            Self::ResizeTerminal { .. } => TYPE_RESIZE_TERMINAL,
            Self::Command { .. } => TYPE_COMMAND,
            Self::CommandResult { .. } => TYPE_COMMAND_RESULT,
            Self::SubscribeEvents { .. } => TYPE_SUBSCRIBE_EVENTS,
            Self::Event { .. } => TYPE_EVENT,
        }
    }

    /// Encode `self`, wrapped in `FRAME_COMPRESSED` when `compression` was
    /// negotiated and deflate actually shrinks it; otherwise a plain
    /// [`Self::encode`]. `scratch` is a reusable buffer for the plain image.
    pub fn encode_compressed(
        &self,
        compression: Compression,
        scratch: &mut BytesMut,
        out: &mut BytesMut,
    ) {
        if compression == Compression::None {
            self.encode(out);
            return;
        }
        scratch.clear();
        self.encode(scratch);
        // The envelope carries the inner frame minus its length prefix.
        let Some(body) = scratch.get(LENGTH_PREFIX_LEN..) else {
            self.encode(out);
            return;
        };
        let Some(deflated) = crate::wire::compress::deflate(body) else {
            out.extend_from_slice(scratch);
            return;
        };
        let Ok(uncompressed_len) = u32::try_from(body.len()) else {
            out.extend_from_slice(scratch);
            return;
        };
        let header_pos = out.len();
        out.extend_from_slice(&[0u8; LENGTH_PREFIX_LEN]);
        let body_start = out.len();
        let mut enc = Encoder::new(out);
        enc.write_u8(TYPE_FRAME_COMPRESSED);
        u8_field(
            &mut enc,
            field::frame_compressed::ALGORITHM,
            compression.as_u8(),
        );
        u32_field(
            &mut enc,
            field::frame_compressed::UNCOMPRESSED_LEN,
            uncompressed_len,
        );
        enc.write_field(field::frame_compressed::PAYLOAD, &deflated);
        let body_len = out.len() - body_start;
        let len_u32 = u32::try_from(body_len).unwrap_or(u32::MAX);
        out[header_pos..header_pos + LENGTH_PREFIX_LEN].copy_from_slice(&len_u32.to_be_bytes());
    }

    /// Encode `self` as a complete length-prefixed frame.
    ///
    /// Writes the four-byte big-endian length header, the type byte, and the
    /// payload. The caller owns the `BytesMut` lifecycle.
    pub fn encode(&self, out: &mut BytesMut) {
        // Reserve four bytes for the length header; backfill once we know how
        // many bytes the type + payload consumed.
        let header_pos = out.len();
        out.extend_from_slice(&[0u8; 4]);

        let body_start = out.len();
        let mut enc = Encoder::new(out);
        enc.write_u8(self.type_byte());
        self.encode_payload(&mut enc);

        // Backfill the length header. The length value excludes the four
        // header bytes themselves but includes the type byte and payload, per
        // SPEC §5.
        let body_len = out.len() - body_start;
        debug_assert!(
            u32::try_from(body_len).is_ok_and(|n| n <= MAX_FRAME_LEN),
            "encoded frame exceeds protocol cap",
        );
        let len_u32 = u32::try_from(body_len).unwrap_or(u32::MAX);
        out[header_pos..header_pos + 4].copy_from_slice(&len_u32.to_be_bytes());
    }

    /// Decode a single frame from `input`. Returns the decoded frame and the
    /// unconsumed tail of `input`.
    pub fn decode(input: &[u8]) -> Result<(Self, &[u8]), DecodeError> {
        Decoder::new(input).read_frame()
    }

    /// Decode one frame using the payload limits negotiated in `HELLO_OK`.
    ///
    /// Bootstrap/history payload lengths are rejected against `limits` while
    /// still borrowed from the input, before an owned payload copy is made.
    pub fn decode_with_limits(
        input: &[u8],
        limits: BootstrapLimits,
    ) -> Result<(Self, &[u8]), DecodeError> {
        Decoder::with_bootstrap_limits(input, limits).read_frame()
    }

    /// Decode one frame whose payload byte fields alias `input`.
    ///
    /// [`Self::decode`] copies those fields because a borrowed slice cannot
    /// keep the buffer alive. This entry point is the client read path: the
    /// frame is already a frozen view of the socket buffer, and the payload
    /// stays that view until the caller drops it. `limits` is the negotiated
    /// bootstrap ceiling; `None` uses the spec maximum.
    pub fn decode_shared(
        input: &bytes::Bytes,
        limits: Option<crate::caps::BootstrapLimits>,
    ) -> Result<(Self, bytes::Bytes), DecodeError> {
        let mut decoder = Decoder::on_shared(input, limits);
        let (frame, tail) = decoder.read_frame()?;
        let rest = decoder.share_exact(tail);
        Ok((frame, rest))
    }
}

/// Write one tagged `ResourceId` field. A local id (every id on the live
/// path) is a fixed five bytes written straight into the frame; only a
/// satellite id, with its host string, goes through the scratch builder.
fn id_field(enc: &mut Encoder<'_>, field_id: u32, id: &ResourceId) {
    match id {
        ResourceId::Local { id } => {
            let mut value = [0_u8; 5];
            value[0] = RESOURCE_ID_TAG_LOCAL;
            value[1..].copy_from_slice(&id.to_be_bytes());
            enc.write_field(field_id, &value);
        }
        ResourceId::Satellite { .. } => {
            enc.write_field_with(field_id, |e| encode_terminal_id(id, e));
        }
    }
}

/// Write fields 1-3 (resource, stream, generation) that open every
/// bootstrap and history frame (`docs/spec/L1.md` §4.3).
fn generation_fields(
    enc: &mut Encoder<'_>,
    terminal_id: &ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
) {
    id_field(enc, field::bootstrap_begin::TERMINAL_ID, terminal_id);
    u64_field(enc, field::bootstrap_begin::STREAM_ID, stream_id.get());
    u64_field(
        enc,
        field::bootstrap_begin::BOOTSTRAP_ID,
        bootstrap_id.get(),
    );
}

/// Write one fixed-width `u8` field.
fn u8_field(enc: &mut Encoder<'_>, field_id: u32, value: u8) {
    enc.write_field(field_id, &value.to_be_bytes());
}

/// Write one fixed-width `u16` field.
fn u16_field(enc: &mut Encoder<'_>, field_id: u32, value: u16) {
    enc.write_field(field_id, &value.to_be_bytes());
}

/// Write one fixed-width `u32` field.
fn u32_field(enc: &mut Encoder<'_>, field_id: u32, value: u32) {
    enc.write_field(field_id, &value.to_be_bytes());
}

/// Write one fixed-width `u64` field.
fn u64_field(enc: &mut Encoder<'_>, field_id: u32, value: u64) {
    enc.write_field(field_id, &value.to_be_bytes());
}

/// Write one field whose value is an `i32` as its two's-complement `u32`.
fn write_i32_field(enc: &mut Encoder<'_>, field_id: u32, value: i32) {
    u32_field(enc, field_id, u32::from_be_bytes(value.to_be_bytes()));
}

/// Write an `EVENT` journal stamp (fields 3-6, ADR-0123). `seq` and `ts_ms`
/// always travel together; `actor` and `operation_id` only when set.
fn encode_event_stamp(enc: &mut Encoder<'_>, stamp: &EventStamp) {
    u64_field(enc, field::event::SEQ, stamp.seq);
    u64_field(enc, field::event::TS_MS, stamp.ts_ms);
    if let Some(actor) = &stamp.actor {
        enc.write_field_with(field::event::ACTOR, |e| encode_actor_ref(actor, e));
    }
    if let Some(key) = &stamp.operation_id {
        enc.write_field(field::event::OPERATION_ID, key.as_bytes());
    }
}
