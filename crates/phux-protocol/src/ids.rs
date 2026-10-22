//! Stable identifiers used across the protocol.
//!
//! Most IDs are opaque server-allocated `u32`s, never reused within a server's
//! lifetime. [`ResourceId`] is a tagged union that also names the owning
//! federation host (ADR-0016, ADR-0007).

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        pub struct $name(pub u32);

        impl $name {
            /// Construct from a raw `u32`.
            #[must_use]
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            /// Inner raw value.
            #[must_use]
            pub const fn get(self) -> u32 {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

id_type!(
    /// Identifier for a session within a server.
    SessionId
);
id_type!(
    /// Identifier for a window within a server.
    WindowId
);
id_type!(
    /// Identifier for a currently-connected client.
    ClientId
);
id_type!(
    /// Opaque grouping key: the `Scope::Group` metadata scope, the spawn
    /// `group` field, and `CommandValue::GroupId`. Not a lifecycle tier
    /// (ADR-0019, ADR-0027); servers expose a single static `GroupId(1)`.
    GroupId
);

mod handles;
mod resource;

use handles::is_nonzero;
pub use handles::{
    ApprovalId, BootstrapId, FileUploadId, IdempotencyKey, InputOperationId, StreamId,
};
pub use resource::{
    FrameId, RESOURCE_ID_TAG_LOCAL, RESOURCE_ID_TAG_SATELLITE, RESOURCE_KIND_TAG_AGENT_SESSION,
    RESOURCE_KIND_TAG_TERMINAL, ResourceId, ResourceKind, SatelliteHost, ServerInstance,
};

/// Lowercase RFC 4648 base32 alphabet, the text form of a 16-byte key.
const BASE32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Characters in the text form of a 16-byte key: `ceil(128 / 5)`.
pub const KEY_TEXT_LEN: usize = 26;

/// Shortest prefix a display may shorten a key's text form to; a parser
/// always accepts the full form.
pub const KEY_MIN_PREFIX: usize = 6;

/// Write `bytes` as unpadded lowercase base32.
fn write_base32(bytes: &[u8; 16], f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    use core::fmt::Write as _;
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            f.write_char(char::from(
                BASE32_ALPHABET[((buffer >> bits) & 0x1f) as usize],
            ))?;
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        f.write_char(char::from(
            BASE32_ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize],
        ))?;
    }
    Ok(())
}

/// The value of one base32 digit, either case.
const fn base32_value(digit: u8) -> Option<u32> {
    match digit {
        b'a'..=b'z' => Some((digit - b'a') as u32),
        b'A'..=b'Z' => Some((digit - b'A') as u32),
        b'2'..=b'7' => Some((digit - b'2') as u32 + 26),
        _ => None,
    }
}

/// Parse exactly [`KEY_TEXT_LEN`] base32 digits into 16 bytes. The final
/// digit's two padding bits must be zero, so every key has one spelling.
fn parse_base32(text: &str) -> Option<[u8; 16]> {
    let digits = text.as_bytes();
    if digits.len() != KEY_TEXT_LEN {
        return None;
    }
    let mut out = [0_u8; 16];
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;
    let mut index = 0;
    for digit in digits {
        buffer = (buffer << 5) | base32_value(*digit)?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(index)? = u8::try_from(buffer >> bits).ok()?;
            index += 1;
        }
        buffer &= (1 << bits) - 1;
    }
    (index == 16 && buffer == 0).then_some(out)
}

/// A key's text form was not [`KEY_TEXT_LEN`] base32 digits naming a
/// non-zero key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a key is 26 base32 digits naming a non-zero 16-byte value")]
pub struct KeyParseError;

/// Declare a persistent 16-byte organization key (`docs/spec/L1.md` §5.7):
/// random, never all-zero, 16 raw bytes on the wire, unpadded lowercase
/// base32 as text. Keys name records forever; live handles (`SessionId`,
/// `ResourceId`) name current execution and never stand in for one.
macro_rules! key16 {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Wrap 16 key bytes; `None` for the reserved all-zero value.
            #[must_use]
            pub const fn new(bytes: [u8; 16]) -> Option<Self> {
                if is_nonzero(&bytes) {
                    Some(Self(bytes))
                } else {
                    None
                }
            }

            /// Borrow the 16-byte wire representation.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// Parse the full text form: [`KEY_TEXT_LEN`] base32 digits,
            /// either case, not all zero.
            #[must_use]
            pub fn parse(text: &str) -> Option<Self> {
                parse_base32(text).and_then(Self::new)
            }

            /// Whether the text form starts with `prefix`, ignoring case: how
            /// a shortened key a user typed back is resolved.
            #[must_use]
            pub fn has_prefix(&self, prefix: &str) -> bool {
                let text = self.to_string();
                text.len() >= prefix.len()
                    && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write_base32(&self.0, f)
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({self})", stringify!($name))
            }
        }

        impl core::str::FromStr for $name {
            type Err = KeyParseError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Self::parse(text).ok_or(KeyParseError)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Self::parse(&text).ok_or_else(|| {
                    serde::de::Error::custom(concat!("invalid ", stringify!($name)))
                })
            }
        }
    };
}

key16!(
    /// The organization authority: minted once when the org store is first
    /// created and never changed. Refs across machines prefix it.
    AuthorityId
);
key16!(
    /// A session record's persistent key, unique per authority.
    SessionKey
);
key16!(
    /// A resource record's persistent key, unique per authority.
    ResourceKey
);
key16!(
    /// A workspace record's persistent key, unique per authority.
    WorkspaceKey
);
key16!(
    /// An arrangement record's persistent key, unique per authority.
    ArrangementKey
);
key16!(
    /// An acting principal: the owner human, or an owner-registered
    /// credential.
    PrincipalId
);

/// The caller-generated id of one organization operation.
///
/// The same type as the keyed-supervisory [`IdempotencyKey`]: 16 non-zero
/// bytes with a redacted Debug, in one retry space, so an organization
/// result and an `EVENT.operation_id` stamp correlate without conversion.
pub type OperationId = IdempotencyKey;

/// Declare a persisted `u64` counter newtype.
macro_rules! counter_u64 {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
            serde::Serialize, serde::Deserialize,
        )]
        pub struct $name(pub u64);

        impl $name {
            /// Construct from a raw `u64`.
            #[must_use]
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            /// Inner raw value.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

counter_u64!(
    /// One execution of a resource: monotonic from 1 per resource, persisted.
    ActivationId
);
counter_u64!(
    /// A resource's input-lease epoch; bumps on every lease change.
    LeaseEpoch
);
counter_u64!(
    /// A record's revision; bumps on every committed change to that record.
    Revision
);

#[cfg(test)]
mod tests;
