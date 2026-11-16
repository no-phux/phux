//! [`FrameKind`] — the decoded wire frame — with its type-byte table and
//! its encode/decode entry points.

use bytes::BytesMut;

use crate::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities, Compression,
    ServerCapabilities,
};
use crate::ids::{BootstrapId, ClientId, GroupId, ResourceId, SatelliteHost, StreamId};
use crate::input::InputEvent;
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::{ClipboardData, PasteEvent};
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;
use crate::wire::framing::LENGTH_PREFIX_LEN;
use crate::wire::info::SessionSnapshot;
use crate::wire::tlv::TlvValue;

use super::directory::DirectoryListingResult;
use super::path::PathQueryResult;
use super::{
    ActorRef, AgentEvent, AttachTarget, CloseReason, Command, CommandResult, DetachReason,
    ErrorCode, EventStamp, HistoryRejectionReason, HistoryTombstoneReason, MAX_FRAME_LEN,
    MoveResult, Scope, SpawnResource, SpawnResult, TYPE_FRAME_COMPRESSED, TombstoneReason,
    ViewportInfo, encode_actor_ref,
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
/// Decoded wire frame (`docs/protocol.md`).
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

/// Generate the per-variant tables of [`FrameKind`] from the frame catalog.
macro_rules! frame_kind_tables {
    ($(
        $(#[$attr:meta])*
        $variant:ident = $name:ident $tag:literal, $spec:literal, $module:literal, $decode:ident;
    )*) => {
        /// Each variant's row in the frame catalog, numbered `0..` in row
        /// order: the order `docs/spec/wire-schema.json` lists frames in.
        #[cfg(test)]
        #[derive(Clone, Copy)]
        enum CatalogRow {
            $($variant,)*
        }

        impl FrameKind {
            /// Number of [`FrameKind`] variants: one per frame catalog row.
            #[cfg(test)]
            pub(crate) const VARIANTS: usize = [$(CatalogRow::$variant),*].len();

            /// Type discriminant from `docs/protocol.md`.
            #[must_use]
            pub const fn type_byte(&self) -> u8 {
                match self {
                    $(Self::$variant { .. } => crate::wire::frame::$name,)*
                }
            }

            /// This variant's frame catalog row, below [`Self::VARIANTS`].
            #[cfg(test)]
            pub(crate) const fn catalog_row(&self) -> usize {
                match self {
                    $(Self::$variant { .. } => CatalogRow::$variant as usize,)*
                }
            }

            /// The frame's name in `docs/protocol.md`, such as `HELLO_OK`.
            #[cfg(test)]
            pub(crate) const fn spec_name(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $spec,)*
                }
            }

            /// The `wire/field.rs` module naming this frame's fields; empty
            /// for a frame without fields.
            #[cfg(test)]
            pub(crate) const fn field_module(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $module,)*
                }
            }
        }
    };
}

frame_catalog!(frame_kind_tables);

impl FrameKind {
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
        enc.write_u8_field(field::frame_compressed::ALGORITHM, compression.as_u8());
        enc.write_u32_field(field::frame_compressed::UNCOMPRESSED_LEN, uncompressed_len);
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

/// Write one tagged `ResourceId` field.
fn id_field(enc: &mut Encoder<'_>, field_id: u32, id: &ResourceId) {
    id.put(enc, field_id);
}

/// Write fields 1-3 (resource, stream, generation) that open every
/// bootstrap and history frame (`docs/protocol.md#attach-and-output`).
fn generation_fields(
    enc: &mut Encoder<'_>,
    terminal_id: &ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
) {
    id_field(enc, field::bootstrap_begin::TERMINAL_ID, terminal_id);
    enc.write_u64_field(field::bootstrap_begin::STREAM_ID, stream_id.get());
    enc.write_u64_field(field::bootstrap_begin::BOOTSTRAP_ID, bootstrap_id.get());
}

/// Write an `EVENT` journal stamp (fields 3-6, ADR-0123). `seq` and `ts_ms`
/// always travel together; `actor` and `operation_id` only when set.
fn encode_event_stamp(enc: &mut Encoder<'_>, stamp: &EventStamp) {
    enc.write_u64_field(field::event::SEQ, stamp.seq);
    enc.write_u64_field(field::event::TS_MS, stamp.ts_ms);
    if let Some(actor) = &stamp.actor {
        enc.write_field_with(field::event::ACTOR, |e| encode_actor_ref(actor, e));
    }
    if let Some(key) = &stamp.operation_id {
        enc.write_field(field::event::OPERATION_ID, key.as_bytes());
    }
}
