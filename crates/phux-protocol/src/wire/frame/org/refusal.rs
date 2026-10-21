//! Why an organization command was refused, and its tag-plus-body codec.

use super::{
    ActivationId, ActorRef, BytesMut, DecodeError, Decoder, Encoder, Revision, Verb,
    decode_actor_ref, encode_actor_ref,
};

/// Why an organization command was refused (`COMMAND_RESULT` tag `0x03`).
///
/// Sent only in reply to an organization command; a legacy command, and a
/// legacy write the server translates, never receives one, because an older
/// decoder fails the whole frame on an unknown result tag.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The caller's grant lacks `verb` on the command's subject. The same
    /// refusal answers an absent subject, so existence does not leak.
    Unauthorized {
        /// The verb that was missing.
        verb: Verb,
    },
    /// A named record does not exist.
    NotFound,
    /// The expected revision is not the record's current one.
    Conflict {
        /// The record's current revision.
        current: Revision,
    },
    /// The live target names an activation that is not current.
    Stale {
        /// The current activation, if any.
        current: Option<ActivationId>,
    },
    /// The session still has members.
    NotEmpty {
        /// How many.
        members: u32,
    },
    /// The server does not support what the command needs.
    Unsupported {
        /// The missing capability, by name.
        capability: String,
    },
    /// An argument broke a rule the codec cannot check.
    InvalidArgument {
        /// The argument's name.
        field: String,
        /// Why it was refused.
        reason: String,
    },
    /// Another live session has the name.
    NameTaken,
    /// A lost activation blocks the operation until it is resolved.
    ExecutionUncertain,
    /// The input lease is held by a principal the caller cannot take it from.
    LeaseHeld {
        /// The holder.
        holder: ActorRef,
    },
    /// The operation id was used before with a different payload.
    DuplicateOperation,
    /// The server's durable organization store could not commit, so nothing
    /// changed; the same operation may be retried.
    StoreFailure {
        /// What the store reported.
        reason: String,
    },
    /// A refusal tag this build does not know, body kept verbatim. Produced
    /// only by the decoder.
    Unknown {
        /// The unrecognised tag.
        tag: u8,
        /// The body bytes.
        body: Vec<u8>,
    },
}

pub(super) const REFUSAL_UNAUTHORIZED: u8 = 0x00;

pub(super) const REFUSAL_NOT_FOUND: u8 = 0x01;

pub(super) const REFUSAL_CONFLICT: u8 = 0x02;

pub(super) const REFUSAL_STALE: u8 = 0x03;

pub(super) const REFUSAL_NOT_EMPTY: u8 = 0x04;

pub(super) const REFUSAL_UNSUPPORTED: u8 = 0x05;

pub(super) const REFUSAL_INVALID_ARGUMENT: u8 = 0x06;

pub(super) const REFUSAL_NAME_TAKEN: u8 = 0x07;

pub(super) const REFUSAL_EXECUTION_UNCERTAIN: u8 = 0x08;

pub(super) const REFUSAL_LEASE_HELD: u8 = 0x09;

pub(super) const REFUSAL_DUPLICATE_OPERATION: u8 = 0x0a;

pub(super) const REFUSAL_STORE_FAILURE: u8 = 0x0b;

