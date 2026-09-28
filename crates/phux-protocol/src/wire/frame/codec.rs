//! Sub-record codec helpers shared by the frame encoder and the wire
//! decoder.

use crate::caps::{
    BootstrapCodec, BootstrapProfile, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use crate::ids::{
    BootstrapId, GroupId, RESOURCE_ID_TAG_LOCAL, RESOURCE_ID_TAG_SATELLITE, ResourceId,
    SatelliteHost, SessionId, StreamId,
};
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::PasteEvent;
use crate::wire::decode::{Decoder, utf8_value};
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;
use crate::wire::info::{decode_option_str, encode_option_str};

use super::{
    ATTACH_TARGET_BY_ID, ATTACH_TARGET_BY_NAME, ATTACH_TARGET_CREATE_IF_MISSING,
    ATTACH_TARGET_LAST, ActorRef, AttachTarget, MOVE_ERROR_TAG_MOVE_FAILED,
    MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE, MOVE_RESULT_ERR, MOVE_RESULT_OK, MoveError,
    MoveResult, SCOPE_TAG_GLOBAL, SCOPE_TAG_GROUP, SCOPE_TAG_RESOURCE,
    SPAWN_ERROR_TAG_GROUP_NOT_FOUND, SPAWN_ERROR_TAG_IDEMPOTENCY_CONFLICT,
    SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH, SPAWN_ERROR_TAG_PARENT_NOT_FOUND,
    SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE, SPAWN_ERROR_TAG_SPAWN_FAILED,
    SPAWN_ERROR_TAG_UNSUPPORTED_KIND, SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE,
    SPAWN_RESULT_ERR, SPAWN_RESULT_OK, Scope, SpawnError, SpawnResult, ViewportInfo,
};

pub(in crate::wire) fn encode_bootstrap_codec(codec: BootstrapCodec, enc: &mut Encoder<'_>) {
    match codec {
        BootstrapCodec::SynthesizedVtV1 => {
            enc.write_u8(BootstrapCodec::SYNTHESIZED_VT_V1_TAG);
        }
        BootstrapCodec::Native(version) => {
            enc.write_u8(BootstrapCodec::NATIVE_TAG);
            enc.write_u8(version.as_wire());
        }
        BootstrapCodec::AgentEventsJsonlV1 => {
            enc.write_u8(BootstrapCodec::AGENT_EVENTS_JSONL_V1_TAG);
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
            let codec = EngineCodec::from_wire(value)
                .ok_or_else(|| DecodeError::unknown_enum("EngineCodec", value))?;
            Ok(BootstrapCodec::Native(codec))
        }
        BootstrapCodec::AGENT_EVENTS_JSONL_V1_TAG => Ok(BootstrapCodec::AgentEventsJsonlV1),
        value => Err(DecodeError::unknown_enum("BootstrapCodec", value)),
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
            let codec = EngineCodec::from_wire(value)
                .ok_or_else(|| DecodeError::unknown_enum("EngineCodec", value))?;
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
        value => Err(DecodeError::unknown_enum("BootstrapProfile", value)),
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
        (BootstrapCodec::AgentEventsJsonlV1, 0) => Ok(BootstrapStreamProfile::AgentEventsJsonlV1),
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
            encode_option_str(cwd.as_deref(), enc);
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
            let cwd = decode_option_str(dec)?.map(str::to_owned);
            Ok(AttachTarget::CreateIfMissing { name, command, cwd })
        }
        other => Err(DecodeError::unknown_enum("AttachTarget", other)),
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
        other => Err(DecodeError::unknown_enum("FocusEvent", other)),
    }
}

pub(in crate::wire) fn encode_key_event(event: &KeyEvent, enc: &mut Encoder<'_>) {
    // `#[repr(u32)]` discriminants (ADR-0024); decoded via `TryFrom<u32>`.
    enc.write_u32_be(event.action as u32);
    enc.write_u32_be(event.key as u32);
    enc.write_u16_be(event.mods.bits());
    enc.write_u16_be(event.consumed_mods.bits());
    enc.write_u8(u8::from(event.composing));
    encode_option_str(event.text.as_deref(), enc);
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
    let text = decode_option_str(dec)?.map(str::to_owned);
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
            return Err(DecodeError::unknown_enum("PasteTrust", other));
        }
    };
    let data = dec.read_bytes()?.to_vec();
    Ok(PasteEvent { trust, data })
}

/// Encode a [`ResourceId`] (ADR-0016): tag `0` + `u32`, or tag `1` + host
/// `str` + `u32`.
pub(in crate::wire) fn encode_terminal_id(id: &ResourceId, enc: &mut Encoder<'_>) {
    match id {
        ResourceId::Local { id } => {
            enc.write_u8(RESOURCE_ID_TAG_LOCAL);
            enc.write_u32_be(*id);
        }
        ResourceId::Satellite { host, id } => {
            enc.write_u8(RESOURCE_ID_TAG_SATELLITE);
            enc.write_str(host.as_str());
            enc.write_u32_be(*id);
        }
    }
}

