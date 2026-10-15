//! Opaque ids and the display alias stored beside them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;

/// Maximum byte length of a repo id.
pub(crate) const MAX_REPO_BYTES: usize = 64;

/// Maximum byte length of a task title.
pub(crate) const MAX_TITLE_BYTES: usize = 512;

/// Maximum byte length of a task description.
pub(crate) const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;

/// Maximum byte length of a signal body.
pub(crate) const MAX_SIGNAL_BODY_BYTES: usize = 1024;

/// Maximum byte length of the actor recorded on an event.
pub(crate) const MAX_ACTOR_BYTES: usize = 128;

/// A 16-byte opaque id. Zero is not a valid id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpaqueId([u8; 16]);

impl OpaqueId {
    pub(crate) fn generate() -> Result<Self, Error> {
        Ok(Self(mint()?))
    }

    pub(crate) fn from_slice(bytes: &[u8]) -> Result<Self, Error> {
        let Ok(bytes) = <[u8; 16]>::try_from(bytes) else {
            return Err(Error::Corrupt("id is not 16 bytes".to_owned()));
        };
        if bytes == [0; 16] {
            return Err(Error::Corrupt("id is all zeros".to_owned()));
        }
        Ok(Self(bytes))
    }

    /// Lowercase hex, 32 characters.
    #[must_use]
    pub fn to_hex(self) -> String {
        encode_hex(&self.0)
    }

    pub(crate) const fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Serialize for OpaqueId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for OpaqueId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        decode_hex_16(&text)
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("id must be 32 hex digits and not all zeros"))
    }
}

/// Client operation id. Replaying the same id is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationId([u8; 16]);

impl OperationId {
    /// Mint a fresh id.
    ///
    /// # Errors
    ///
    /// [`Error::Random`] when the system generator fails.
    pub fn generate() -> Result<Self, Error> {
        Ok(Self(mint()?))
    }

    /// Parse 32 hex digits. All-zero ids are refused.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidOperationId`] when `text` is not a usable id.
    pub fn parse(text: &str) -> Result<Self, Error> {
        decode_hex_16(text)
            .map(Self)
            .ok_or_else(|| Error::InvalidOperationId(text.to_owned()))
    }

    /// Lowercase hex, 32 characters.
    #[must_use]
    pub fn to_hex(self) -> String {
        encode_hex(&self.0)
    }

    pub(crate) const fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

/// Objective lifecycle stored on the row.
///
/// `NeedsYou` is the needs-you state. An unresolved [`SignalKind::CredentialLost`]
/// projects every objective that carries it into this state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveState {
    /// Not started.
    Todo,
    /// A worker is in progress.
    Working,
    /// A person has to look. Stored as `needs_you`.
    NeedsYou,
}

impl ObjectiveState {
    /// The stored and JSON spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Working => "working",
            Self::NeedsYou => "needs_you",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "todo" => Ok(Self::Todo),
            "working" => Ok(Self::Working),
            "needs_you" => Ok(Self::NeedsYou),
            _ => Err(Error::Corrupt(format!("unknown objective state {text}"))),
        }
    }
}

/// Signal kinds this store records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// The worker's grant is gone after a cold server restart.
    ///
    /// The grant ledger lives in memory, so a restart drops it. While a
    /// signal of this kind is unresolved, the objective's state is
    /// `needs_you` and a write cannot leave it `working`.
    CredentialLost,
}

impl SignalKind {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CredentialLost => "credential_lost",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "credential_lost" => Ok(Self::CredentialLost),
            _ => Err(Error::Corrupt(format!("unknown signal kind {text}"))),
        }
    }
}

/// Display alias `repo/n`, stored beside the opaque objective id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Alias {
    /// Repo id, the part before the slash.
    pub repo: String,
    /// Per-repo number, starting at 1.
    pub number: u64,
}

impl Alias {
    /// Parse `repo/n`. The number has no leading zeros.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidAlias`] when `text` is not a canonical alias.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let Some((repo, number)) = text.split_once('/') else {
            return Err(Error::InvalidAlias(text.to_owned()));
        };
        if number.contains('/') {
            return Err(Error::InvalidAlias(text.to_owned()));
        }
        let repo = parse_repo(repo).map_err(|_| Error::InvalidAlias(text.to_owned()))?;
        let number = parse_number(number).map_err(|_| Error::InvalidAlias(text.to_owned()))?;
        Ok(Self { repo, number })
    }

    /// `repo/n`.
    #[must_use]
    pub fn display(&self) -> String {
        format!("{}/{}", self.repo, self.number)
    }
}