impl Refusal {
    /// The stable lowercase name consumers print.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unauthorized { .. } => "unauthorized",
            Self::NotFound => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::Stale { .. } => "stale",
            Self::NotEmpty { .. } => "not_empty",
            Self::Unsupported { .. } => "unsupported",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::NameTaken => "name_taken",
            Self::ExecutionUncertain => "execution_uncertain",
            Self::LeaseHeld { .. } => "lease_held",
            Self::DuplicateOperation => "duplicate_operation",
            Self::StoreFailure { .. } => "store_failure",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// Write a refusal: `tag: u8 || body: bytes` (`u32` length), the body
/// positional per tag, so an unknown tag is skipped whole.
pub(in crate::wire) fn encode_refusal(refusal: &Refusal, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    let tag = {
        let mut e = Encoder::new(&mut body);
        match refusal {
            Refusal::Unauthorized { verb } => {
                e.write_u8(verb.bit());
                REFUSAL_UNAUTHORIZED
            }
            Refusal::NotFound => REFUSAL_NOT_FOUND,
            Refusal::Conflict { current } => {
                e.write_u64_be(current.get());
                REFUSAL_CONFLICT
            }
            Refusal::Stale { current } => {
                e.write_option(*current, |e, id| e.write_u64_be(id.get()));
                REFUSAL_STALE
            }
            Refusal::NotEmpty { members } => {
                e.write_u32_be(*members);
                REFUSAL_NOT_EMPTY
            }
            Refusal::Unsupported { capability } => {
                e.write_str(capability);
                REFUSAL_UNSUPPORTED
            }
            Refusal::InvalidArgument { field, reason } => {
                e.write_str(field);
                e.write_str(reason);
                REFUSAL_INVALID_ARGUMENT
            }
            Refusal::NameTaken => REFUSAL_NAME_TAKEN,
            Refusal::ExecutionUncertain => REFUSAL_EXECUTION_UNCERTAIN,
            Refusal::LeaseHeld { holder } => {
                encode_actor_ref(holder, &mut e);
                REFUSAL_LEASE_HELD
            }
            Refusal::DuplicateOperation => REFUSAL_DUPLICATE_OPERATION,
            Refusal::StoreFailure { reason } => {
                e.write_str(reason);
                REFUSAL_STORE_FAILURE
            }
            Refusal::Unknown { tag, body: raw } => {
                for byte in raw {
                    e.write_u8(*byte);
                }
                *tag
            }
        }
    };
    enc.write_u8(tag);
    enc.write_bytes(&body);
}

/// Read a refusal; an unknown tag, or a known tag whose body names an
/// unknown verb, decodes as [`Refusal::Unknown`].
pub(in crate::wire) fn decode_refusal(dec: &mut Decoder<'_>) -> Result<Refusal, DecodeError> {
    let tag = dec.read_u8()?;
    let body = dec.read_bytes()?;
    let mut b = Decoder::new(body);
    let unknown = || Refusal::Unknown {
        tag,
        body: body.to_vec(),
    };
    let refusal = match tag {
        REFUSAL_UNAUTHORIZED => {
            let bit = b.read_u8()?;
            Verb::ALL
                .into_iter()
                .find(|verb| verb.bit() == bit)
                .map_or_else(unknown, |verb| Refusal::Unauthorized { verb })
        }
        REFUSAL_NOT_FOUND => Refusal::NotFound,
        REFUSAL_CONFLICT => Refusal::Conflict {
            current: Revision::new(b.read_u64_be()?),
        },
        REFUSAL_STALE => Refusal::Stale {
            current: b.read_option("Option<ActivationId> tag", |d| {
                Ok(ActivationId::new(d.read_u64_be()?))
            })?,
        },
        REFUSAL_NOT_EMPTY => Refusal::NotEmpty {
            members: b.read_u32_be()?,
        },
        REFUSAL_UNSUPPORTED => Refusal::Unsupported {
            capability: b.read_str()?.to_owned(),
        },
        REFUSAL_INVALID_ARGUMENT => Refusal::InvalidArgument {
            field: b.read_str()?.to_owned(),
            reason: b.read_str()?.to_owned(),
        },
        REFUSAL_NAME_TAKEN => Refusal::NameTaken,
        REFUSAL_EXECUTION_UNCERTAIN => Refusal::ExecutionUncertain,
        REFUSAL_LEASE_HELD => Refusal::LeaseHeld {
            holder: decode_actor_ref(&mut b)?,
        },
        REFUSAL_DUPLICATE_OPERATION => Refusal::DuplicateOperation,
        REFUSAL_STORE_FAILURE => Refusal::StoreFailure {
            reason: b.read_str()?.to_owned(),
        },
        _ => unknown(),
    };
    Ok(refusal)
}
