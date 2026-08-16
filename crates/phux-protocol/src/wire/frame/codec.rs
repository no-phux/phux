//! Sub-record codec helpers shared by the frame encoder and the wire
//! decoder.

use crate::caps::{
    BootstrapCodec, BootstrapProfile, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use crate::ids::{
    BootstrapId, GroupId, SatelliteHost, SessionId, StreamId, TERMINAL_ID_TAG_LOCAL,
    TERMINAL_ID_TAG_SATELLITE, TerminalId,
};
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::PasteEvent;
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;

use super::{
    ATTACH_TARGET_BY_ID, ATTACH_TARGET_BY_NAME, ATTACH_TARGET_CREATE_IF_MISSING,
    ATTACH_TARGET_LAST, AttachTarget, MOVE_ERROR_TAG_MOVE_FAILED,
    MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE, MOVE_RESULT_ERR, MOVE_RESULT_OK, MoveError,
    MoveResult, SCOPE_TAG_GLOBAL, SCOPE_TAG_GROUP, SCOPE_TAG_TERMINAL,
    SPAWN_ERROR_TAG_GROUP_NOT_FOUND, SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE,
    SPAWN_ERROR_TAG_SPAWN_FAILED, SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE, SPAWN_RESULT_ERR,
    SPAWN_RESULT_OK, Scope, SpawnError, SpawnResult, ViewportInfo,
};

// -----------------------------------------------------------------------------
// Helpers for the message-catalog variants. Kept in this file so encoder and
// decoder share one source of truth for sub-record layout.
// -----------------------------------------------------------------------------
pub(in crate::wire) fn encode_bootstrap_codec(codec: BootstrapCodec, enc: &mut Encoder<'_>) {
    match codec {
        BootstrapCodec::SynthesizedVtV1 => {
            enc.write_u8(BootstrapCodec::SYNTHESIZED_VT_V1_TAG);
        }
        BootstrapCodec::Native(version) => {
            enc.write_u8(BootstrapCodec::NATIVE_TAG);
            enc.write_u8(version.as_wire());
        }
    }
}

pub(in crate::wire) fn decode_bootstrap_codec(
    dec: &mut Decoder<'_>,
) -> Result<BootstrapCodec, DecodeError> {
    match dec.read_u8()? {
        BootstrapCodec::SYNTHESIZED_VT_V1_TAG => Ok(BootstrapCodec::SynthesizedVtV1),
        BootstrapCodec::NATIVE_TAG => {
            let value = dec.read_u8()?;
            let codec =
                EngineCodec::from_wire(value).ok_or_else(|| DecodeError::UnknownEnumValue {
                    field: "EngineCodec",
                    value: u32::from(value),
                })?;
            Ok(BootstrapCodec::Native(codec))
        }
        value => Err(DecodeError::UnknownEnumValue {
            field: "BootstrapCodec",
            value: u32::from(value),
        }),
    }
}

pub(in crate::wire) fn encode_bootstrap_profile(profile: BootstrapProfile, enc: &mut Encoder<'_>) {
    match profile {
        BootstrapProfile::NativeState { codec, features } => {
            enc.write_u8(BootstrapProfile::NATIVE_STATE_TAG);
            enc.write_u8(codec.as_wire());
            enc.write_u32_be(features.as_wire());
        }
        BootstrapProfile::SynthesizedVtRaw => {
            enc.write_u8(BootstrapProfile::SYNTHESIZED_VT_RAW_TAG);
        }
        BootstrapProfile::SynthesizedVtStateSync => {
            enc.write_u8(BootstrapProfile::SYNTHESIZED_VT_STATE_SYNC_TAG);
        }
    }
}

pub(in crate::wire) fn decode_bootstrap_profile(
    dec: &mut Decoder<'_>,
) -> Result<BootstrapProfile, DecodeError> {
    match dec.read_u8()? {
        BootstrapProfile::NATIVE_STATE_TAG => {
            let value = dec.read_u8()?;
            let codec =
                EngineCodec::from_wire(value).ok_or_else(|| DecodeError::UnknownEnumValue {
                    field: "EngineCodec",
                    value: u32::from(value),
                })?;
            let features = EngineFeatureSet::from_wire(dec.read_u32_be()?);
            if !features.supports_native() {
                return Err(DecodeError::InvalidBootstrapProfile);
            }
            Ok(BootstrapProfile::NativeState { codec, features })
        }
        BootstrapProfile::SYNTHESIZED_VT_RAW_TAG => Ok(BootstrapProfile::SynthesizedVtRaw),
        BootstrapProfile::SYNTHESIZED_VT_STATE_SYNC_TAG => {
            Ok(BootstrapProfile::SynthesizedVtStateSync)
        }
        value => Err(DecodeError::UnknownEnumValue {
            field: "BootstrapProfile",
            value: u32::from(value),
        }),
    }
}

pub(in crate::wire) const fn decode_bootstrap_stream_profile(
    codec: BootstrapCodec,
    output_mode: u8,
) -> Result<BootstrapStreamProfile, DecodeError> {
    match (codec, output_mode) {
        (BootstrapCodec::Native(codec), 0) => Ok(BootstrapStreamProfile::NativeState { codec }),
        (BootstrapCodec::SynthesizedVtV1, 0) => Ok(BootstrapStreamProfile::SynthesizedVtRaw),
        (BootstrapCodec::SynthesizedVtV1, 1) => Ok(BootstrapStreamProfile::SynthesizedVtStateSync),
        _ => Err(DecodeError::InvalidBootstrapProfile),
    }
}

pub(in crate::wire) fn decode_stream_id(dec: &mut Decoder<'_>) -> Result<StreamId, DecodeError> {
    StreamId::new(dec.read_u64_be()?).ok_or(DecodeError::InvalidStreamId)
}

pub(in crate::wire) fn decode_bootstrap_id(
    dec: &mut Decoder<'_>,
) -> Result<BootstrapId, DecodeError> {
    BootstrapId::new(dec.read_u64_be()?).ok_or(DecodeError::InvalidBootstrapId)
}

pub(in crate::wire) fn encode_attach_target(target: &AttachTarget, enc: &mut Encoder<'_>) {
    match target {
        AttachTarget::Last => {
            enc.write_u8(ATTACH_TARGET_LAST);
        }
        AttachTarget::ByName(name) => {
            enc.write_u8(ATTACH_TARGET_BY_NAME);
            enc.write_str(name);
        }
        AttachTarget::ById(id) => {
            enc.write_u8(ATTACH_TARGET_BY_ID);
            enc.write_u32_be(id.get());
        }
        AttachTarget::CreateIfMissing { name, command, cwd } => {
            enc.write_u8(ATTACH_TARGET_CREATE_IF_MISSING);
            enc.write_str(name);
            encode_optional_string_list(command.as_deref(), enc);
            encode_optional_str(cwd.as_deref(), enc);
        }
    }
}

pub(in crate::wire) fn decode_attach_target(
    dec: &mut Decoder<'_>,
) -> Result<AttachTarget, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        ATTACH_TARGET_LAST => Ok(AttachTarget::Last),
        ATTACH_TARGET_BY_NAME => Ok(AttachTarget::ByName(dec.read_str()?.to_owned())),
        ATTACH_TARGET_BY_ID => Ok(AttachTarget::ById(SessionId::new(dec.read_u32_be()?))),
        ATTACH_TARGET_CREATE_IF_MISSING => {
            let name = dec.read_str()?.to_owned();
            let command = decode_optional_string_list(dec)?;
            let cwd = decode_optional_str(dec)?.map(str::to_owned);
            Ok(AttachTarget::CreateIfMissing { name, command, cwd })
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "AttachTarget",
            value: u32::from(other),
        }),
    }
}