impl Serialize for Alias {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.display())
    }
}

impl<'de> Deserialize<'de> for Alias {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for Alias {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.repo, self.number)
    }
}

/// Check a repo id: one leading ASCII letter or digit, then letters,
/// digits, `.`, `_`, or `-`, at most 64 bytes.
pub(crate) fn parse_repo(text: &str) -> Result<String, Error> {
    if text.is_empty() || text.len() > MAX_REPO_BYTES {
        return Err(Error::InvalidRepo(text.to_owned()));
    }
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return Err(Error::InvalidRepo(text.to_owned()));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(Error::InvalidRepo(text.to_owned()));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
        return Err(Error::InvalidRepo(text.to_owned()));
    }
    Ok(text.to_owned())
}

pub(crate) fn parse_title(text: &str) -> Result<String, Error> {
    let trimmed = text.trim();
    check_text("title", trimmed, 1, MAX_TITLE_BYTES)?;
    if trimmed.chars().any(|ch| ch == '\n' || ch == '\r') {
        return Err(Error::invalid_text("title", "must be one line"));
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn parse_description(text: &str) -> Result<String, Error> {
    check_text("description", text, 0, MAX_DESCRIPTION_BYTES)?;
    Ok(text.to_owned())
}

pub(crate) fn parse_body(text: &str) -> Result<String, Error> {
    check_text("signal body", text, 0, MAX_SIGNAL_BODY_BYTES)?;
    Ok(text.to_owned())
}

pub(crate) fn parse_actor(text: &str) -> Result<String, Error> {
    let trimmed = text.trim();
    check_text("actor", trimmed, 1, MAX_ACTOR_BYTES)?;
    Ok(trimmed.to_owned())
}

fn check_text(field: &'static str, text: &str, min: usize, max: usize) -> Result<(), Error> {
    if text.bytes().any(|byte| byte == 0) {
        return Err(Error::invalid_text(field, "contains a NUL byte"));
    }
    if text.len() < min {
        return Err(Error::invalid_text(field, "is empty"));
    }
    if text.len() > max {
        return Err(Error::invalid_text(field, "is too long"));
    }
    Ok(())
}

fn parse_number(text: &str) -> Result<u64, Error> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::InvalidAlias(text.to_owned()));
    }
    let number: u64 = text
        .parse()
        .map_err(|_| Error::InvalidAlias(text.to_owned()))?;
    if i64::try_from(number).is_err() {
        return Err(Error::InvalidAlias(text.to_owned()));
    }
    Ok(number)
}

fn mint() -> Result<[u8; 16], Error> {
    let mut bytes = [0_u8; 16];
    loop {
        getrandom::getrandom(&mut bytes).map_err(|err| Error::Random(err.to_string()))?;
        if bytes.iter().any(|byte| *byte != 0) {
            return Ok(bytes);
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn decode_hex_16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = [0_u8; 16];
    for (index, chunk) in bytes.chunks(2).enumerate() {
        let hi = hex_value(chunk[0])?;
        let lo = hex_value(chunk[1])?;
        out[index] = (hi << 4) | lo;
    }
    if out == [0; 16] {
        return None;
    }
    Some(out)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Alias, OperationId};

    #[test]
    fn alias_accepts_the_full_repo_form_and_refuses_bare_numbers() {
        let alias = Alias::parse("phux/3").expect("alias");
        assert_eq!(alias.display(), "phux/3");
        assert_eq!(alias.number, 3);
        for bad in ["3", "phux", "phux/03", "phux/0", "phux/3/1", "/3", "phux/"] {
            assert!(Alias::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn operation_id_rejects_zero_and_short_hex() {
        assert!(OperationId::parse("00".repeat(16).as_str()).is_err());
        assert!(OperationId::parse("abcd").is_err());
        let id = OperationId::parse("ab".repeat(16).as_str()).expect("id");
        assert_eq!(id.to_hex(), "ab".repeat(16));
    }
}
