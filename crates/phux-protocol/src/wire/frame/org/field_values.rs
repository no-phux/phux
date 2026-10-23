//! Field value helpers: keys, integers, bounded strings, and the small positional values organization fields carry.

use super::{
    ActivationId, DecodeError, Decoder, Encoder, FILTER_ALL, FILTER_INTAKE, FILTER_RESOURCE,
    FILTER_SESSION, FILTER_WORKSPACE, LaunchSpec, LiveTarget, MAX_LAUNCH_LIST,
    MAX_LAUNCH_RESTORE_BYTES, MoveItem, OUTCOME_EXITED, OUTCOME_SIGNALED, OUTCOME_UNOBSERVED,
    OperationId, OrgFilter, Outcome, RecordRevision, ResourceKey, Revision, SessionKey,
    WorkspaceKey, decode_server_instance, decode_terminal_id, encode_server_instance,
    encode_terminal_id, field, utf8_value,
};

pub(super) fn write_key(enc: &mut Encoder<'_>, field_id: u32, key: &[u8; 16]) {
    enc.write_field(field_id, key);
}

pub(super) fn write_u64(enc: &mut Encoder<'_>, field_id: u32, value: u64) {
    enc.write_field(field_id, &value.to_be_bytes());
}

pub(super) fn write_u32(enc: &mut Encoder<'_>, field_id: u32, value: u32) {
    enc.write_field(field_id, &value.to_be_bytes());
}

pub(super) fn write_u8(enc: &mut Encoder<'_>, field_id: u32, value: u8) {
    enc.write_field(field_id, &[value]);
}

pub(super) fn key_bytes(value: &[u8]) -> Result<[u8; 16], DecodeError> {
    value
        .try_into()
        .map_err(|_| DecodeError::InvalidOrganizationKey)
}

pub(super) fn read_key<K>(value: &[u8], new: fn([u8; 16]) -> Option<K>) -> Result<K, DecodeError> {
    new(key_bytes(value)?).ok_or(DecodeError::InvalidOrganizationKey)
}

pub(super) fn read_operation(value: &[u8]) -> Result<OperationId, DecodeError> {
    OperationId::new(key_bytes(value)?).ok_or(DecodeError::InvalidIdempotencyKey)
}

pub(super) fn read_u64(value: &[u8]) -> Result<u64, DecodeError> {
    Decoder::new(value).read_u64_be()
}

pub(super) fn read_u32(value: &[u8]) -> Result<u32, DecodeError> {
    Decoder::new(value).read_u32_be()
}

pub(super) fn read_u8(value: &[u8]) -> Result<u8, DecodeError> {
    Decoder::new(value).read_u8()
}

