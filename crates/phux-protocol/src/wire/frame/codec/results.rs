//! The `RESOURCE_SPAWNED` and `RESOURCE_MOVED` result codecs.

use super::super::{
    MOVE_ERROR_TAG_MOVE_FAILED, MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE, MOVE_RESULT_ERR,
    MOVE_RESULT_OK, MoveError, MoveResult, SPAWN_ERROR_TAG_GROUP_NOT_FOUND,
    SPAWN_ERROR_TAG_IDEMPOTENCY_CONFLICT, SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH,
    SPAWN_ERROR_TAG_PARENT_NOT_FOUND, SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE,
    SPAWN_ERROR_TAG_SPAWN_FAILED, SPAWN_ERROR_TAG_UNSUPPORTED_KIND,
    SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE, SPAWN_RESULT_ERR, SPAWN_RESULT_OK, SpawnError,
    SpawnResult,
};
use super::{decode_terminal_id, encode_terminal_id};
use crate::wire::UnreadableResult;
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;

// `SpawnResult` (SPEC §7.2 / §10.1): `Ok` + `ResourceId`, or `Err` + a
// `SpawnError` tag, with a message for `SpawnFailed` / `SatelliteUnreachable`.
//
// The result is the field value's whole tail, so one this build cannot read
// (an unknown result or error tag, or an unknown value inside a known one)
// decodes as `Unknown` with its bytes kept for diagnostics. `Unknown` always
// encodes as the reserved result tag `RESULT_TAG_UNKNOWN`, never those bytes.

/// The result tag reserved for "outcome not stated" in `RESOURCE_SPAWNED`
/// and `RESOURCE_MOVED`: never allocated to a result, so every build decodes
/// it as `Unknown`.
pub(in crate::wire) const RESULT_TAG_UNKNOWN: u8 = 0xFF;

/// The whole result as [`UnreadableResult`] when `decoded` failed on a value
/// this build does not know; malformed bytes stay an error.
fn or_unreadable<T>(
    decoded: Result<T, DecodeError>,
    raw: &[u8],
    unknown: impl FnOnce(UnreadableResult) -> T,
) -> Result<T, DecodeError> {
    match decoded {
        Err(err) if err.is_unsupported() => Ok(unknown(UnreadableResult::from_wire(raw))),
        other => other,
    }
}

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
        SpawnResult::Err(err) => encode_spawn_error(err, enc),
    }
}

pub(in crate::wire) fn decode_spawn_result(
    dec: &mut Decoder<'_>,
) -> Result<SpawnResult, DecodeError> {
    let raw = dec.remaining();
    let decoded = match dec.read_u8()? {
        SPAWN_RESULT_OK => decode_terminal_id(dec).map(SpawnResult::Ok),
        SPAWN_RESULT_ERR => decode_spawn_error(dec).map(SpawnResult::Err),
        other => Err(DecodeError::unknown_enum("SpawnResult", other)),
    };
    or_unreadable(decoded, raw, |raw| {
        SpawnResult::Err(SpawnError::Unknown(raw))
    })
}

/// Write an `Err` result: its tag and the error, or the reserved unknown
/// tag for an unreadable one.
fn encode_spawn_error(err: &SpawnError, enc: &mut Encoder<'_>) {
    if let SpawnError::Unknown(_) = err {
        enc.write_u8(RESULT_TAG_UNKNOWN);
        return;
    }
    enc.write_u8(SPAWN_RESULT_ERR);
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
        // Written verbatim above.
        SpawnError::Unknown(_) => {}
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
        MoveResult::Err(MoveError::Unknown(_)) => enc.write_u8(RESULT_TAG_UNKNOWN),
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
                // Written as the reserved tag by the first arm.
                MoveError::Unknown(_) => {}
            }
        }
    }
}

pub(in crate::wire) fn decode_move_result(
    dec: &mut Decoder<'_>,
) -> Result<MoveResult, DecodeError> {
    let raw = dec.remaining();
    let decoded = match dec.read_u8()? {
        MOVE_RESULT_OK => decode_terminal_id(dec).map(MoveResult::Ok),
        MOVE_RESULT_ERR => decode_move_error(dec).map(MoveResult::Err),
        other => Err(DecodeError::unknown_enum("MoveResult", other)),
    };
    or_unreadable(decoded, raw, |raw| MoveResult::Err(MoveError::Unknown(raw)))
}

fn decode_move_error(dec: &mut Decoder<'_>) -> Result<MoveError, DecodeError> {
    match dec.read_u8()? {
        MOVE_ERROR_TAG_MOVE_FAILED => Ok(MoveError::MoveFailed(dec.read_str()?.to_owned())),
        MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE => Ok(MoveError::UnsupportedSatelliteRoute),
        other => Err(DecodeError::unknown_enum("MoveError", other)),
    }
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