/// Decode a [`ResourceId`]; a non-hub server answers a `Satellite` id with
/// `UnsupportedSatelliteRoute` at dispatch, not here.
pub(in crate::wire) fn decode_terminal_id(
    dec: &mut Decoder<'_>,
) -> Result<ResourceId, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        RESOURCE_ID_TAG_LOCAL => {
            let id = dec.read_u32_be()?;
            Ok(ResourceId::Local { id })
        }
        RESOURCE_ID_TAG_SATELLITE => {
            let host = SatelliteHost::new(dec.read_str()?);
            let id = dec.read_u32_be()?;
            Ok(ResourceId::Satellite { host, id })
        }
        other => Err(DecodeError::unknown_enum("ResourceId", other)),
    }
}

// Presence-byte option helpers for primitives.

fn encode_optional_u16(value: Option<u16>, enc: &mut Encoder<'_>) {
    enc.write_option(value, Encoder::write_u16_be);
}

fn decode_optional_u16(dec: &mut Decoder<'_>) -> Result<Option<u16>, DecodeError> {
    dec.read_option("Option<u16> tag", Decoder::read_u16_be)
}

pub(super) fn encode_optional_u32(value: Option<u32>, enc: &mut Encoder<'_>) {
    enc.write_option(value, Encoder::write_u32_be);
}

pub(in crate::wire) fn decode_optional_u32(
    dec: &mut Decoder<'_>,
) -> Result<Option<u32>, DecodeError> {
    dec.read_option("Option<u32> tag", Decoder::read_u32_be)
}

fn encode_optional_string_list(value: Option<&[String]>, enc: &mut Encoder<'_>) {
    enc.write_option(value, |e, list| {
        debug_assert!(
            u32::try_from(list.len()).is_ok(),
            "string list length exceeds u32",
        );
        e.write_u32_be(u32::try_from(list.len()).unwrap_or(u32::MAX));
        for s in list {
            e.write_str(s);
        }
    });
}

pub(in crate::wire) fn decode_optional_string_list(
    dec: &mut Decoder<'_>,
) -> Result<Option<Vec<String>>, DecodeError> {
    dec.read_option("Option<list<str>> tag", |d| {
        let len = usize::try_from(d.read_u32_be()?).map_err(|_| DecodeError::LengthOverflow)?;
        let mut out = d.bounded_capacity(len);
        for _ in 0..len {
            out.push(d.read_str()?.to_owned());
        }
        Ok(out)
    })
}

/// Encode a string list as a `u32` count + strings; optionality is the TLV
/// field's presence.
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

/// Decode a string list written by [`encode_string_list`].
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

/// Encode an environment list as a `u32` count + `(key, value)` string pairs.
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

/// Decode an environment list written by [`encode_env`].
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

// `Scope` (SPEC §7.4): tag, then a `ResourceId`, a `u32` group, or nothing.

