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
    ($($ty:ty;)*) => {$(
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
    u8;
    u16;
    u32;
    u64;
    i32;
}

/// Implement [`TlvValue`] for a positional sub-record written by `$encode`
/// into a scratch builder and read by `$decode`.
macro_rules! tlv_positional {
    ($($ty:ty, $encode:path, $decode:path;)*) => {$(
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
    Vec<String>, encode_string_list, decode_string_list;
    Scope, encode_scope, decode_scope;
    ActorRef, encode_actor_ref, decode_actor_ref;
    AttachTarget, encode_attach_target, decode_attach_target;
    ViewportInfo, encode_viewport_info, decode_viewport_info;
    SessionSnapshot, encode_session_snapshot, decode_session_snapshot;
    MoveResult, encode_move_result, decode_move_result;
    CommandResult, encode_command_result, decode_command_result;
    KeyEvent, encode_key_event, decode_key_event;
    MouseEvent, encode_mouse_event, decode_mouse_event;
    PasteEvent, encode_paste_event, decode_paste_event;
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

/// Six bytes: foreground then background RGB, as the `HELLO` palette.
impl TlvValue for crate::caps::TerminalDefaultColors {
    fn put(&self, enc: &mut Encoder<'_>, id: u32) {
        let (fg, bg) = (self.foreground, self.background);
        enc.write_field(id, &[fg.r, fg.g, fg.b, bg.r, bg.g, bg.b]);
    }

    fn take(value: &[u8], _field: &'static str) -> Result<Self, DecodeError> {
        let [r, g, b, br, bgr, bb] = Decoder::new(value).read_array()?;
        Ok(Self {
            foreground: crate::caps::TerminalColor { r, g, b },
            background: crate::caps::TerminalColor {
                r: br,
                g: bgr,
                b: bb,
            },
        })
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

/// Declare a frame's field-id module and, for a regular frame, its body
/// codec.
///
/// The module keeps the exact `pub const NAME: u32 = id;` lines a reader
/// greps for. A regular frame is one whose every wire field maps 1:1 to
/// one variant field: its module names the variants it serves
/// (`for Variant => decoder_method, ...`) and each const gains
/// `=> presence field: Type`, where `Type: TlvValue` and presence is:
///
/// - `required`: always written; absent on decode is `UnexpectedEof`.
/// - `default`: always written; absent on decode is `Type::default()`.
/// - `optional`: the variant field is `Option<Type>`, written only when
///   `Some`; absent decodes as `None`.
///
/// Every module also lists its fields as `FIELDS`, the table the wire
/// schema test names fields by. For a regular frame the macro then emits
/// `encode(frame, enc)`, which writes the fields in declaration order (the
/// bytes are the contract), and one `Decoder` method per variant, which
/// reads fields in wire order: the last of a
/// repeated id wins, an unknown id is skipped by its length, and each
/// value is checked by its [`TlvValue::take`] before it is kept. A frame
/// with any other shape keeps a hand-written codec and lists only its
/// consts.
macro_rules! tlv_body {
    (
        $(#[$doc:meta])*
        pub mod $module:ident for $($variant:ident => $decode:ident),+ {
            $(
                $(#[$fdoc:meta])*
                pub const $name:ident: u32 = $id:literal => $mode:ident $field:ident: $ty:ty;
            )*
        }
    ) => {
        $(#[$doc])*
        pub mod $module {
            $(
                $(#[$fdoc])*
                pub const $name: u32 = $id;
            )*

            tlv_body!(@fields $($name = $id),*);

            tlv_body!(@encode_arms [$($variant),+] { $($field),* } {
                $($name $mode $field;)*
            });
        }

        tlv_body!(@decoders $module [$($variant => $decode),+] {
            $($name $mode $field: $ty;)*
        });
    };
    (
        $(#[$doc:meta])*
        pub mod $module:ident {
            $(
                $(#[$fdoc:meta])*
                pub const $name:ident: u32 = $id:literal;
            )*
        }
    ) => {
        $(#[$doc])*
        pub mod $module {
            $(
                $(#[$fdoc])*
                pub const $name: u32 = $id;
            )*

            tlv_body!(@fields $($name = $id),*);
        }
    };
    (@fields $($name:ident = $id:literal),*) => {
        /// `(NAME, id)` of each field, in declaration order: what
        /// `docs/spec/wire-schema.json` names this frame's fields by.
        #[cfg(test)]
        pub(crate) const FIELDS: &[(&str, u32)] = &[$((stringify!($name), $id)),*];
    };
    (@encode_arms [$($variant:ident),+] $pat:tt $fields:tt) => {
        tlv_body!(@encode [$($crate::wire::frame::FrameKind::$variant $pat)|+] $fields);
    };
    (@encode [$($arm:tt)*] { $($name:ident $mode:ident $field:ident;)* }) => {
        /// Write this frame's body: every field, in declaration order.
        pub(crate) fn encode(
            frame: &$crate::wire::frame::FrameKind,
            enc: &mut $crate::wire::encode::Encoder<'_>,
        ) {
            // The parentheses group a shared body's or-pattern.
            #[allow(unused_parens)]
            let ($($arm)*) = frame else {
                unreachable!("encode_payload routes only this module's frames here");
            };
            $(tlv_body!(@put $mode enc $name $field);)*
        }
    };
    (@decoders $module:ident [$($variant:ident => $decode:ident),+] $fields:tt) => {
        $(tlv_body!(@decode $module $variant $decode $fields);)+
    };
    (@decode $module:ident $variant:ident $decode:ident {
        $($name:ident $mode:ident $field:ident: $ty:ty;)*
    }) => {
        impl $crate::wire::decode::Decoder<'_> {
            pub(in crate::wire) fn $decode(
                &mut self,
            ) -> Result<$crate::wire::frame::FrameKind, $crate::wire::error::DecodeError> {
                $(let mut $field = tlv_body!(@init $mode $ty);)*
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        $($module::$name => {
                            let label = concat!(stringify!($variant), ".", stringify!($field));
                            $field = tlv_body!(@take $mode $ty, value, label);
                        })*
                        _ => {}
                    }
                }
                Ok($crate::wire::frame::FrameKind::$variant {
                    $($field: tlv_body!(@finish $mode $field),)*
                })
            }
        }
    };
    (@put optional $enc:ident $name:ident $field:ident) => {
        if let Some(value) = $field {
            $crate::wire::tlv::TlvValue::put(value, $enc, $name);
        }
    };
    (@put required $enc:ident $name:ident $field:ident) => {
        $crate::wire::tlv::TlvValue::put($field, $enc, $name);
    };
    (@put default $enc:ident $name:ident $field:ident) => {
        $crate::wire::tlv::TlvValue::put($field, $enc, $name);
    };
    (@init required $ty:ty) => {
        None::<$ty>
    };
    (@init optional $ty:ty) => {
        None::<$ty>
    };
    (@init default $ty:ty) => {
        <$ty as Default>::default()
    };
    (@take required $ty:ty, $value:ident, $label:ident) => {
        Some(<$ty as $crate::wire::tlv::TlvValue>::take($value, $label)?)
    };
    (@take optional $ty:ty, $value:ident, $label:ident) => {
        Some(<$ty as $crate::wire::tlv::TlvValue>::take($value, $label)?)
    };
    (@take default $ty:ty, $value:ident, $label:ident) => {
        <$ty as $crate::wire::tlv::TlvValue>::take($value, $label)?
    };
    (@finish required $field:ident) => {
        $field.ok_or($crate::wire::error::DecodeError::UnexpectedEof)?
    };
    (@finish optional $field:ident) => {
        $field
    };
    (@finish default $field:ident) => {
        $field
    };
}
