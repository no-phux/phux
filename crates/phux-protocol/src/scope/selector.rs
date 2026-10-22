//! Hosts and selectors: containment, overlap, encoding, and parsing.

use super::reader::Reader;
use super::{
    MAX_HOST_BYTES, SUB_LOCAL, SUB_SATELLITE, ScopeError, ScopeGrammarError, TAG_GLOBAL, TAG_GROUP,
    TAG_HOST, TAG_RESOURCE, TAG_SESSION, TAG_TERMINAL,
};
use crate::ids::{ResourceKey, SessionKey};
use std::fmt;

// -----------------------------------------------------------------------------
// Hosts and selectors.
// -----------------------------------------------------------------------------

/// A satellite host as a selector names it.
///
/// It is 1..=255 UTF-8 bytes with no NUL or control character, compared
/// byte-for-byte (§5): the federation host key `ResourceId::Satellite`
/// carries, never a DNS name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Host(Box<str>);

impl Host {
    /// Validate `name` as a selector host.
    ///
    /// # Errors
    ///
    /// [`ScopeError::InvalidHost`] when `name` is empty, longer than
    /// [`MAX_HOST_BYTES`], or carries a control character.
    pub fn new(name: &str) -> Result<Self, ScopeError> {
        let sized = (1..=MAX_HOST_BYTES).contains(&name.len());
        if sized && !name.chars().any(char::is_control) {
            Ok(Self(name.into()))
        } else {
            Err(ScopeError::InvalidHost)
        }
    }

    /// The host as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whose resources a grant covers (§5).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Selector {
    /// Every subject.
    Global,
    /// The serving host: its Groups and its Terminals.
    HostLocal,
    /// One satellite host and its Terminals.
    HostSatellite(Host),
    /// One local Group and its current Terminal members.
    Group(u32),
    /// Exactly one local Terminal.
    TerminalLocal(u32),
    /// Exactly one satellite Terminal.
    TerminalSatellite(Host, u32),
    /// One local session record, by its persistent key, and its current
    /// Terminal members: a Group named so it survives a server restart.
    Session(SessionKey),
    /// One local resource record, by its persistent key: whichever
    /// activation of it is current.
    Resource(ResourceKey),
}

impl Selector {
    /// The canonical bytes of §5.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Append the canonical bytes of §5 to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Global => out.push(TAG_GLOBAL),
            Self::HostLocal => out.extend_from_slice(&[TAG_HOST, SUB_LOCAL]),
            Self::HostSatellite(host) => {
                out.extend_from_slice(&[TAG_HOST, SUB_SATELLITE]);
                write_host(out, host);
            }
            Self::Group(id) => {
                out.push(TAG_GROUP);
                out.extend_from_slice(&id.to_be_bytes());
            }
            Self::TerminalLocal(id) => {
                out.extend_from_slice(&[TAG_TERMINAL, SUB_LOCAL]);
                out.extend_from_slice(&id.to_be_bytes());
            }
            Self::TerminalSatellite(host, id) => {
                out.extend_from_slice(&[TAG_TERMINAL, SUB_SATELLITE]);
                write_host(out, host);
                out.extend_from_slice(&id.to_be_bytes());
            }
            Self::Session(key) => {
                out.push(TAG_SESSION);
                out.extend_from_slice(key.as_bytes());
            }
            Self::Resource(key) => {
                out.push(TAG_RESOURCE);
                out.extend_from_slice(key.as_bytes());
            }
        }
    }

    /// Decode exactly one selector occupying all of `bytes`.
    ///
    /// # Errors
    ///
    /// A [`ScopeError`] for an unknown tag or subtype, an invalid host,
    /// truncation, or bytes past the selector's canonical end
    /// ([`ScopeError::NonMinimalSelector`]).
    pub fn decode(bytes: &[u8]) -> Result<Self, ScopeError> {
        let mut reader = Reader::new(bytes);
        let selector = match reader.u8()? {
            TAG_GLOBAL => Self::Global,
            TAG_HOST => decode_host_selector(&mut reader)?,
            TAG_GROUP => Self::Group(reader.u32()?),
            TAG_TERMINAL => decode_terminal_selector(&mut reader)?,
            TAG_SESSION => Self::Session(reader.key(SessionKey::new)?),
            TAG_RESOURCE => Self::Resource(reader.key(ResourceKey::new)?),
            _ => return Err(ScopeError::UnknownSelectorTag),
        };
        if !reader.is_empty() {
            return Err(ScopeError::NonMinimalSelector);
        }
        Ok(selector)
    }

    /// Parse the registry spelling: `global`, `host`, `host:<name>`,
    /// `group:<u32>`, `terminal:<u32>`, `terminal:<host>/<u32>`,
    /// `session:<key>`, or `resource:<key>`.
    ///
    /// # Errors
    ///
    /// The first [`ScopeGrammarError`] rule the text breaks.
    pub fn parse(text: &str) -> Result<Self, ScopeGrammarError> {
        match text {
            "global" => return Ok(Self::Global),
            "host" => return Ok(Self::HostLocal),
            _ => {}
        }
        match text.split_once(':') {
            Some(("host", name)) => parse_host(name).map(Self::HostSatellite),
            Some(("group", id)) => parse_id(id).map(Self::Group),
            Some(("terminal", target)) => parse_terminal(target),
            Some(("session", key)) => parse_key(key, SessionKey::parse).map(Self::Session),
            Some(("resource", key)) => parse_key(key, ResourceKey::parse).map(Self::Resource),
            _ => Err(ScopeGrammarError::UnknownSelector),
        }
    }

    /// A short, identity-free name for the selector's kind (`global`,
    /// `host`, `group`, `terminal`, `session`, `resource`), for diagnostics
    /// that must not name a subject.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::HostLocal | Self::HostSatellite(_) => "host",
            Self::Group(_) => "group",
            Self::TerminalLocal(_) | Self::TerminalSatellite(..) => "terminal",
            Self::Session(_) => "session",
            Self::Resource(_) => "resource",
        }
    }

    /// Whether the selector names a resource by an id that restarts with the
    /// server process (`group:`, `terminal:`), so a persisted record cannot
    /// hold it. Keyed selectors name records that outlive the process.
    #[must_use]
    pub const fn names_process_id(&self) -> bool {
        matches!(
            self,
            Self::Group(_) | Self::TerminalLocal(_) | Self::TerminalSatellite(..)
        )
    }

    /// Whether some subject could lie in both selectors under some topology.
    ///
    /// Conservative on purpose: a clause for a pair that turns out not to
    /// overlap grants nothing, because both selectors are re-checked on every
    /// dispatch. Only pairs that can never share a subject return `false`.
    #[must_use]
    pub fn may_overlap(&self, other: &Self) -> bool {
        if self == other || matches!(self, Self::Global) || matches!(other, Self::Global) {
            return true;
        }
        overlaps_one_way(self, other) || overlaps_one_way(other, self)
    }
}