pub(in crate::wire) fn encode_scope(scope: &Scope, enc: &mut Encoder<'_>) {
    match scope {
        Scope::Resource(terminal_id) => {
            enc.write_u8(SCOPE_TAG_RESOURCE);
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
        SCOPE_TAG_RESOURCE => Ok(Scope::Resource(decode_terminal_id(dec)?)),
        SCOPE_TAG_GROUP => Ok(Scope::Group(GroupId::new(dec.read_u32_be()?))),
        SCOPE_TAG_GLOBAL => Ok(Scope::Global),
        other => Err(DecodeError::unknown_enum("Scope", other)),
    }
}

/// Decode the shared `{request_id, scope, key}` body of `GET_METADATA` and
/// `DELETE_METADATA` (`docs/spec/L3.md` §1).
pub(in crate::wire) fn decode_metadata_scope_key(
    dec: &mut Decoder<'_>,
) -> Result<(u32, Scope, String), DecodeError> {
    let mut request_id = 0u32;
    let mut scope: Option<Scope> = None;
    let mut key: Option<String> = None;
    while let Some((id, value)) = dec.read_field()? {
        match id {
            field::get_metadata::REQUEST_ID => request_id = Decoder::new(value).read_u32_be()?,
            field::get_metadata::SCOPE => scope = Some(decode_scope(&mut Decoder::new(value))?),
            field::get_metadata::KEY => key = Some(utf8_value(value)?),
            _ => {}
        }
    }
    Ok((
        request_id,
        scope.ok_or(DecodeError::UnexpectedEof)?,
        key.ok_or(DecodeError::UnexpectedEof)?,
    ))
}

// `SpawnResult` (SPEC §7.2 / §10.1): `Ok` + `ResourceId`, or `Err` + a
// `SpawnError` tag, with a message for `SpawnFailed` / `SatelliteUnreachable`.

pub(in crate::wire) fn encode_spawn_result(result: &SpawnResult, enc: &mut Encoder<'_>) {
    match result {
        // Bound and replayed results are `Ok` bytes; their extras ride
        // `RESOURCE_SPAWNED` fields 3 and 4.
        SpawnResult::Ok(terminal_id)
        | SpawnResult::OkBound {
            id: terminal_id, ..
        }
        | SpawnResult::Replayed {
            id: terminal_id, ..
        } => {
            enc.write_u8(SPAWN_RESULT_OK);
            encode_terminal_id(terminal_id, enc);
        }
        SpawnResult::Err(err) => {
            enc.write_u8(SPAWN_RESULT_ERR);
            encode_spawn_error(err, enc);
        }
    }
}

/// Write an [`ActorRef`] positionally: `client: u32 || credential_id:
/// optional<str> || client_name: optional<str>` (ADR-0123).
pub(in crate::wire) fn encode_actor_ref(actor: &ActorRef, enc: &mut Encoder<'_>) {
    enc.write_u32_be(actor.client.get());
    encode_option_str(actor.credential_id.as_deref(), enc);
    encode_option_str(actor.client_name.as_deref(), enc);
}

/// Read an [`ActorRef`] written by [`encode_actor_ref`].
pub(in crate::wire) fn decode_actor_ref(dec: &mut Decoder<'_>) -> Result<ActorRef, DecodeError> {
    let client = crate::ids::ClientId::new(dec.read_u32_be()?);
    let credential_id = decode_option_str(dec)?.map(str::to_owned);
    let client_name = decode_option_str(dec)?.map(str::to_owned);
    Ok(ActorRef::new(client)
        .with_credential_id(credential_id)
        .with_client_name(client_name))
}

/// Read an [`IdempotencyKey`](crate::ids::IdempotencyKey) from a field value
/// that must be exactly 16 non-zero bytes.
pub(in crate::wire) fn decode_idempotency_key(
    value: &[u8],
) -> Result<crate::ids::IdempotencyKey, DecodeError> {
    let bytes: [u8; 16] = value
        .try_into()
        .map_err(|_| DecodeError::InvalidIdempotencyKey)?;
    crate::ids::IdempotencyKey::new(bytes).ok_or(DecodeError::InvalidIdempotencyKey)
}

/// Write a [`ServerInstance`](crate::ids::ServerInstance) as its 16 raw bytes.
pub(in crate::wire) fn encode_server_instance(
    instance: &crate::ids::ServerInstance,
    enc: &mut Encoder<'_>,
) {
    for byte in instance.as_bytes() {
        enc.write_u8(*byte);
    }
}

/// Read a [`ServerInstance`](crate::ids::ServerInstance) from 16 raw bytes.
pub(in crate::wire) fn decode_server_instance(
    dec: &mut Decoder<'_>,
) -> Result<crate::ids::ServerInstance, DecodeError> {
    let mut bytes = [0; 16];
    for byte in &mut bytes {
        *byte = dec.read_u8()?;
    }
    Ok(crate::ids::ServerInstance::new(bytes))
}

pub(in crate::wire) fn decode_spawn_result(
    dec: &mut Decoder<'_>,
) -> Result<SpawnResult, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        SPAWN_RESULT_OK => Ok(SpawnResult::Ok(decode_terminal_id(dec)?)),
        SPAWN_RESULT_ERR => Ok(SpawnResult::Err(decode_spawn_error(dec)?)),
        other => Err(DecodeError::unknown_enum("SpawnResult", other)),
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
        SpawnError::UnsupportedKind => enc.write_u8(SPAWN_ERROR_TAG_UNSUPPORTED_KIND),
        SpawnError::ParentNotFound => enc.write_u8(SPAWN_ERROR_TAG_PARENT_NOT_FOUND),
        SpawnError::ParentKindMismatch => enc.write_u8(SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH),
        SpawnError::IdempotencyConflict => enc.write_u8(SPAWN_ERROR_TAG_IDEMPOTENCY_CONFLICT),
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
        SPAWN_ERROR_TAG_UNSUPPORTED_KIND => Ok(SpawnError::UnsupportedKind),
        SPAWN_ERROR_TAG_PARENT_NOT_FOUND => Ok(SpawnError::ParentNotFound),
        SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH => Ok(SpawnError::ParentKindMismatch),
        SPAWN_ERROR_TAG_IDEMPOTENCY_CONFLICT => Ok(SpawnError::IdempotencyConflict),
        other => Err(DecodeError::unknown_enum("SpawnError", other)),
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
                other => Err(DecodeError::unknown_enum("MoveError", other)),
            }
        }
        other => Err(DecodeError::unknown_enum("MoveResult", other)),
    }
}
