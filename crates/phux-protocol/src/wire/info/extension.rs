//! The snapshot's field-tagged extension block: resource state, sessions' state, and the journal head.

use super::{
    BytesMut, CloseReason, DecodeError, Decoder, Encoder, ExitFacet, ResourceInfo,
    ResourceLifecycle, SessionId, SessionInfo, SessionKey, SessionLifetime, decode_client_id,
    decode_optional_i32, decode_terminal_id, encode_client_id, encode_optional_i32,
    encode_terminal_id, field,
};

/// Build the snapshot extension block: `RESOURCE_STATE` per non-default
/// resource, the journal head, `RESOURCE_SESSION` per resource with a
/// reported session, then `SESSION_STATE` per session with a reported
/// lifetime, key, attribute, or recent input target; empty means nothing is
/// written.
pub(super) fn encode_snapshot_extension(
    sessions: &[SessionInfo],
    resources: &[ResourceInfo],
    journal_head: Option<u64>,
) -> BytesMut {
    let mut block = BytesMut::new();
    let mut enc = Encoder::new(&mut block);
    for info in resources.iter().filter(|r| r.has_resource_state()) {
        enc.write_field_with(field::snapshot_extension::RESOURCE_STATE, |e| {
            encode_resource_state(info, e);
        });
    }
    if let Some(head) = journal_head {
        enc.write_field_with(field::snapshot_extension::JOURNAL_HEAD, |e| {
            e.write_u64_be(head);
        });
    }
    for info in resources {
        if let Some(session) = info.session {
            enc.write_field_with(field::snapshot_extension::RESOURCE_SESSION, |e| {
                encode_terminal_id(&info.id, e);
                e.write_u32_be(session.get());
            });
        }
    }
    for info in sessions.iter().filter(|s| s.has_session_state()) {
        enc.write_field_with(field::snapshot_extension::SESSION_STATE, |e| {
            e.write_u32_be(info.id.get());
            if let Some(lifetime) = info.lifetime {
                e.write_field(field::session_state::LIFETIME, &[lifetime.to_u8()]);
            }
            if let Some(key) = &info.key {
                e.write_field(field::session_state::KEY, key.as_bytes());
            }
            for (name, value) in &info.attrs {
                e.write_field_with(field::session_state::ATTR, |e| {
                    e.write_str(name);
                    e.write_str(value);
                });
            }
            if let Some(recent) = &info.recent_input {
                e.write_field_with(field::session_state::RECENT_INPUT, |e| {
                    encode_terminal_id(recent, e);
                });
            }
        });
    }
    block
}

/// One `RESOURCE_STATE` value: the positional id, then only the fields that
/// differ from the defaults.
pub(super) fn encode_resource_state(info: &ResourceInfo, enc: &mut Encoder<'_>) {
    encode_terminal_id(&info.id, enc);
    if !matches!(info.lifecycle, ResourceLifecycle::Running) {
        enc.write_field(field::resource_state::LIFECYCLE, &[info.lifecycle.to_u8()]);
    }
    if let Some(exit) = &info.exit {
        enc.write_field_with(field::resource_state::EXIT, |e| encode_exit_facet(exit, e));
    }
    if let Some(holder) = info.input_holder {
        enc.write_field_with(field::resource_state::INPUT_HOLDER, |e| {
            encode_client_id(holder, e);
        });
    }
    for viewer in &info.viewers {
        enc.write_field_with(field::resource_state::VIEWER, |e| {
            encode_client_id(*viewer, e);
        });
    }
}

/// `ExitFacet`, positional: `exit_status: optional<i32> || signal:
/// optional<i32> || reason: u8 || exited_at_ms: u64 || retained_until_ms: u64`.
pub(super) fn encode_exit_facet(exit: &ExitFacet, enc: &mut Encoder<'_>) {
    encode_optional_i32(exit.exit_status, enc);
    encode_optional_i32(exit.signal, enc);
    enc.write_u8(exit.reason.as_wire());
    enc.write_u64_be(exit.exited_at_ms);
    enc.write_u64_be(exit.retained_until_ms);
}

pub(super) fn decode_exit_facet(dec: &mut Decoder<'_>) -> Result<ExitFacet, DecodeError> {
    let exit_status = decode_optional_i32(dec)?;
    let signal = decode_optional_i32(dec)?;
    let reason = CloseReason::from_wire(dec.read_u8()?);
    let exited_at_ms = dec.read_u64_be()?;
    let retained_until_ms = dec.read_u64_be()?;
    Ok(ExitFacet::new(exited_at_ms, retained_until_ms)
        .with_exit_status(exit_status)
        .with_signal(signal)
        .with_reason(reason))
}