pub(in crate::wire) fn encode_viewport_info(v: &ViewportInfo, enc: &mut Encoder<'_>) {
    enc.write_u16_be(v.cols);
    enc.write_u16_be(v.rows);
    encode_optional_u16(v.pixel_w, enc);
    encode_optional_u16(v.pixel_h, enc);
}

pub(in crate::wire) fn decode_viewport_info(
    dec: &mut Decoder<'_>,
) -> Result<ViewportInfo, DecodeError> {
    let cols = dec.read_u16_be()?;
    let rows = dec.read_u16_be()?;
    let pixel_w = decode_optional_u16(dec)?;
    let pixel_h = decode_optional_u16(dec)?;
    Ok(ViewportInfo {
        cols,
        rows,
        pixel_w,
        pixel_h,
    })
}

pub(in crate::wire) const fn encode_focus_event(event: FocusEvent) -> u8 {
    match event {
        FocusEvent::Gained => 0,
        FocusEvent::Lost => 1,
    }
}

pub(in crate::wire) fn decode_focus_event(tag: u8) -> Result<FocusEvent, DecodeError> {
    match tag {
        0 => Ok(FocusEvent::Gained),
        1 => Ok(FocusEvent::Lost),
        other => Err(DecodeError::UnknownEnumValue {
            field: "FocusEvent",
            value: u32::from(other),
        }),
    }
}

