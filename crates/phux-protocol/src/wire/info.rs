//! Snapshot-graph types delivered with `ATTACHED` (`docs/protocol.md`).
//!
//! Wire mirrors of `phux_core`'s session/window shapes (this crate cannot
//! depend on `phux-core`): enough to render chrome. Layout is not here; it
//! is L3 metadata (`docs/organization.md#records-handles-and-arrangements`, ADR-0030).
//! Terminal contents flow through the bootstrap streams.

use bytes::BytesMut;

use crate::ids::{
    ClientId, ResourceId, ResourceKind, SatelliteHost, SessionId, SessionKey, WindowId,
};
use crate::wire::frame::SessionLifetime;

use super::decode::Decoder;
use super::encode::Encoder;
use super::error::DecodeError;
use super::field;
use super::frame::{
    CloseReason, ResourceLifecycle, decode_optional_i32, decode_terminal_id, encode_optional_i32,
    encode_terminal_id,
};

mod extension;
mod records;
mod rows;
mod snapshot;
mod snapshot_codec;

use extension::{decode_snapshot_extension, encode_snapshot_extension};
pub use records::{AgentFacet, ExitFacet, ResourceInfo, SessionInfo, WindowInfo};
pub use rows::MAX_LAYOUT_DEPTH;
use rows::{SESSION_FACET_KEEP_EMPTY, decode_resource_facets, encode_resource_facets};
pub(super) use rows::{
    decode_session_info, decode_terminal_info, decode_window_info, encode_session_info,
    encode_terminal_info, encode_window_info,
};
pub use snapshot::{HostInventory, HostSessionInfo, SessionSnapshot};
use snapshot_codec::boxed_hosts;
pub(super) use snapshot_codec::{decode_session_snapshot, encode_session_snapshot};

/// Tag byte of a leaf in the retired `WindowInfo` layout slot.
const LAYOUT_TAG_LEAF: u8 = 0;
/// Tag byte of a split in the retired `WindowInfo` layout slot.
const LAYOUT_TAG_SPLIT: u8 = 1;

/// Prefix of the TUI's per-session layout key.
///
/// The key is `phux.tui.layout/v1/<session-id>`, the wire `SessionId` in
/// decimal. A server advertising
/// [`ServerFeatureExt::Organization`](crate::caps::ServerFeatureExt::Organization)
/// translates this key to and from the session's arrangement; every other
/// layout key stays opaque metadata.
pub const TUI_LAYOUT_KEY_PREFIX: &str = "phux.tui.layout/v1/";

/// The TUI layout key of `session`: [`TUI_LAYOUT_KEY_PREFIX`] then its id.
#[must_use]
pub fn tui_layout_key(session: SessionId) -> String {
    format!("{TUI_LAYOUT_KEY_PREFIX}{}", session.get())
}

/// The session a [`TUI_LAYOUT_KEY_PREFIX`] key names: `None` unless the rest
/// is a canonical `u32` decimal (no sign, no leading zero).
#[must_use]
pub fn tui_layout_key_session(key: &str) -> Option<SessionId> {
    let digits = key.strip_prefix(TUI_LAYOUT_KEY_PREFIX)?;
    let canonical = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'));
    canonical
        .then(|| digits.parse().ok())
        .flatten()
        .map(SessionId::new)
}

// Positional encoding helpers.

// Presence-byte option and `u32` list-length helpers.

pub(super) fn encode_option_window_id(value: Option<WindowId>, enc: &mut Encoder<'_>) {
    enc.write_option(value, |e, id| e.write_u32_be(id.get()));
}

pub(super) fn decode_option_window_id(
    dec: &mut Decoder<'_>,
) -> Result<Option<WindowId>, DecodeError> {
    dec.read_option("Option<WindowId> tag", |d| {
        Ok(WindowId::new(d.read_u32_be()?))
    })
}

pub(super) fn encode_option_terminal_id(value: Option<&ResourceId>, enc: &mut Encoder<'_>) {
    enc.write_option(value, |e, id| encode_terminal_id(id, e));
}

pub(super) fn decode_option_terminal_id(
    dec: &mut Decoder<'_>,
) -> Result<Option<ResourceId>, DecodeError> {
    dec.read_option("Option<ResourceId> tag", decode_terminal_id)
}

pub(in crate::wire) fn encode_option_str(value: Option<&str>, enc: &mut Encoder<'_>) {
    enc.write_option(value, Encoder::write_str);
}

pub(in crate::wire) fn decode_option_str<'a>(
    dec: &mut Decoder<'a>,
) -> Result<Option<&'a str>, DecodeError> {
    dec.read_option("Option<str> tag", Decoder::read_str)
}

pub(super) fn encode_list_len(len: usize, enc: &mut Encoder<'_>) {
    debug_assert!(
        u32::try_from(len).is_ok(),
        "list length exceeds u32 (positional encoding cap)",
    );
    let len_u32 = u32::try_from(len).unwrap_or(u32::MAX);
    enc.write_u32_be(len_u32);
}

/// Write a `u32`-counted list, each item via `item`.
fn encode_list<T>(items: &[T], enc: &mut Encoder<'_>, item: impl Fn(&T, &mut Encoder<'_>)) {
    encode_list_len(items.len(), enc);
    for value in items {
        item(value, enc);
    }
}

/// Read a `u32`-counted list; an over-declared count cannot pre-allocate
/// past the remaining bytes.
fn decode_list<'a, T>(
    dec: &mut Decoder<'a>,
    item: impl Fn(&mut Decoder<'a>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let len = decode_list_len(dec)?;
    let mut out = dec.bounded_capacity(len);
    for _ in 0..len {
        out.push(item(dec)?);
    }
    Ok(out)
}

pub(super) fn decode_list_len(dec: &mut Decoder<'_>) -> Result<usize, DecodeError> {
    let len = dec.read_u32_be()?;
    usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)
}

// The one `ClientId` wire codec.

pub(super) fn encode_client_id(id: ClientId, enc: &mut Encoder<'_>) {
    enc.write_u32_be(id.get());
}

pub(super) fn decode_client_id(dec: &mut Decoder<'_>) -> Result<ClientId, DecodeError> {
    Ok(ClientId::new(dec.read_u32_be()?))
}