/// A UTF-8 value of at most `max` bytes.
pub(super) fn bounded_str(value: &[u8], max: usize) -> Result<String, DecodeError> {
    if value.len() > max {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    utf8_value(value)
}

pub(super) fn bounded_dec_str(dec: &mut Decoder<'_>, max: usize) -> Result<String, DecodeError> {
    let text = dec.read_str()?;
    if text.len() > max {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    Ok(text.to_owned())
}

/// Push onto a repeated field, refusing more than `max` entries.
pub(super) fn push_bounded<T>(list: &mut Vec<T>, item: T, max: usize) -> Result<(), DecodeError> {
    if list.len() >= max {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    list.push(item);
    Ok(())
}

pub(super) fn enum_byte<T>(
    field: &'static str,
    value: Option<u8>,
    from: fn(u8) -> Option<T>,
) -> Result<Option<T>, DecodeError> {
    value
        .map(|raw| from(raw).ok_or_else(|| DecodeError::unknown_enum(field, raw)))
        .transpose()
}

pub(super) fn req<T>(value: Option<T>) -> Result<T, DecodeError> {
    value.ok_or(DecodeError::UnexpectedEof)
}

pub(super) fn encode_pair(enc: &mut Encoder<'_>, field_id: u32, pair: &(String, String)) {
    enc.write_field_with(field_id, |e| {
        e.write_str(&pair.0);
        e.write_str(&pair.1);
    });
}

pub(super) fn decode_pair(value: &[u8], max: usize) -> Result<(String, String), DecodeError> {
    let mut dec = Decoder::new(value);
    let key = bounded_dec_str(&mut dec, max)?;
    let value = bounded_dec_str(&mut dec, max)?;
    Ok((key, value))
}

pub(super) fn encode_live_target(target: &LiveTarget, enc: &mut Encoder<'_>) {
    encode_terminal_id(&target.resource, enc);
    encode_server_instance(&target.instance, enc);
    enc.write_u64_be(target.activation.get());
}

pub(super) fn decode_live_target(value: &[u8]) -> Result<LiveTarget, DecodeError> {
    let mut dec = Decoder::new(value);
    let resource = decode_terminal_id(&mut dec)?;
    let instance = decode_server_instance(&mut dec)?;
    let activation = ActivationId::new(dec.read_u64_be()?);
    Ok(LiveTarget {
        resource,
        instance,
        activation,
    })
}

pub(super) fn encode_outcome(outcome: Outcome, enc: &mut Encoder<'_>) {
    match outcome {
        Outcome::Exited(code) => {
            enc.write_u8(OUTCOME_EXITED);
            enc.write_u32_be(u32::from_be_bytes(code.to_be_bytes()));
        }
        Outcome::Signaled(signal) => {
            enc.write_u8(OUTCOME_SIGNALED);
            enc.write_u32_be(u32::from_be_bytes(signal.to_be_bytes()));
        }
        Outcome::Unobserved => enc.write_u8(OUTCOME_UNOBSERVED),
    }
}

pub(super) fn decode_outcome(value: &[u8]) -> Result<Outcome, DecodeError> {
    let mut dec = Decoder::new(value);
    let tag = dec.read_u8()?;
    let int = |dec: &mut Decoder<'_>| -> Result<i32, DecodeError> {
        Ok(i32::from_be_bytes(dec.read_u32_be()?.to_be_bytes()))
    };
    match tag {
        OUTCOME_EXITED => Ok(Outcome::Exited(int(&mut dec)?)),
        OUTCOME_SIGNALED => Ok(Outcome::Signaled(int(&mut dec)?)),
        OUTCOME_UNOBSERVED => Ok(Outcome::Unobserved),
        other => Err(DecodeError::unknown_enum("Outcome", other)),
    }
}

pub(super) fn encode_filter(filter: OrgFilter, enc: &mut Encoder<'_>) {
    match filter {
        OrgFilter::All => enc.write_u8(FILTER_ALL),
        OrgFilter::Session(key) => {
            enc.write_u8(FILTER_SESSION);
            for byte in key.as_bytes() {
                enc.write_u8(*byte);
            }
        }
        OrgFilter::Workspace(key) => {
            enc.write_u8(FILTER_WORKSPACE);
            for byte in key.as_bytes() {
                enc.write_u8(*byte);
            }
        }
        OrgFilter::Intake => enc.write_u8(FILTER_INTAKE),
        OrgFilter::Resource(key) => {
            enc.write_u8(FILTER_RESOURCE);
            for byte in key.as_bytes() {
                enc.write_u8(*byte);
            }
        }
    }
}

pub(super) fn decode_filter(value: &[u8]) -> Result<OrgFilter, DecodeError> {
    let (&tag, rest) = value.split_first().ok_or(DecodeError::UnexpectedEof)?;
    match tag {
        FILTER_ALL => Ok(OrgFilter::All),
        FILTER_SESSION => read_key(rest, SessionKey::new).map(OrgFilter::Session),
        FILTER_WORKSPACE => read_key(rest, WorkspaceKey::new).map(OrgFilter::Workspace),
        FILTER_INTAKE => Ok(OrgFilter::Intake),
        FILTER_RESOURCE => read_key(rest, ResourceKey::new).map(OrgFilter::Resource),
        other => Err(DecodeError::unknown_enum("OrgFilter", other)),
    }
}

pub(super) fn encode_launch(launch: &LaunchSpec, enc: &mut Encoder<'_>) {
    for arg in &launch.command {
        enc.write_field(field::org_launch::COMMAND, arg.as_bytes());
    }
    if let Some(cwd) = &launch.cwd {
        enc.write_field(field::org_launch::CWD, cwd.as_bytes());
    }
    for pair in &launch.env {
        encode_pair(enc, field::org_launch::ENV, pair);
    }
    if let Some(term) = &launch.term {
        enc.write_field(field::org_launch::TERM, term.as_bytes());
    }
    if let Some(restore) = &launch.restore {
        enc.write_field(field::org_launch::RESTORE, restore);
    }
}

pub(super) fn decode_launch(value: &[u8]) -> Result<LaunchSpec, DecodeError> {
    let mut dec = Decoder::new(value);
    let mut launch = LaunchSpec::default();
    while let Some((id, value)) = dec.read_field()? {
        match id {
            field::org_launch::COMMAND => {
                push_bounded(&mut launch.command, utf8_value(value)?, MAX_LAUNCH_LIST)?;
            }
            field::org_launch::CWD => launch.cwd = Some(utf8_value(value)?),
            field::org_launch::ENV => {
                let pair = decode_pair(value, usize::MAX)?;
                push_bounded(&mut launch.env, pair, MAX_LAUNCH_LIST)?;
            }
            field::org_launch::TERM => launch.term = Some(utf8_value(value)?),
            field::org_launch::RESTORE => {
                if value.len() > MAX_LAUNCH_RESTORE_BYTES {
                    return Err(DecodeError::OrganizationLimitExceeded);
                }
                launch.restore = Some(value.to_vec());
            }
            _ => {}
        }
    }
    Ok(launch)
}

pub(super) fn encode_move_item(item: &MoveItem, enc: &mut Encoder<'_>) {
    enc.write_field_with(field::org::MOVE_ITEM, |e| {
        for byte in item.resource.as_bytes().iter().chain(item.from.as_bytes()) {
            e.write_u8(*byte);
        }
        e.write_u64_be(item.from_expected.get());
    });
}

pub(super) fn decode_move_item(value: &[u8]) -> Result<MoveItem, DecodeError> {
    if value.len() < 32 {
        return Err(DecodeError::UnexpectedEof);
    }
    let (resource, rest) = value.split_at(16);
    let (from, expected) = rest.split_at(16);
    Ok(MoveItem {
        resource: read_key(resource, ResourceKey::new)?,
        from: read_key(from, SessionKey::new)?,
        from_expected: Revision::new(read_u64(expected)?),
    })
}

pub(super) fn encode_pin(pin: &RecordRevision, enc: &mut Encoder<'_>) {
    enc.write_field_with(field::org::PIN, |e| {
        for byte in &pin.key {
            e.write_u8(*byte);
        }
        e.write_u64_be(pin.revision.get());
    });
}

pub(super) fn decode_pin(value: &[u8]) -> Result<RecordRevision, DecodeError> {
    if value.len() < 16 {
        return Err(DecodeError::UnexpectedEof);
    }
    let (key, revision) = value.split_at(16);
    let key = key_bytes(key)?;
    if key == [0; 16] {
        return Err(DecodeError::InvalidOrganizationKey);
    }
    Ok(RecordRevision {
        key,
        revision: Revision::new(read_u64(revision)?),
    })
}