/// Whether `outer` can contain a subject `inner` also contains, for two
/// distinct, non-Global selectors. A key and a wire id may name the same
/// session or Terminal, so a keyed selector overlaps every local one of the
/// other spelling.
fn overlaps_one_way(outer: &Selector, inner: &Selector) -> bool {
    match (outer, inner) {
        (
            Selector::HostLocal,
            Selector::Group(_)
            | Selector::TerminalLocal(_)
            | Selector::Session(_)
            | Selector::Resource(_),
        )
        | (
            Selector::Group(_) | Selector::Session(_),
            Selector::TerminalLocal(_) | Selector::Resource(_),
        )
        | (Selector::Session(_), Selector::Group(_))
        | (Selector::Resource(_), Selector::TerminalLocal(_)) => true,
        (Selector::HostSatellite(host), Selector::TerminalSatellite(terminal_host, _)) => {
            host == terminal_host
        }
        _ => false,
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => f.write_str("global"),
            Self::HostLocal => f.write_str("host"),
            Self::HostSatellite(host) => write!(f, "host:{}", host.as_str()),
            Self::Group(id) => write!(f, "group:{id}"),
            Self::TerminalLocal(id) => write!(f, "terminal:{id}"),
            Self::TerminalSatellite(host, id) => write!(f, "terminal:{}/{id}", host.as_str()),
            Self::Session(key) => write!(f, "session:{key}"),
            Self::Resource(key) => write!(f, "resource:{key}"),
        }
    }
}

fn write_host(out: &mut Vec<u8>, host: &Host) {
    let bytes = host.as_str().as_bytes();
    // A `Host` is at most 255 bytes, so the length always fits.
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

fn decode_host_selector(reader: &mut Reader<'_>) -> Result<Selector, ScopeError> {
    match reader.u8()? {
        SUB_LOCAL => Ok(Selector::HostLocal),
        SUB_SATELLITE => reader.host().map(Selector::HostSatellite),
        _ => Err(ScopeError::UnknownSubtype),
    }
}

fn decode_terminal_selector(reader: &mut Reader<'_>) -> Result<Selector, ScopeError> {
    match reader.u8()? {
        SUB_LOCAL => reader.u32().map(Selector::TerminalLocal),
        SUB_SATELLITE => {
            let host = reader.host()?;
            Ok(Selector::TerminalSatellite(host, reader.u32()?))
        }
        _ => Err(ScopeError::UnknownSubtype),
    }
}

/// `terminal:<u32>` names a local Terminal; `terminal:<host>/<u32>` a
/// satellite one. The id never contains `/`, so the last one splits.
fn parse_terminal(target: &str) -> Result<Selector, ScopeGrammarError> {
    match target.rsplit_once('/') {
        Some((host, id)) => {
            let host = parse_host(host)?;
            Ok(Selector::TerminalSatellite(host, parse_id(id)?))
        }
        None => parse_id(target).map(Selector::TerminalLocal),
    }
}

fn parse_host(name: &str) -> Result<Host, ScopeGrammarError> {
    Host::new(name).map_err(|_| ScopeGrammarError::InvalidHost)
}

/// One spelling per value: a key's full lowercase text form (L1 §5.7.1),
/// never a shortened prefix or the uppercase a key reader also accepts.
fn parse_key<K: fmt::Display>(
    text: &str,
    parse: impl FnOnce(&str) -> Option<K>,
) -> Result<K, ScopeGrammarError> {
    parse(text)
        .filter(|key| key.to_string() == text)
        .ok_or(ScopeGrammarError::InvalidKey)
}

/// One spelling per value: digits only, no sign, no leading zero.
fn parse_id(id: &str) -> Result<u32, ScopeGrammarError> {
    let digits = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
    let minimal = id == "0" || !id.starts_with('0');
    if !(digits && minimal) {
        return Err(ScopeGrammarError::InvalidId);
    }
    id.parse::<u32>().map_err(|_| ScopeGrammarError::InvalidId)
}