/// Read the extension block if bytes remain, joining each `RESOURCE_STATE`
/// and `RESOURCE_SESSION` onto its resource and each `SESSION_STATE` onto
/// its session by id, and return its journal head.
pub(super) fn decode_snapshot_extension(
    dec: &mut Decoder<'_>,
    sessions: &mut [SessionInfo],
    resources: &mut [ResourceInfo],
) -> Result<Option<u64>, DecodeError> {
    if dec.at_body_end() {
        return Ok(None);
    }
    let mut journal_head = None;
    let mut block = Decoder::new(dec.read_bytes()?);
    while let Some((id, value)) = block.read_field()? {
        match id {
            field::snapshot_extension::RESOURCE_STATE => apply_resource_state(value, resources)?,
            field::snapshot_extension::JOURNAL_HEAD => {
                journal_head = Some(Decoder::new(value).read_u64_be()?);
            }
            field::snapshot_extension::RESOURCE_SESSION => {
                let mut v = Decoder::new(value);
                let resource = decode_terminal_id(&mut v)?;
                let session = SessionId::new(v.read_u32_be()?);
                if let Some(info) = resources.iter_mut().find(|r| r.id == resource) {
                    info.session = Some(session);
                }
            }
            field::snapshot_extension::SESSION_STATE => apply_session_state(value, sessions)?,
            _ => {}
        }
    }
    Ok(journal_head)
}

/// Join one `SESSION_STATE` value onto its session (ignored when none); an
/// unknown lifetime byte, a zero key, or a malformed recent input target
/// reads as not reported.
pub(super) fn apply_session_state(
    value: &[u8],
    sessions: &mut [SessionInfo],
) -> Result<(), DecodeError> {
    let mut dec = Decoder::new(value);
    let id = SessionId::new(dec.read_u32_be()?);
    let mut lifetime = None;
    let mut key = None;
    let mut attrs = Vec::new();
    let mut recent_input = None;
    while let Some((field_id, v)) = dec.read_field()? {
        match field_id {
            field::session_state::LIFETIME => {
                lifetime = SessionLifetime::from_u8(Decoder::new(v).read_u8()?);
            }
            field::session_state::KEY => {
                key = <[u8; 16]>::try_from(v).ok().and_then(SessionKey::new);
            }
            field::session_state::ATTR if attrs.len() < MAX_SESSION_ATTRS => {
                if let Some(pair) = decode_attr(v) {
                    attrs.push(pair);
                }
            }
            field::session_state::RECENT_INPUT => {
                recent_input = decode_terminal_id(&mut Decoder::new(v)).ok();
            }
            _ => {}
        }
    }
    if let Some(info) = sessions.iter_mut().find(|s| s.id == id) {
        info.lifetime = lifetime;
        info.key = key;
        info.attrs = attrs;
        info.recent_input = recent_input;
    }
    Ok(())
}

/// Most attributes one `SESSION_STATE` carries; later ones are dropped.
pub(super) const MAX_SESSION_ATTRS: usize = 64;

/// One `attr` value: `str key || str value`, each at most 256 bytes. A
/// malformed or oversized pair reads as absent.
pub(super) fn decode_attr(value: &[u8]) -> Option<(String, String)> {
    let mut dec = Decoder::new(value);
    let key = dec.read_str().ok()?;
    let value = dec.read_str().ok()?;
    (key.len() <= 256 && value.len() <= 256).then(|| (key.to_owned(), value.to_owned()))
}

/// Join one `RESOURCE_STATE` value onto its entry (ignored when none); an
/// unknown lifecycle byte reads as `Running`.
pub(super) fn apply_resource_state(
    value: &[u8],
    resources: &mut [ResourceInfo],
) -> Result<(), DecodeError> {
    let mut dec = Decoder::new(value);
    let id = decode_terminal_id(&mut dec)?;
    let mut lifecycle = ResourceLifecycle::Running;
    let mut exit = None;
    let mut input_holder = None;
    let mut viewers = Vec::new();
    while let Some((field_id, v)) = dec.read_field()? {
        let mut v = Decoder::new(v);
        match field_id {
            field::resource_state::VIEWER => viewers.push(decode_client_id(&mut v)?),
            field::resource_state::LIFECYCLE => {
                lifecycle = ResourceLifecycle::from_u8(v.read_u8()?).unwrap_or_default();
            }
            field::resource_state::EXIT => exit = Some(decode_exit_facet(&mut v)?),
            field::resource_state::INPUT_HOLDER => input_holder = Some(decode_client_id(&mut v)?),
            _ => {}
        }
    }
    if let Some(info) = resources.iter_mut().find(|r| r.id == id) {
        info.lifecycle = lifecycle;
        info.exit = exit;
        info.input_holder = input_holder;
        info.viewers = viewers;
    }
    Ok(())
}