pub(in crate::wire) fn encode_key_event(event: &KeyEvent, enc: &mut Encoder<'_>) {
    // `KeyAction`/`PhysicalKey` are phux-owned `#[repr(u32)]` enums (ADR-0024);
    // cast to the discriminant; the decoder round-trips via `TryFrom<u32>`.
    enc.write_u32_be(event.action as u32);
    enc.write_u32_be(event.key as u32);
    enc.write_u16_be(event.mods.bits());
    enc.write_u16_be(event.consumed_mods.bits());
    enc.write_u8(u8::from(event.composing));
    encode_optional_str(event.text.as_deref(), enc);
    encode_optional_u32(event.unshifted_codepoint, enc);
}

pub(in crate::wire) fn decode_key_event(dec: &mut Decoder<'_>) -> Result<KeyEvent, DecodeError> {
    use crate::input::key::{KeyAction, ModSet, PhysicalKey};

    let action_raw = dec.read_u32_be()?;
    let action = KeyAction::try_from(action_raw).map_err(|_| DecodeError::UnknownEnumValue {
        field: "KeyAction",
        value: action_raw,
    })?;
    let key_raw = dec.read_u32_be()?;
    let key = PhysicalKey::try_from(key_raw).map_err(|_| DecodeError::UnknownEnumValue {
        field: "PhysicalKey",
        value: key_raw,
    })?;
    let mods = ModSet::from_bits_truncate(dec.read_u16_be()?);
    let consumed_mods = ModSet::from_bits_truncate(dec.read_u16_be()?);
    let composing = dec.read_u8()? != 0;
    let text = decode_optional_str(dec)?.map(str::to_owned);
    let unshifted_codepoint = decode_optional_u32(dec)?;
    Ok(KeyEvent {
        action,
        key,
        mods,
        consumed_mods,
        composing,
        text,
        unshifted_codepoint,
    })
}

pub(in crate::wire) fn encode_mouse_event(event: &MouseEvent, enc: &mut Encoder<'_>) {
    enc.write_u32_be(event.action as u32);
    enc.write_u32_be(event.button as u32);
    enc.write_u16_be(event.mods.bits());
    enc.write_f64_be(event.x);
    enc.write_f64_be(event.y);
}

pub(in crate::wire) fn decode_mouse_event(
    dec: &mut Decoder<'_>,
) -> Result<MouseEvent, DecodeError> {
    use crate::input::key::ModSet;
    use crate::input::mouse::{MouseAction, MouseButton};

    let action_raw = dec.read_u32_be()?;
    let action = MouseAction::try_from(action_raw).map_err(|_| DecodeError::UnknownEnumValue {
        field: "MouseAction",
        value: action_raw,
    })?;
    let button_raw = dec.read_u32_be()?;
    let button = MouseButton::try_from(button_raw).map_err(|_| DecodeError::UnknownEnumValue {
        field: "MouseButton",
        value: button_raw,
    })?;
    let mods = ModSet::from_bits_truncate(dec.read_u16_be()?);
    let x = dec.read_f64_be()?;
    let y = dec.read_f64_be()?;
    Ok(MouseEvent {
        action,
        button,
        mods,
        x,
        y,
    })
}

