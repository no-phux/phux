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
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;

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

pub(in crate::wire) fn decode_spawn_result(
    dec: &mut Decoder<'_>,
) -> Result<SpawnResult, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        SPAWN_RESULT_OK => Ok(SpawnResult::Ok(decode_terminal_id(dec)?)),
        SPAWN_RESULT_ERR => Ok(SpawnResult::Err(decode_spawn_error(dec)?)),
        other => Ok(SpawnResult::Err(SpawnError::Unknown(other))),
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
        SpawnError::Unknown(tag) => enc.write_u8(*tag),
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
        other => Ok(SpawnError::Unknown(other)),
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
                MoveError::Unknown(tag) => enc.write_u8(*tag),
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
                other => Ok(MoveResult::Err(MoveError::Unknown(other))),
            }
        }
        other => Ok(MoveResult::Err(MoveError::Unknown(other))),
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
