//! Persistent identifiers: 16-byte keys that name records forever, and the
//! `u64` counters that version them.
//!
//! Keys are minted outside core (the server draws them from the OS RNG) and
//! handed to [`crate::apply::apply`] through [`FreshKeys`]; core itself never
//! generates randomness. Live handles ([`crate::SessionId`],
//! [`crate::ResourceId`]) are a separate family and never appear in records.
//!
//! Text form: lowercase RFC 4648 base32 without padding, 26 characters for
//! 16 bytes. [`Key16::short`] shortens it to the shortest unique prefix of
//! at least [`MIN_DISPLAY_PREFIX`] characters for display; parsing always
//! takes the full text.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Length of a key's full text form.
pub const KEY_TEXT_LEN: usize = 26;

/// Shortest display prefix [`Key16::short`] ever returns.
pub const MIN_DISPLAY_PREFIX: usize = 6;

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Why a key's text form did not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KeyParseError {
    /// The text is not exactly [`KEY_TEXT_LEN`] characters.
    #[error("key text must be {KEY_TEXT_LEN} characters, got {0}")]
    Length(usize),
    /// A character is outside the base32 alphabet.
    #[error("invalid base32 character {0:?}")]
    Character(char),
    /// The final character carries non-zero padding bits, so the text is
    /// not the canonical encoding of any key.
    #[error("non-canonical key text")]
    NonCanonical,
}

/// Encode 16 bytes as 26 lowercase base32 characters.
#[must_use]
pub fn encode_base32(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(KEY_TEXT_LEN);
    let mut acc: u16 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        acc = (acc << 8) | u16::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[usize::from((acc >> bits) & 31)]));
            acc &= (1 << bits) - 1;
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[usize::from((acc << (5 - bits)) & 31)]));
    }
    out
}

fn symbol_value(c: char) -> Option<u16> {
    let b = u8::try_from(c).ok()?;
    match b {
        b'a'..=b'z' => Some(u16::from(b - b'a')),
        b'A'..=b'Z' => Some(u16::from(b - b'A')),
        b'2'..=b'7' => Some(u16::from(b - b'2') + 26),
        _ => None,
    }
}

/// Decode the full 26-character text of a key. Case-insensitive.
///
/// # Errors
///
/// [`KeyParseError`] for a wrong length, a foreign character, or non-zero
/// trailing padding bits.
pub fn decode_base32(text: &str) -> Result<[u8; 16], KeyParseError> {
    let count = text.chars().count();
    if count != KEY_TEXT_LEN {
        return Err(KeyParseError::Length(count));
    }
    let mut out = [0_u8; 16];
    let mut acc: u16 = 0;
    let mut bits: u32 = 0;
    let mut idx = 0;
    for c in text.chars() {
        let v = symbol_value(c).ok_or(KeyParseError::Character(c))?;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            let byte = u8::try_from((acc >> bits) & 0xff).unwrap_or_default();
            if let Some(slot) = out.get_mut(idx) {
                *slot = byte;
            }
            idx += 1;
            acc &= (1 << bits) - 1;
        }
    }
    if acc != 0 {
        return Err(KeyParseError::NonCanonical);
    }
    Ok(out)
}

/// Length of the shortest prefix of `full` that no text in `others` (other
/// than `full` itself) shares, floored at [`MIN_DISPLAY_PREFIX`].
#[must_use]
pub fn unique_prefix_len<'a>(full: &str, others: impl IntoIterator<Item = &'a str>) -> usize {
    let longest_shared = others
        .into_iter()
        .filter(|o| *o != full)
        .map(|o| {
            full.bytes()
                .zip(o.bytes())
                .take_while(|(a, b)| a == b)
                .count()
        })
        .max()
        .unwrap_or(0);
    (longest_shared + 1).clamp(MIN_DISPLAY_PREFIX, full.len().max(MIN_DISPLAY_PREFIX))
}

/// Shared behavior of every 16-byte key type.
pub trait Key16: Copy + Eq + Ord + fmt::Display {
    /// The raw bytes.
    fn to_bytes(self) -> [u8; 16];

    /// Wrap raw bytes.
    fn from_bytes(bytes: [u8; 16]) -> Self;

    /// The shortest unique prefix of this key's text among `among`, at
    /// least [`MIN_DISPLAY_PREFIX`] characters.
    fn short<'a, I>(self, among: I) -> String
    where
        Self: 'a,
        I: IntoIterator<Item = &'a Self>,
    {
        let full = encode_base32(&self.to_bytes());
        let texts: Vec<String> = among
            .into_iter()
            .map(|k| encode_base32(&k.to_bytes()))
            .collect();
        let len = unique_prefix_len(&full, texts.iter().map(String::as_str));
        full.chars().take(len).collect()
    }

    /// Whether this key's text starts with `prefix` (case-insensitive).
    fn matches_prefix(self, prefix: &str) -> bool {
        let full = encode_base32(&self.to_bytes());
        prefix.len() <= full.len()
            && full
                .bytes()
                .zip(prefix.bytes())
                .all(|(a, b)| a == b.to_ascii_lowercase())
    }
}

macro_rules! key16 {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Wrap raw bytes.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            /// Borrow the raw bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }
        }

        impl Key16 for $name {
            fn to_bytes(self) -> [u8; 16] {
                self.0
            }

            fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&encode_base32(&self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), encode_base32(&self.0))
            }
        }

        impl FromStr for $name {
            type Err = KeyParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                decode_base32(s).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&encode_base32(&self.0))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = std::borrow::Cow::<'de, str>::deserialize(d)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