pub(in crate::wire) fn encode_paste_event(event: &PasteEvent, enc: &mut Encoder<'_>) {
    enc.write_u8(event.trust as u8);
    enc.write_bytes(&event.data);
}

pub(in crate::wire) fn decode_paste_event(
    dec: &mut Decoder<'_>,
) -> Result<PasteEvent, DecodeError> {
    use crate::input::paste::PasteTrust;
    let trust_tag = dec.read_u8()?;
    let trust = match trust_tag {
        0 => PasteTrust::Trusted,
        1 => PasteTrust::Untrusted,
        other => {
            return Err(DecodeError::UnknownEnumValue {
                field: "PasteTrust",
                value: u32::from(other),
            });
        }
    };
    let data = dec.read_bytes()?.to_vec();
    Ok(PasteEvent { trust, data })
}

// -----------------------------------------------------------------------------
// `TerminalId` tagged-union codec — ADR-0016 §Decision (phux-vp0.4).
//
// Every `TerminalId` on the wire is prefixed with a 1-byte tag:
//
//   tag = 0  → Local      { id: u32 }
//   tag = 1  → Satellite  { host: str, id: u32 }
//
// v0.1 encoders only produce tag=0. v0.1 decoders MUST accept tag=1; the
// dispatch layer (in `phux-server`) responds with `ERROR
// { UnsupportedSatelliteRoute }` (SPEC §14) when the server is not a
// federation hub. Unknown tags surface as `DecodeError::UnknownEnumValue`.
// -----------------------------------------------------------------------------

/// Encode a [`TerminalId`] including its discriminant byte.
pub(in crate::wire) fn encode_terminal_id(id: &TerminalId, enc: &mut Encoder<'_>) {
    match id {
        TerminalId::Local { id } => {
            enc.write_u8(TERMINAL_ID_TAG_LOCAL);
            enc.write_u32_be(*id);
        }
        TerminalId::Satellite { host, id } => {
            enc.write_u8(TERMINAL_ID_TAG_SATELLITE);
            enc.write_str(host.as_str());
            enc.write_u32_be(*id);
        }
    }
}

/// Decode a [`TerminalId`] previously written by [`encode_terminal_id`].
///
/// v0.1 decoders MUST accept the `Satellite` tag and surface it to the
/// dispatcher; the dispatcher responds with `ERROR
/// { UnsupportedSatelliteRoute }` when the server is not a federation hub.
pub(in crate::wire) fn decode_terminal_id(
    dec: &mut Decoder<'_>,
) -> Result<TerminalId, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        TERMINAL_ID_TAG_LOCAL => {
            let id = dec.read_u32_be()?;
            Ok(TerminalId::Local { id })
        }
        TERMINAL_ID_TAG_SATELLITE => {
            let host = SatelliteHost::new(dec.read_str()?);
            let id = dec.read_u32_be()?;
            Ok(TerminalId::Satellite { host, id })
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "TerminalId",
            value: u32::from(other),
        }),
    }
}

// The optional `TerminalId` scope of `SUBSCRIBE_EVENTS` / `EVENT` is now
// carried by TLV field *presence* (an absent `terminal` field = server-scoped
// `None`), so the old `encode_optional_terminal_id` / `decode_optional_terminal_id`
// presence-tag helpers were retired with the field-tagged migration.

// -----------------------------------------------------------------------------
// Small option-of-primitive helpers. Local to this module — `info.rs` has its
// own parallel set tuned to its types (id newtypes, layout nodes).
// -----------------------------------------------------------------------------

pub(super) fn encode_optional_str(value: Option<&str>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(s) => {
            enc.write_u8(1);
            enc.write_str(s);
        }
    }
}

pub(in crate::wire) fn decode_optional_str<'a>(
    dec: &mut Decoder<'a>,
) -> Result<Option<&'a str>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(dec.read_str()?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<str> tag",
            value: u32::from(other),
        }),
    }
}

fn encode_optional_u16(value: Option<u16>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(n) => {
            enc.write_u8(1);
            enc.write_u16_be(n);
        }
    }
}

