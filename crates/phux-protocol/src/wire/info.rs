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
    canonical_layout_session_id(key.strip_prefix(TUI_LAYOUT_KEY_PREFIX)?)
}

/// Parse the session a `<prefix>.layout/v1/<session-id>` key names.
///
/// `None` when `key` does not match the grammar. The `<session-id>` segment
/// must be the session's **canonical** decimal form — no leading zero
/// (other than `"0"`), no leading `+`, and only ASCII digits.
/// Without this rule, `myapp.layout/v1/07` and `myapp.layout/v1/7` would
/// both name session 7 but compare unequal as strings: the server's reap
/// cleanup matches the literal key `*.layout/v1/7`, so a non-canonical key
/// that slipped past a looser check would never be deleted when its session
/// reaps. `<prefix>` must also not itself contain the `.layout/v1/`
/// separator, so a key with two occurrences (`a.layout/v1/b.layout/v1/7`)
/// is rejected rather than silently matched on its last one.
#[must_use]
pub fn projection_key_session(key: &str) -> Option<SessionId> {
    let (prefix, suffix) = key.rsplit_once(".layout/v1/")?;
    if prefix.is_empty() || prefix.contains(".layout/v1/") {
        return None;
    }
    canonical_layout_session_id(suffix)
}

fn canonical_layout_session_id(digits: &str) -> Option<SessionId> {
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
    decode_counted(dec, len, item)
}

/// Write a `u16`-counted list, each item via `item`. A list longer than
/// `u16::MAX` saturates the count; the decoder then reads the first
/// `u16::MAX` items and skips the rest as trailing bytes.
pub(in crate::wire) fn encode_u16_list<T>(
    items: &[T],
    enc: &mut Encoder<'_>,
    item: impl Fn(&T, &mut Encoder<'_>),
) {
    enc.write_u16_be(u16::try_from(items.len()).unwrap_or(u16::MAX));
    for value in items {
        item(value, enc);
    }
}

/// Read a `u16`-counted list; an over-declared count cannot pre-allocate
/// past the remaining bytes.
pub(in crate::wire) fn decode_u16_list<'a, T>(
    dec: &mut Decoder<'a>,
    item: impl Fn(&mut Decoder<'a>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let count = usize::from(dec.read_u16_be()?);
    decode_counted(dec, count, item)
}

/// Read `count` items, reserving no more than the remaining body can hold.
pub(in crate::wire) fn decode_counted<'a, T>(
    dec: &mut Decoder<'a>,
    count: usize,
    item: impl Fn(&mut Decoder<'a>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let mut out = dec.bounded_capacity(count);
    for _ in 0..count {
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

#[cfg(test)]
mod layout_key_tests {
    use super::*;

    #[test]
    fn projection_key_session_accepts_the_adr_0129_grammar() {
        assert_eq!(
            projection_key_session("phux.tui.layout/v1/7"),
            Some(SessionId::new(7))
        );
        assert_eq!(
            projection_key_session("myapp.layout/v1/7"),
            Some(SessionId::new(7))
        );
        assert_eq!(
            projection_key_session("a.b.c.layout/v1/0"),
            Some(SessionId::new(0))
        );
    }

    #[test]
    fn projection_key_session_rejects_empty_prefix_and_foreign_shapes() {
        assert_eq!(projection_key_session(".layout/v1/7"), None);
        assert_eq!(projection_key_session("not-a-layout-key"), None);
        assert_eq!(projection_key_session("phux.tui.layout/v1"), None);
        assert_eq!(projection_key_session("layout/v1/7"), None);
    }

    #[test]
    fn projection_key_session_id_must_be_canonical_decimal() {
        for non_canonical in [
            "myapp.layout/v1/07",
            "myapp.layout/v1/+7",
            "myapp.layout/v1/7 ",
        ] {
            assert_eq!(
                projection_key_session(non_canonical),
                None,
                "{non_canonical:?} must not resolve to any session"
            );
        }
    }

    #[test]
    fn projection_key_prefix_must_not_contain_the_separator_itself() {
        assert_eq!(projection_key_session("a.layout/v1/b.layout/v1/7"), None);
    }
}