key16!(
    /// The persistent identity of one user's phux server. Minted once when
    /// the organization store is created; never changes.
    AuthorityId
);
key16!(
    /// Names a [`crate::Session`] record forever, per authority.
    SessionKey
);
key16!(
    /// Names a [`crate::Resource`] record forever, per authority.
    ResourceKey
);
key16!(
    /// Names an [`crate::Arrangement`] record forever, per authority.
    ArrangementKey
);
key16!(
    /// Names a workspace record forever, per authority.
    WorkspaceKey
);
key16!(
    /// Caller-generated identity of one command, used for retry
    /// deduplication and organization receipts.
    OperationId
);
key16!(
    /// One run of an authority's server. Scopes live handles and records
    /// which run started an [`crate::Activation`].
    InstanceId
);

macro_rules! counter {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// The value before any assignment; never names a real record
            /// state.
            pub const ZERO: Self = Self(0);
            /// The first assigned value.
            pub const FIRST: Self = Self(1);

            /// The next value.
            #[must_use]
            pub const fn next(self) -> Self {
                Self(self.0.saturating_add(1))
            }

            /// The raw counter.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

counter!(
    /// Bumps by one on every committed change to a record. A created record
    /// starts at [`Revision::FIRST`].
    Revision
);
counter!(
    /// One execution of a resource, numbered from [`ActivationId::FIRST`]
    /// per resource.
    ActivationId
);
counter!(
    /// Bumps on every input-lease change of a resource.
    LeaseEpoch
);

/// Source of fresh random keys for [`crate::apply::apply`]. The server
/// implements it with the OS RNG; tests use [`SequentialKeys`].
pub trait FreshKeys {
    /// Sixteen fresh bytes, never returned before.
    fn fresh(&mut self) -> [u8; 16];
}

/// Deterministic [`FreshKeys`]: a big-endian counter in the low eight bytes
/// behind a fixed eight-byte tag. For tests and replay, never production.
#[derive(Debug, Clone, Default)]
pub struct SequentialKeys {
    tag: [u8; 8],
    next: u64,
}

impl SequentialKeys {
    /// A counter starting at 1 behind `tag`.
    #[must_use]
    pub const fn new(tag: [u8; 8]) -> Self {
        Self { tag, next: 1 }
    }
}

impl FreshKeys for SequentialKeys {
    fn fresh(&mut self) -> [u8; 16] {
        let mut out = [0_u8; 16];
        out[..8].copy_from_slice(&self.tag);
        out[8..].copy_from_slice(&self.next.to_be_bytes());
        self.next = self.next.wrapping_add(1);
        out
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trips_and_is_canonical() {
        for bytes in [[0_u8; 16], [0xff; 16], *b"0123456789abcdef"] {
            let text = encode_base32(&bytes);
            assert_eq!(text.len(), KEY_TEXT_LEN);
            assert_eq!(decode_base32(&text), Ok(bytes));
            assert_eq!(decode_base32(&text.to_uppercase()), Ok(bytes));
        }
        assert_eq!(encode_base32(&[0; 16]), "a".repeat(26));
        assert_eq!(encode_base32(&[0xff; 16]), format!("{}4", "7".repeat(25)));
        assert_eq!(
            decode_base32(&"7".repeat(26)),
            Err(KeyParseError::NonCanonical)
        );
        assert_eq!(decode_base32("abc"), Err(KeyParseError::Length(3)));
        assert_eq!(
            decode_base32(&format!("{}1", "a".repeat(25))),
            Err(KeyParseError::Character('1'))
        );
    }

    #[test]
    fn keys_serialize_as_text_and_parse_back() {
        let key = SessionKey::from_bytes(*b"0123456789abcdef");
        let json = serde_json::to_string(&key).expect("serialize");
        assert_eq!(json, format!("\"{key}\""));
        let back: SessionKey = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, key);
        assert_eq!(key.to_string().parse::<SessionKey>(), Ok(key));
        assert!(serde_json::from_str::<SessionKey>("\"short\"").is_err());
        assert_eq!(serde_json::to_string(&Revision(7)).expect("serialize"), "7");
    }

    #[test]
    fn short_display_is_the_shortest_unique_prefix_of_at_least_six() {
        let mut a = [0_u8; 16];
        let mut b = [0_u8; 16];
        a[15] = 1;
        b[15] = 2;
        let (a, b) = (ResourceKey::from_bytes(a), ResourceKey::from_bytes(b));
        assert_eq!(a.short(std::iter::once(&a)), "aaaaaa");
        // a and b differ only in the last characters.
        assert_eq!(a.short([a, b].iter()), a.to_string());
        let c = ResourceKey::from_bytes([0xff; 16]);
        assert_eq!(c.short([a, b, c].iter()).len(), MIN_DISPLAY_PREFIX);
        assert!(c.matches_prefix("777777"));
        assert!(c.matches_prefix("7777"));
        assert!(!a.matches_prefix("777777"));
    }

    #[test]
    fn sequential_keys_never_repeat() {
        let mut keys = SequentialKeys::new(*b"testtest");
        assert_ne!(keys.fresh(), keys.fresh());
    }
}