fn decode_optional_u16(dec: &mut Decoder<'_>) -> Result<Option<u16>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(dec.read_u16_be()?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<u16> tag",
            value: u32::from(other),
        }),
    }
}

pub(super) fn encode_optional_u32(value: Option<u32>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(n) => {
            enc.write_u8(1);
            enc.write_u32_be(n);
        }
    }
}

pub(in crate::wire) fn decode_optional_u32(
    dec: &mut Decoder<'_>,
) -> Result<Option<u32>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(dec.read_u32_be()?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<u32> tag",
            value: u32::from(other),
        }),
    }
}

fn encode_optional_string_list(value: Option<&[String]>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(list) => {
            enc.write_u8(1);
            debug_assert!(
                u32::try_from(list.len()).is_ok(),
                "string list length exceeds u32",
            );
            let len = u32::try_from(list.len()).unwrap_or(u32::MAX);
            enc.write_u32_be(len);
            for s in list {
                enc.write_str(s);
            }
        }
    }
}

pub(in crate::wire) fn decode_optional_string_list(
    dec: &mut Decoder<'_>,
) -> Result<Option<Vec<String>>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => {
            let len = dec.read_u32_be()?;
            let len_usize = usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)?;
            // Clamp reservation to remaining bytes (each element is >=1 byte):
            // an over-declared length errors on EOF below rather than driving
            // an unbounded `Vec::with_capacity`.
            let mut out = dec.bounded_capacity(len_usize);
            for _ in 0..len_usize {
                out.push(dec.read_str()?.to_owned());
            }
            Ok(Some(out))
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<list<str>> tag",
            value: u32::from(other),
        }),
    }
}

/// Encode a string list as a `u32` count + N length-prefixed UTF-8 strings,
/// with no outer presence tag.
///
/// The optionality of a `SPAWN_TERMINAL.command` field is now carried by TLV
/// field *presence* (an absent field is `None`); a present field always holds a
/// concrete list, so the inner encoding drops the old `0/1` presence byte. An
/// empty list (`Some(vec![])`) round-trips as a present field whose value is
/// just a zero count.
pub(in crate::wire) fn encode_string_list(list: &[String], enc: &mut Encoder<'_>) {
    debug_assert!(
        u32::try_from(list.len()).is_ok(),
        "string list length exceeds u32",
    );
    let len = u32::try_from(list.len()).unwrap_or(u32::MAX);
    enc.write_u32_be(len);
    for s in list {
        enc.write_str(s);
    }
}

/// Decode a string list previously written by [`encode_string_list`].
///
/// Clamps the pre-reservation to the bytes remaining in the field value (each
/// element is at least one byte on the wire), so an over-declared count errors
/// on EOF rather than driving an unbounded `Vec::with_capacity`.
pub(in crate::wire) fn decode_string_list(
    dec: &mut Decoder<'_>,
) -> Result<Vec<String>, DecodeError> {
    let len = dec.read_u32_be()?;
    let len_usize = usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)?;
    let mut out = dec.bounded_capacity(len_usize);
    for _ in 0..len_usize {
        out.push(dec.read_str()?.to_owned());
    }
    Ok(out)
}

/// Encode an environment list as a `u32` count + N `(key, value)` string pairs,
/// with no outer presence tag (presence is the TLV field, as for
/// [`encode_string_list`]).
pub(in crate::wire) fn encode_env(list: &[(String, String)], enc: &mut Encoder<'_>) {
    debug_assert!(
        u32::try_from(list.len()).is_ok(),
        "env list length exceeds u32",
    );
    let len = u32::try_from(list.len()).unwrap_or(u32::MAX);
    enc.write_u32_be(len);
    for (k, v) in list {
        enc.write_str(k);
        enc.write_str(v);
    }
}

