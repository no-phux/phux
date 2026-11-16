//! One body-level TLV field value: how a Rust type is written into a
//! field and read back out of one (`docs/protocol.md#framing`).
//!
//! [`TlvValue`] is the single writer and reader of each value shape the
//! frame bodies carry. The `Encoder::write_*_field` helpers delegate to it,
//! and [`tlv_body!`] builds whole frame bodies from it.

use crate::ids::{BootstrapId, ClientId, RESOURCE_ID_TAG_LOCAL, ResourceId, StreamId};
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::{ClipboardData, PasteEvent};
use crate::wire::decode::{Decoder, utf8_value};
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::frame::{
    ActorRef, AttachTarget, CommandResult, ErrorCode, MAX_CLIPBOARD_TEXT, MoveResult, RolePolicy,
    Scope, ViewportInfo, decode_actor_ref, decode_attach_target, decode_bootstrap_id,
    decode_command_result, decode_focus_event, decode_key_event, decode_mouse_event,
    decode_move_result, decode_paste_event, decode_scope, decode_stream_id, decode_string_list,
    decode_terminal_id, decode_viewport_info, encode_actor_ref, encode_attach_target,
    encode_command_result, encode_focus_event, encode_key_event, encode_mouse_event,
    encode_move_result, encode_paste_event, encode_scope, encode_string_list, encode_terminal_id,
    encode_viewport_info,
};
use crate::wire::info::{
    SessionSnapshot, decode_client_id, decode_session_snapshot, encode_client_id,
    encode_session_snapshot,
};

/// A value carried whole in one body-level TLV field.
pub(super) trait TlvValue: Sized {
    /// Write `self` as the value of field `id`.
    fn put(&self, enc: &mut Encoder<'_>, id: u32);

    /// Read a value from a field's bytes. `field` names the frame field in
    /// an unknown-value error (`Frame.field`).
    fn take(value: &[u8], field: &'static str) -> Result<Self, DecodeError>;
}

/// A value decoded positionally from a fresh [`Decoder`] over the field's
/// bytes, so a malformed nested value cannot read past its field.
fn positional<T>(
    value: &[u8],
    decode: impl FnOnce(&mut Decoder<'_>) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
    decode(&mut Decoder::new(value))
}

/// Implement [`TlvValue`] for a big-endian integer carried as its bytes.
/// Bytes after the integer are ignored; fewer is `UnexpectedEof`.
macro_rules! tlv_integer {
    ($($ty:ty => $schema:literal;)*) => {$(
        impl TlvValue for $ty {

            fn put(&self, enc: &mut Encoder<'_>, id: u32) {
                enc.write_field(id, &self.to_be_bytes());
            }

            fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
                Decoder::new(value).read_array().map(<$ty>::from_be_bytes)
            }
        }
    )*};
}

// An `i32` rides as its two's-complement `u32`.
tlv_integer! {
    u8 => "u8";
    u16 => "u16";
    u32 => "u32";
    u64 => "u64";
    i32 => "i32";
}

/// Implement [`TlvValue`] for a positional sub-record written by `$encode`
/// into a scratch builder and read by `$decode`.
macro_rules! tlv_positional {
    ($($ty:ty => $schema:literal, $encode:path, $decode:path;)*) => {$(
        impl TlvValue for $ty {

            fn put(&self, enc: &mut Encoder<'_>, id: u32) {
                enc.write_field_with(id, |e| $encode(self, e));
            }

            fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
                positional(value, $decode)
            }
        }
    )*};
}

tlv_positional! {
    Vec<String> => "StringList", encode_string_list, decode_string_list;
    Scope => "Scope", encode_scope, decode_scope;
    ActorRef => "ActorRef", encode_actor_ref, decode_actor_ref;
    AttachTarget => "AttachTarget", encode_attach_target, decode_attach_target;
    ViewportInfo => "ViewportInfo", encode_viewport_info, decode_viewport_info;
    SessionSnapshot => "SessionSnapshot", encode_session_snapshot, decode_session_snapshot;
    MoveResult => "MoveResult", encode_move_result, decode_move_result;
    CommandResult => "CommandResult", encode_command_result, decode_command_result;
    KeyEvent => "KeyEvent", encode_key_event, decode_key_event;
    MouseEvent => "MouseEvent", encode_mouse_event, decode_mouse_event;
    PasteEvent => "PasteEvent", encode_paste_event, decode_paste_event;
}

impl TlvValue for bool {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        u8::from(*self).put(enc, id);
    }

    /// `0` or `1`; any other byte is `UnknownEnumValue { field }`.
    fn take(value: &[u8], field: &'static str) -> Result<Self, DecodeError> {
        Decoder::new(value).read_bool(field)
    }
}

/// Raw UTF-8: the TLV header already delimits the string.
impl TlvValue for String {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        enc.write_field(id, self.as_bytes());
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        utf8_value(value)
    }
}

/// Opaque bytes, copied out of the field.
impl TlvValue for Vec<u8> {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        enc.write_field(id, self);
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        Ok(value.to_vec())
    }
}

impl TlvValue for ResourceId {
    /// A local id (every id on the live path) is a fixed five bytes written
    /// straight into the frame; only a satellite id, with its host string,
    /// goes through the scratch builder.
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        match self {
            Self::Local { id: local } => {
                let mut value = [0_u8; 5];
                value[0] = RESOURCE_ID_TAG_LOCAL;
                value[1..].copy_from_slice(&local.to_be_bytes());
                enc.write_field(id, &value);
            }
            Self::Satellite { .. } => {
                enc.write_field_with(id, |e| encode_terminal_id(self, e));
            }
        }
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        positional(value, decode_terminal_id)
    }
}

/// A non-zero `u64`; zero is `InvalidStreamId`.
impl TlvValue for StreamId {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        self.get().put(enc, id);
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        positional(value, decode_stream_id)
    }
}

/// A non-zero `u64`; zero is `InvalidBootstrapId`.
impl TlvValue for BootstrapId {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        self.get().put(enc, id);
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        positional(value, decode_bootstrap_id)
    }
}

impl TlvValue for ClientId {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        enc.write_field_with(id, |e| encode_client_id(*self, e));
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        positional(value, decode_client_id)
    }
}

/// Focus kind (`u8`: gained=0 / lost=1).
impl TlvValue for FocusEvent {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        encode_focus_event(*self).put(enc, id);
    }

    fn take(value: &[u8], field: &'static str) -> Result<Self, DecodeError> {
        decode_focus_event(u8::take(value, field)?)
    }
}

/// `role_policy: u8` (ADR-0127); a reserved bit is an unknown value.
impl TlvValue for RolePolicy {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        self.to_u8().put(enc, id);
    }

    fn take(value: &[u8], field: &'static str) -> Result<Self, DecodeError> {
        Self::from_wire(u8::take(value, field)?)
    }
}

/// `ErrorCode` (`u16`); a code this build does not define is kept.
impl TlvValue for ErrorCode {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        self.as_wire().put(enc, id);
    }

    fn take(value: &[u8], field: &'static str) -> Result<Self, DecodeError> {
        u16::take(value, field).map(Self::from_wire_or_unknown)
    }
}

/// UTF-8 clipboard text, at most [`MAX_CLIPBOARD_TEXT`] bytes, checked
/// before the text is copied.
impl TlvValue for ClipboardData {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        enc.write_field(id, self.0.as_bytes());
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        if value.len() > MAX_CLIPBOARD_TEXT {
            return Err(DecodeError::LengthOverflow);
        }
        utf8_value(value).map(ClipboardData)
    }
}