/// Decode an environment list previously written by [`encode_env`]. Bounds
/// pre-reservation by the remaining field bytes (each pair is at least eight
/// bytes on the wire).
pub(in crate::wire) fn decode_env(
    dec: &mut Decoder<'_>,
) -> Result<Vec<(String, String)>, DecodeError> {
    let len = dec.read_u32_be()?;
    let len_usize = usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)?;
    let mut out = dec.bounded_capacity(len_usize);
    for _ in 0..len_usize {
        let k = dec.read_str()?.to_owned();
        let v = dec.read_str()?.to_owned();
        out.push((k, v));
    }
    Ok(out)
}

// Optional byte fields (`BOOTSTRAP_READY.history_cursor`,
// `HISTORY_PAGE.next_cursor`, `METADATA_CHANGED.value`,
// `METADATA_VALUE.value`) express `None` as an absent TLV field.

// -----------------------------------------------------------------------------
// Scope codec — SPEC §7.4 (phux-4li.2).
//
// Layout: 1-byte tag + variant body.
//   0x00 Terminal   → tagged TerminalId (re-uses the L1 codec)
//   0x01 Group      → u32 (the inner GroupId; once federation ships a
//                     Local/Satellite tag will prefix this, mirroring the
//                     ADR-0016 TerminalId shape)
//   0x02 Global     → no body
// -----------------------------------------------------------------------------

pub(in crate::wire) fn encode_scope(scope: &Scope, enc: &mut Encoder<'_>) {
    match scope {
        Scope::Terminal(terminal_id) => {
            enc.write_u8(SCOPE_TAG_TERMINAL);
            encode_terminal_id(terminal_id, enc);
        }
        Scope::Group(group_id) => {
            enc.write_u8(SCOPE_TAG_GROUP);
            enc.write_u32_be(group_id.get());
        }
        Scope::Global => {
            enc.write_u8(SCOPE_TAG_GLOBAL);
        }
    }
}

pub(in crate::wire) fn decode_scope(dec: &mut Decoder<'_>) -> Result<Scope, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        SCOPE_TAG_TERMINAL => Ok(Scope::Terminal(decode_terminal_id(dec)?)),
        SCOPE_TAG_GROUP => Ok(Scope::Group(GroupId::new(dec.read_u32_be()?))),
        SCOPE_TAG_GLOBAL => Ok(Scope::Global),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Scope",
            value: u32::from(other),
        }),
    }
}

/// Decode the shared `{request_id, scope, key}` body of `GET_METADATA` and
/// `DELETE_METADATA` (identical field-tagged shape; `docs/spec/L3.md` §1).
///
/// Loops over the message body's TLV fields using the `field::get_metadata::*`
/// ids (shared by both messages) and surfaces a missing required `scope` /
/// `key` as [`DecodeError::UnexpectedEof`].
pub(in crate::wire) fn decode_metadata_scope_key(
    dec: &mut Decoder<'_>,
) -> Result<(u32, Scope, String), DecodeError> {
    let mut request_id = 0u32;
    let mut scope: Option<Scope> = None;
    let mut key: Option<String> = None;
    while let Some((id, value)) = dec.read_field()? {
        match id {
            field::get_metadata::REQUEST_ID => {
                request_id = Decoder::new(value).read_u32_be()?;
            }
            field::get_metadata::SCOPE => {
                scope = Some(decode_scope(&mut Decoder::new(value))?);
            }
            field::get_metadata::KEY => {
                key = Some(
                    core::str::from_utf8(value)
                        .map_err(|_| DecodeError::InvalidUtf8)?
                        .to_owned(),
                );
            }
            _ => {}
        }
    }
    Ok((
        request_id,
        scope.ok_or(DecodeError::UnexpectedEof)?,
        key.ok_or(DecodeError::UnexpectedEof)?,
    ))
}

// -----------------------------------------------------------------------------
// SpawnResult / SpawnError codec — SPEC §7.2 / §10.1 (phux-4li.10).
//
// Layout (outer SpawnResult, the body of `TERMINAL_SPAWNED.result`):
//   tag 0x00 Ok  → tagged TerminalId
//   tag 0x01 Err → SpawnError body:
//                    tag 0x00 GroupNotFound             → no further bytes
//                    tag 0x01 SpawnFailed               → length-prefixed UTF-8
//                    tag 0x02 UnsupportedSatelliteRoute → no further bytes
//                    tag 0x03 SatelliteUnreachable      → length-prefixed UTF-8
//
// The `Ok = 0x00 / Err = 0x01` convention deliberately mirrors the
// `Option` tag convention (`None = 0x00 / Some = 0x01`) so hex-dump
// readers do not need a second per-shape table.
// -----------------------------------------------------------------------------

pub(in crate::wire) fn encode_spawn_result(result: &SpawnResult, enc: &mut Encoder<'_>) {
    match result {
        SpawnResult::Ok(terminal_id) => {
            enc.write_u8(SPAWN_RESULT_OK);
            encode_terminal_id(terminal_id, enc);
        }
        SpawnResult::Err(err) => {
            enc.write_u8(SPAWN_RESULT_ERR);
            encode_spawn_error(err, enc);
        }
    }
}

pub(in crate::wire) fn decode_spawn_result(
    dec: &mut Decoder<'_>,
) -> Result<SpawnResult, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        SPAWN_RESULT_OK => Ok(SpawnResult::Ok(decode_terminal_id(dec)?)),
        SPAWN_RESULT_ERR => Ok(SpawnResult::Err(decode_spawn_error(dec)?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "SpawnResult",
            value: u32::from(other),
        }),
    }
}

fn encode_spawn_error(err: &SpawnError, enc: &mut Encoder<'_>) {
    match err {
        SpawnError::GroupNotFound => {
            enc.write_u8(SPAWN_ERROR_TAG_GROUP_NOT_FOUND);
        }
        SpawnError::SpawnFailed(msg) => {
            enc.write_u8(SPAWN_ERROR_TAG_SPAWN_FAILED);
            enc.write_str(msg);
        }
        SpawnError::UnsupportedSatelliteRoute => {
            enc.write_u8(SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE);
        }
        SpawnError::SatelliteUnreachable(msg) => {
            enc.write_u8(SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE);
            enc.write_str(msg);
        }
    }
}

fn decode_spawn_error(dec: &mut Decoder<'_>) -> Result<SpawnError, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        SPAWN_ERROR_TAG_GROUP_NOT_FOUND => Ok(SpawnError::GroupNotFound),
        SPAWN_ERROR_TAG_SPAWN_FAILED => Ok(SpawnError::SpawnFailed(dec.read_str()?.to_owned())),
        SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE => Ok(SpawnError::UnsupportedSatelliteRoute),
        SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE => {
            Ok(SpawnError::SatelliteUnreachable(dec.read_str()?.to_owned()))
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "SpawnError",
            value: u32::from(other),
        }),
    }
}

pub(in crate::wire) fn encode_move_result(result: &MoveResult, enc: &mut Encoder<'_>) {
    match result {
        MoveResult::Ok(terminal_id) => {
            enc.write_u8(MOVE_RESULT_OK);
            encode_terminal_id(terminal_id, enc);
        }
        MoveResult::Err(err) => {
            enc.write_u8(MOVE_RESULT_ERR);
            match err {
                MoveError::MoveFailed(msg) => {
                    enc.write_u8(MOVE_ERROR_TAG_MOVE_FAILED);
                    enc.write_str(msg);
                }
                MoveError::UnsupportedSatelliteRoute => {
                    enc.write_u8(MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE);
                }
            }
        }
    }
}

pub(in crate::wire) fn decode_move_result(
    dec: &mut Decoder<'_>,
) -> Result<MoveResult, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        MOVE_RESULT_OK => Ok(MoveResult::Ok(decode_terminal_id(dec)?)),
        MOVE_RESULT_ERR => {
            let err_tag = dec.read_u8()?;
            match err_tag {
                MOVE_ERROR_TAG_MOVE_FAILED => Ok(MoveResult::Err(MoveError::MoveFailed(
                    dec.read_str()?.to_owned(),
                ))),
                MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE => {
                    Ok(MoveResult::Err(MoveError::UnsupportedSatelliteRoute))
                }
                other => Err(DecodeError::UnknownEnumValue {
                    field: "MoveError",
                    value: u32::from(other),
                }),
            }
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "MoveResult",
            value: u32::from(other),
        }),
    }
}
