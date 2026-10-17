//! Closed scope grants for the terminal endpoint (`docs/spec/workload-auth.md`
//! §5).
//!
//! A grant pairs a nonempty [`Verbs`] set with one [`Selector`]. A
//! [`TerminalScopeSet`] is a canonical set of grants; an
//! [`EffectiveScopeSet`] is the conjunctive intersection of a requested set
//! with a registry ceiling, keeping both selectors per clause so a Group or
//! Host ceiling is never flattened into a Terminal grant.
//!
//! Both spellings are strict: the §5 canonical bytes (unsorted, duplicate,
//! unknown-bit, zero-verb, non-minimal, truncated, or trailing images are
//! refused, never normalized) and the registry grammar
//! `"<verb>[,<verb>...]@<selector>"`. The grammar may hold `?signal` for
//! approval (ADR-0128); a hold is server-local and never in the canonical
//! image.
//!
//! The scope schema is versioned ([`ScopeSchema`]). Schema v2 adds
//! `ORGANIZE` and `TERMINATE`; a v1 grant decodes with `SIGNAL` implying
//! `TERMINATE` and `CREATE` implying `ORGANIZE`, so no stored grant loses
//! the authority it had and none gains a legacy operation.
//!
//! Nothing here consults server state: the server passes a containment
//! predicate to [`EffectiveScopeSet::admits`].

use std::fmt;

use crate::ids::{ResourceKey, SessionKey};
pub use crate::kinds::{Verb, Verbs};

/// The scope schema this build writes (workload-auth §5).
pub const SCOPE_SCHEMA_VERSION: u8 = ScopeSchema::CURRENT.version();

/// One version of the closed scope schema (workload-auth §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScopeSchema {
    /// Six verbs, `0x01`..=`0x20`; bits `0xC0` are refused. Decoded grants
    /// gain the v2 implications ([`Verbs::with_v1_implications`]).
    V1,
    /// Eight verbs: v1's six, `ORGANIZE = 0x40`, and `TERMINATE = 0x80`.
    V2,
}

impl ScopeSchema {
    /// The schema this build writes.
    pub const CURRENT: Self = Self::V2;

    /// The schema's version number.
    #[must_use]
    pub const fn version(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }

    /// The schema numbered `version`, or `None` for one this build does not
    /// know (a reader refuses it rather than guessing).
    #[must_use]
    pub const fn from_version(version: u8) -> Option<Self> {
        match version {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            _ => None,
        }
    }

    /// The verb bits a grant may carry under this schema.
    #[must_use]
    pub const fn known_bits(self) -> u8 {
        match self {
            Self::V1 => Verbs::V1_KNOWN_BITS,
            Self::V2 => Verbs::KNOWN_BITS,
        }
    }
}

/// Most grants a [`TerminalScopeSet`] may hold (§5).
pub const MAX_GRANTS: usize = 64;

/// Most clauses an [`EffectiveScopeSet`] may hold (§5).
pub const MAX_CLAUSES: usize = 64;

/// Largest encoded [`EffectiveScopeSet`], in bytes (§5). A larger
/// intersection denies admission rather than truncating authority.
pub const MAX_EFFECTIVE_BYTES: usize = 32_768;

/// Longest satellite host a selector may name, in bytes (§5).
pub const MAX_HOST_BYTES: usize = 255;

const TAG_GLOBAL: u8 = 0x00;
const TAG_HOST: u8 = 0x01;
const TAG_GROUP: u8 = 0x02;
const TAG_TERMINAL: u8 = 0x03;
const TAG_SESSION: u8 = 0x04;
const TAG_RESOURCE: u8 = 0x05;
const SUB_LOCAL: u8 = 0x00;
const SUB_SATELLITE: u8 = 0x01;

/// The registry spelling of every verb, in bit order.
const VERB_NAMES: [(&str, Verb); 8] = [
    ("inventory", Verb::Inventory),
    ("observe", Verb::Observe),
    ("create", Verb::Create),
    ("bind", Verb::Bind),
    ("input", Verb::Input),
    ("signal", Verb::Signal),
    ("organize", Verb::Organize),
    ("terminate", Verb::Terminate),
];

/// The wildcard verb: every verb of the schema, and only on its own.
const ALL_VERBS: &str = "*";

/// The prefix that marks a verb as held for approval (ADR-0128).
const HELD_PREFIX: char = '?';

/// The only verb a grant may hold: approvals are held `SIGNAL` actions.
const HOLDABLE: Verbs = Verbs::of(&[Verb::Signal]);

// -----------------------------------------------------------------------------
// Errors.
// -----------------------------------------------------------------------------

/// Why a canonical scope image was refused (§5). No variant carries input
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    /// The image ended before a declared field.
    #[error("scope image is truncated")]
    Truncated,
    /// Bytes follow the last declared grant or clause.
    #[error("scope image has trailing bytes")]
    TrailingBytes,
    /// A selector's declared length exceeds its canonical encoding.
    #[error("scope selector length is not minimal")]
    NonMinimalSelector,
    /// A selector tag outside `0x00..=0x05`.
    #[error("scope selector tag is unknown")]
    UnknownSelectorTag,
    /// A Host or Terminal subtype outside `LOCAL` / `SATELLITE`.
    #[error("scope selector subtype is unknown")]
    UnknownSubtype,
    /// A satellite host that is empty, too long, not UTF-8, or carries a
    /// control character.
    #[error("scope host must be 1..=255 UTF-8 bytes with no NUL or control character")]
    InvalidHost,
    /// A session or resource key that is all zero.
    #[error("scope key must be 16 bytes, not all zero")]
    InvalidKey,
    /// A grant or clause with no verb.
    #[error("scope verbs must be nonzero")]
    ZeroVerbs,
    /// A verb bit the image's schema does not know: `0xC0` in a v1 image.
    #[error("scope verbs carry an unknown bit")]
    UnknownVerbBits,
    /// Grants or clauses out of canonical order.
    #[error("scope entries are not strictly increasing")]
    Unsorted,
    /// A selector (or selector pair) listed twice.
    #[error("scope entries repeat a selector")]
    Duplicate,
    /// More than [`MAX_GRANTS`] grants.
    #[error("scope set holds more than 64 grants")]
    TooManyGrants,
    /// More than [`MAX_CLAUSES`] clauses.
    #[error("effective scope set holds more than 64 clauses")]
    TooManyClauses,
    /// An effective set larger than [`MAX_EFFECTIVE_BYTES`].
    #[error("effective scope set exceeds 32768 bytes")]
    TooLarge,
}

/// Why a registry scope string was refused. The message names the rule,
/// never the input, so a secret pasted into the wrong argument cannot reach
/// stderr through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopeGrammarError {
    /// Not of the form `<verbs>@<selector>`.
    #[error("a scope must have the form <verb>[,<verb>...]@<selector>")]
    Shape,
    /// A verb outside the closed set of the record's schema (v1 has no
    /// `organize` or `terminate`).
    #[error(
        "a scope names a verb outside inventory|observe|create|bind|input|signal|?signal|organize|terminate|*"
    )]
    UnknownVerb,
    /// `?` on a verb other than `signal`: only `SIGNAL` actions are held.
    #[error("a scope may hold only `?signal`")]
    UnholdableVerb,
    /// A verb listed twice, or `*` combined with named verbs.
    #[error("a scope repeats a verb or combines `*` with named verbs")]
    RedundantVerb,
    /// A selector outside the closed set.
    #[error(
        "a scope names a selector outside global|host|host:<name>|group:<id>|terminal:<id>|terminal:<host>/<id>|session:<key>|resource:<key>"
    )]
    UnknownSelector,
    /// A host that is empty, too long, or carries a control character.
    #[error("a scope host must be 1..=255 bytes with no NUL or control character")]
    InvalidHost,
    /// An id that is not a canonical unsigned 32-bit decimal.
    #[error("a scope id must be a canonical unsigned 32-bit decimal")]
    InvalidId,
    /// A key that is not 26 lowercase base32 digits naming a nonzero key.
    #[error("a scope key must be 26 lowercase base32 digits naming a nonzero 16-byte key")]
    InvalidKey,
}

// -----------------------------------------------------------------------------
// Verbs.
// -----------------------------------------------------------------------------

/// The verbs whose bits are set in `bits`, or `None` when `bits` carries a
/// bit outside the current schema.
const fn verbs_from_bits(bits: u8) -> Option<Verbs> {
    verbs_from_schema_bits(bits, ScopeSchema::CURRENT)
}

/// The verbs whose bits are set in `bits`, or `None` when `bits` carries a
/// bit `schema` does not know.
const fn verbs_from_schema_bits(bits: u8, schema: ScopeSchema) -> Option<Verbs> {
    if bits & !schema.known_bits() != 0 {
        return None;
    }
    let mut verbs = Verbs::EMPTY;
    let mut i = 0;
    while i < Verb::ALL.len() {
        let verb = Verb::ALL[i];
        if bits & verb.bit() != 0 {
            verbs = verbs.union(Verbs::of(&[verb]));
        }
        i += 1;
    }
    Some(verbs)
}

/// Every verb in both sets.
fn intersect_verbs(a: Verbs, b: Verbs) -> Verbs {
    verbs_from_bits(a.bits() & b.bits()).unwrap_or(Verbs::EMPTY)
}

/// Every verb of the current schema (all eight).
#[must_use]
pub const fn all_verbs() -> Verbs {
    Verbs::of(&Verb::ALL)
}

/// Every verb of `schema`.
#[must_use]
pub const fn schema_verbs(schema: ScopeSchema) -> Verbs {
    match verbs_from_schema_bits(schema.known_bits(), schema) {
        Some(verbs) => verbs,
        None => Verbs::EMPTY,
    }
}

/// The registry name of `verb`, e.g. `"observe"`.
#[must_use]
pub const fn verb_name(verb: Verb) -> &'static str {
    match verb {
        Verb::Inventory => "inventory",
        Verb::Observe => "observe",
        Verb::Create => "create",
        Verb::Bind => "bind",
        Verb::Input => "input",
        Verb::Signal => "signal",
        Verb::Organize => "organize",
        Verb::Terminate => "terminate",
    }
}

/// Validate a verb set read from a canonical image of `schema`.
const fn checked_verbs(bits: u8, schema: ScopeSchema) -> Result<Verbs, ScopeError> {
    if bits == 0 {
        return Err(ScopeError::ZeroVerbs);
    }
    match verbs_from_schema_bits(bits, schema) {
        Some(verbs) => Ok(verbs),
        None => Err(ScopeError::UnknownVerbBits),
    }
}

/// A grant's verbs and holds as the current schema reads a `schema` grant:
/// a v1 grant gains `ORGANIZE` with `CREATE`, and `TERMINATE` with an
/// un-held `SIGNAL`. A held `?signal` implies nothing, so a hold never
/// turns into an un-held terminating authority.
const fn upgraded(verbs: Verbs, held: Verbs, schema: ScopeSchema) -> Verbs {
    match schema {
        ScopeSchema::V2 => verbs,
        ScopeSchema::V1 if held.contains(Verb::Signal) => {
            if verbs.contains(Verb::Create) {
                verbs.union(Verbs::of(&[Verb::Organize]))
            } else {
                verbs
            }
        }
        ScopeSchema::V1 => verbs.with_v1_implications(),
    }
}

/// The verbs a grant's verb list names under `schema`, and which of them
/// are held. A v1 list may name only the six v1 verbs, and its `*` is those
/// six.
fn parse_schema_verbs(
    text: &str,
    schema: ScopeSchema,
) -> Result<(Verbs, Verbs), ScopeGrammarError> {
    if text == ALL_VERBS {
        return Ok((schema_verbs(schema), Verbs::EMPTY));
    }
    let mut verbs = Verbs::EMPTY;
    let mut held = Verbs::EMPTY;
    for token in text.split(',') {
        let (name, holds) = token
            .strip_prefix(HELD_PREFIX)
            .map_or((token, false), |name| (name, true));
        let verb = parse_verb_name(name)?;
        if verb.bit() & !schema.known_bits() != 0 {
            return Err(ScopeGrammarError::UnknownVerb);
        }
        if verbs.contains(verb) {
            return Err(ScopeGrammarError::RedundantVerb);
        }
        let one = Verbs::of(&[verb]);
        if holds && !HOLDABLE.contains(verb) {
            return Err(ScopeGrammarError::UnholdableVerb);
        }
        verbs = verbs.union(one);
        if holds {
            held = held.union(one);
        }
    }
    Ok((verbs, held))
}

/// One named verb. `*` is only valid as the whole verb list.
fn parse_verb_name(name: &str) -> Result<Verb, ScopeGrammarError> {
    if name == ALL_VERBS {
        return Err(ScopeGrammarError::RedundantVerb);
    }
    VERB_NAMES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, verb)| *verb)
        .ok_or(ScopeGrammarError::UnknownVerb)
}

fn write_verbs(f: &mut fmt::Formatter<'_>, verbs: Verbs, held: Verbs) -> fmt::Result {
    if verbs == all_verbs() && held.is_empty() {
        return f.write_str(ALL_VERBS);
    }
    let names: Vec<String> = verbs
        .iter()
        .map(|verb| {
            let marker = if held.contains(verb) { "?" } else { "" };
            format!("{marker}{}", verb_name(verb))
        })
        .collect();
    f.write_str(&names.join(","))
}

/// The verbs of `verbs` that are not held.
fn unheld(verbs: Verbs, held: Verbs) -> Verbs {
    verbs_from_bits(verbs.bits() & !held.bits()).unwrap_or(Verbs::EMPTY)
}

/// The holds of two merged grants or clauses. A verb one of them carries
/// un-held stays un-held: merging is a union of authority, and a hold only
/// ever restricts.
fn merged_held(a: (Verbs, Verbs), b: (Verbs, Verbs)) -> Verbs {
    let free = unheld(a.0, a.1).union(unheld(b.0, b.1));
    unheld(a.0.union(b.0), free)
}

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

// -----------------------------------------------------------------------------
// Grants and TerminalScopeSet.
// -----------------------------------------------------------------------------

/// One grant: a nonempty verb set on one selector.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeGrant {
    /// The verbs the grant carries. Never empty in a canonical set.
    pub verbs: Verbs,
    /// Whose resources it covers.
    pub selector: Selector,
    /// The verbs of [`Self::verbs`] held for approval (`?signal`,
    /// ADR-0128). Only `SIGNAL` may be held. Not part of the canonical
    /// image: a decoded grant holds nothing.
    pub held: Verbs,
}

impl ScopeGrant {
    /// Parse one registry scope string, `"<verb>[,<verb>...]@<selector>"`.
    ///
    /// # Errors
    ///
    /// The first [`ScopeGrammarError`] rule the string breaks.
    pub fn parse(text: &str) -> Result<Self, ScopeGrammarError> {
        Self::parse_versioned(text, ScopeSchema::CURRENT)
    }

    /// Parse one registry scope string written under `schema`, read as the
    /// current schema reads it: a v1 string may name only the six v1 verbs,
    /// and gains the v2 implications (`SIGNAL` implies `TERMINATE` unless
    /// held, `CREATE` implies `ORGANIZE`).
    ///
    /// # Errors
    ///
    /// The first [`ScopeGrammarError`] rule the string breaks.
    pub fn parse_versioned(text: &str, schema: ScopeSchema) -> Result<Self, ScopeGrammarError> {
        let (verbs, selector) = text.split_once('@').ok_or(ScopeGrammarError::Shape)?;
        let (verbs, held) = parse_schema_verbs(verbs, schema)?;
        Ok(Self {
            verbs: upgraded(verbs, held, schema),
            selector: Selector::parse(selector)?,
            held,
        })
    }

    /// A grant of `verbs` on `selector` with nothing held.
    #[must_use]
    pub const fn new(verbs: Verbs, selector: Selector) -> Self {
        Self {
            verbs,
            selector,
            held: Verbs::EMPTY,
        }
    }
}

impl fmt::Display for ScopeGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_verbs(f, self.verbs, self.held)?;
        write!(f, "@{}", self.selector)
    }
}

/// A canonical set of grants (§5): at most [`MAX_GRANTS`], each selector
/// once, nonzero verbs, strictly increasing by selector bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalScopeSet {
    grants: Vec<ScopeGrant>,
}

impl TerminalScopeSet {
    /// Canonicalize `grants` the way §5's encoder does: merge equal
    /// selectors by OR-ing their verbs, drop empty entries, and sort.
    ///
    /// # Errors
    ///
    /// [`ScopeError::TooManyGrants`] when more than [`MAX_GRANTS`] selectors
    /// remain.
    pub fn from_grants(grants: impl IntoIterator<Item = ScopeGrant>) -> Result<Self, ScopeError> {
        let mut keyed: Vec<(Vec<u8>, ScopeGrant)> = Vec::new();
        for grant in grants.into_iter().filter(|grant| !grant.verbs.is_empty()) {
            merge_grant(&mut keyed, grant);
        }
        if keyed.len() > MAX_GRANTS {
            return Err(ScopeError::TooManyGrants);
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self {
            grants: keyed.into_iter().map(|(_, grant)| grant).collect(),
        })
    }

    /// Parse a registry record's scope strings as one set. One bad string
    /// refuses the whole set: a partial grant is never produced.
    ///
    /// # Errors
    ///
    /// The index (0-based) and rule of the first string that does not parse,
    /// or [`None`] for the rule when the parsed set is over [`MAX_GRANTS`].
    pub fn parse_all<S: AsRef<str>>(
        scopes: &[S],
    ) -> Result<Self, (usize, Option<ScopeGrammarError>)> {
        Self::parse_all_versioned(scopes, ScopeSchema::CURRENT)
    }

    /// [`Self::parse_all`] for a record written under `schema`; see
    /// [`ScopeGrant::parse_versioned`].
    ///
    /// # Errors
    ///
    /// As [`Self::parse_all`].
    pub fn parse_all_versioned<S: AsRef<str>>(
        scopes: &[S],
        schema: ScopeSchema,
    ) -> Result<Self, (usize, Option<ScopeGrammarError>)> {
        let mut grants = Vec::with_capacity(scopes.len());
        for (index, scope) in scopes.iter().enumerate() {
            grants.push(
                ScopeGrant::parse_versioned(scope.as_ref(), schema)
                    .map_err(|rule| (index, Some(rule)))?,
            );
        }
        Self::from_grants(grants).map_err(|_| (scopes.len(), None))
    }

    /// The single grant of every verb on [`Selector::Global`].
    #[must_use]
    pub fn all_global() -> Self {
        Self {
            grants: vec![ScopeGrant::new(all_verbs(), Selector::Global)],
        }
    }

    /// The grants, in canonical order.
    #[must_use]
    pub fn grants(&self) -> &[ScopeGrant] {
        &self.grants
    }

    /// Whether the set grants nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// The canonical bytes of §5.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        push_count(&mut out, self.grants.len());
        for grant in &self.grants {
            push_selector(&mut out, &grant.selector);
            out.push(grant.verbs.bits());
        }
        out
    }

    /// Decode a canonical image, refusing every non-canonical one.
    ///
    /// # Errors
    ///
    /// A [`ScopeError`] naming the first rule the image breaks.
    pub fn decode(bytes: &[u8]) -> Result<Self, ScopeError> {
        Self::decode_versioned(bytes, ScopeSchema::CURRENT)
    }

    /// Decode a canonical image written under `schema`, read as the current
    /// schema reads it: a v1 image refuses bits `0xC0`, and each decoded
    /// grant gains the v2 implications ([`Verbs::with_v1_implications`]).
    ///
    /// # Errors
    ///
    /// A [`ScopeError`] naming the first rule the image breaks.
    pub fn decode_versioned(bytes: &[u8], schema: ScopeSchema) -> Result<Self, ScopeError> {
        let mut reader = Reader::new(bytes);
        let count = reader.count(MAX_GRANTS, ScopeError::TooManyGrants)?;
        let mut grants = Vec::with_capacity(count);
        let mut previous: Option<Vec<u8>> = None;
        for _ in 0..count {
            let (grant, key) = read_grant(&mut reader, previous.as_deref(), schema)?;
            grants.push(grant);
            previous = Some(key);
        }
        reader.finish()?;
        Ok(Self { grants })
    }
}

/// One grant, checked to sort strictly after `previous`; returns it with
/// its ordering key (the raw selector bytes).
fn read_grant(
    reader: &mut Reader<'_>,
    previous: Option<&[u8]>,
    schema: ScopeSchema,
) -> Result<(ScopeGrant, Vec<u8>), ScopeError> {
    let raw = reader.selector_bytes()?;
    check_order(previous, &raw)?;
    let selector = Selector::decode(&raw)?;
    let verbs = checked_verbs(reader.u8()?, schema)?;
    Ok((
        ScopeGrant::new(upgraded(verbs, Verbs::EMPTY, schema), selector),
        raw,
    ))
}

fn merge_grant(keyed: &mut Vec<(Vec<u8>, ScopeGrant)>, grant: ScopeGrant) {
    let key = grant.selector.to_bytes();
    match keyed.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, existing)) => {
            existing.held = merged_held((existing.verbs, existing.held), (grant.verbs, grant.held));
            existing.verbs = existing.verbs.union(grant.verbs);
        }
        None => keyed.push((key, grant)),
    }
}

/// Canonical order: each key strictly greater than the previous one.
fn check_order(previous: Option<&[u8]>, current: &[u8]) -> Result<(), ScopeError> {
    let Some(previous) = previous else {
        return Ok(());
    };
    match previous.cmp(current) {
        std::cmp::Ordering::Less => Ok(()),
        std::cmp::Ordering::Equal => Err(ScopeError::Duplicate),
        std::cmp::Ordering::Greater => Err(ScopeError::Unsorted),
    }
}

// -----------------------------------------------------------------------------
// EffectiveScopeSet.
// -----------------------------------------------------------------------------

/// One conjunctive clause: a subject is covered for `verbs` only when both
/// selectors contain it under the current topology.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectiveClause {
    /// The selector the connection asked for.
    pub requested: Selector,
    /// The registry ceiling's selector.
    pub ceiling: Selector,
    /// The verbs both carry.
    pub verbs: Verbs,
    /// The verbs of [`Self::verbs`] held for approval: held when either
    /// side holds it (ADR-0128). Not part of the canonical image.
    pub held: Verbs,
}

/// The requested set intersected with a ceiling, as conjunctive clauses
/// (§5). A Group or Host ceiling stays a Group or Host clause: it is never
/// laundered into a permanent Terminal grant.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectiveScopeSet {
    clauses: Vec<EffectiveClause>,
}

impl EffectiveScopeSet {
    /// Intersect `requested` with `ceiling`: one clause for every pair whose
    /// verbs meet and whose selectors may overlap, equal pairs merged.
    ///
    /// # Errors
    ///
    /// [`ScopeError::TooManyClauses`] or [`ScopeError::TooLarge`]: the
    /// intersection denies admission rather than truncating authority.
    pub fn intersect(
        requested: &TerminalScopeSet,
        ceiling: &TerminalScopeSet,
    ) -> Result<Self, ScopeError> {
        let mut keyed: KeyedClauses = Vec::new();
        for want in requested.grants() {
            for cap in ceiling.grants() {
                if let Some(clause) = clause_for(want, cap) {
                    merge_clause(&mut keyed, clause);
                }
            }
        }
        if keyed.len() > MAX_CLAUSES {
            return Err(ScopeError::TooManyClauses);
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        let set = Self {
            clauses: keyed.into_iter().map(|(_, clause)| clause).collect(),
        };
        if set.encode().len() > MAX_EFFECTIVE_BYTES {
            return Err(ScopeError::TooLarge);
        }
        Ok(set)
    }

    /// The effective set of a ceiling with no requested attenuation: one
    /// clause `(g, g, verbs)` per grant.
    ///
    /// Authority equals [`Self::intersect`] of the ceiling with itself. An
    /// off-diagonal clause covers only subjects both its selectors contain,
    /// and the diagonal clause for either selector already covers them with
    /// at least the same verbs. The diagonal is also bounded by the
    /// ceiling's own 64 grants, where the full intersection can exceed 64
    /// clauses for a ceiling well under 64 grants.
    ///
    /// # Errors
    ///
    /// [`ScopeError::TooLarge`] when the clauses encode larger than
    /// [`MAX_EFFECTIVE_BYTES`].
    pub fn unattenuated(ceiling: &TerminalScopeSet) -> Result<Self, ScopeError> {
        let clauses = ceiling
            .grants()
            .iter()
            .map(|grant| EffectiveClause {
                requested: grant.selector.clone(),
                ceiling: grant.selector.clone(),
                verbs: grant.verbs,
                held: grant.held,
            })
            .collect();
        let set = Self { clauses };
        if set.encode().len() > MAX_EFFECTIVE_BYTES {
            return Err(ScopeError::TooLarge);
        }
        Ok(set)
    }

    /// The clauses, in canonical order.
    #[must_use]
    pub fn clauses(&self) -> &[EffectiveClause] {
        &self.clauses
    }

    /// Whether the set covers nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// Whether some clause carries `verb` and both of its selectors contain
    /// the subject, as `contains` decides against the live topology.
    ///
    /// A held verb is admitted here; `without_holds().admits(..)` asks
    /// whether it reaches the subject without a decision (ADR-0128).
    pub fn admits(&self, verb: Verb, contains: impl Fn(&Selector) -> bool) -> bool {
        self.clauses.iter().any(|clause| {
            clause.verbs.contains(verb) && contains(&clause.requested) && contains(&clause.ceiling)
        })
    }

    /// Whether any clause holds a verb for approval.
    #[must_use]
    pub fn holds_any(&self) -> bool {
        self.clauses.iter().any(|clause| !clause.held.is_empty())
    }

    /// The same clauses with every held verb removed: what the grant admits
    /// without a decision. A clause left with no verb is dropped.
    #[must_use]
    pub fn without_holds(&self) -> Self {
        let clauses = self
            .clauses
            .iter()
            .map(|clause| EffectiveClause {
                verbs: unheld(clause.verbs, clause.held),
                held: Verbs::EMPTY,
                ..clause.clone()
            })
            .filter(|clause| !clause.verbs.is_empty())
            .collect();
        Self { clauses }
    }

    /// Whether any clause carries `verb`, whatever its selectors.
    #[must_use]
    pub fn carries(&self, verb: Verb) -> bool {
        self.clauses
            .iter()
            .any(|clause| clause.verbs.contains(verb))
    }

    /// The canonical bytes of §5.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        push_count(&mut out, self.clauses.len());
        for clause in &self.clauses {
            push_selector(&mut out, &clause.requested);
            push_selector(&mut out, &clause.ceiling);
            out.push(clause.verbs.bits());
        }
        out
    }

    /// Decode a canonical image, refusing every non-canonical one.
    ///
    /// # Errors
    ///
    /// A [`ScopeError`] naming the first rule the image breaks.
    pub fn decode(bytes: &[u8]) -> Result<Self, ScopeError> {
        if bytes.len() > MAX_EFFECTIVE_BYTES {
            return Err(ScopeError::TooLarge);
        }
        let mut reader = Reader::new(bytes);
        let count = reader.count(MAX_CLAUSES, ScopeError::TooManyClauses)?;
        let mut clauses = Vec::with_capacity(count);
        let mut previous: Option<Vec<u8>> = None;
        for _ in 0..count {
            let (clause, key) = read_clause(&mut reader, previous.as_deref())?;
            clauses.push(clause);
            previous = Some(key);
        }
        reader.finish()?;
        Ok(Self { clauses })
    }
}

/// One clause, checked to sort strictly after `previous`; returns it with
/// its ordering key. Selector encodings are prefix-free, so ordering the
/// concatenation `requested || ceiling` equals ordering the pair.
fn read_clause(
    reader: &mut Reader<'_>,
    previous: Option<&[u8]>,
) -> Result<(EffectiveClause, Vec<u8>), ScopeError> {
    let requested_raw = reader.selector_bytes()?;
    let ceiling_raw = reader.selector_bytes()?;
    let key = [requested_raw.as_slice(), ceiling_raw.as_slice()].concat();
    check_order(previous, &key)?;
    let clause = EffectiveClause {
        requested: Selector::decode(&requested_raw)?,
        ceiling: Selector::decode(&ceiling_raw)?,
        verbs: checked_verbs(reader.u8()?, ScopeSchema::CURRENT)?,
        held: Verbs::EMPTY,
    };
    Ok((clause, key))
}

fn clause_for(want: &ScopeGrant, cap: &ScopeGrant) -> Option<EffectiveClause> {
    let verbs = intersect_verbs(want.verbs, cap.verbs);
    if verbs.is_empty() || !want.selector.may_overlap(&cap.selector) {
        return None;
    }
    Some(EffectiveClause {
        requested: want.selector.clone(),
        ceiling: cap.selector.clone(),
        verbs,
        held: intersect_verbs(verbs, want.held.union(cap.held)),
    })
}

/// A clause's ordering key: its requested and ceiling selector bytes.
type ClauseKey = (Vec<u8>, Vec<u8>);

/// Clauses being merged, each beside its ordering key.
type KeyedClauses = Vec<(ClauseKey, EffectiveClause)>;

fn merge_clause(keyed: &mut KeyedClauses, clause: EffectiveClause) {
    let key = (clause.requested.to_bytes(), clause.ceiling.to_bytes());
    match keyed.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, existing)) => {
            existing.held =
                merged_held((existing.verbs, existing.held), (clause.verbs, clause.held));
            existing.verbs = existing.verbs.union(clause.verbs);
        }
        None => keyed.push((key, clause)),
    }
}

// -----------------------------------------------------------------------------
// Byte helpers.
// -----------------------------------------------------------------------------

fn push_count(out: &mut Vec<u8>, count: usize) {
    // Both sets are bounded at 64 entries, so the count always fits.
    let count = u16::try_from(count).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_be_bytes());
}

fn push_selector(out: &mut Vec<u8>, selector: &Selector) {
    let bytes = selector.to_bytes();
    // The longest selector is a satellite Terminal: 2 + 2 + 255 + 4 bytes.
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&bytes);
}

/// A bounds-checked big-endian reader over one image.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    const fn take(&mut self, len: usize) -> Result<&'a [u8], ScopeError> {
        if self.bytes.len() < len {
            return Err(ScopeError::Truncated);
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, ScopeError> {
        Ok(self.take(1)?[0])
    }

    /// A `U16` entry count, refused with `too_many` above `max`.
    fn count(&mut self, max: usize, too_many: ScopeError) -> Result<usize, ScopeError> {
        let count = usize::from(self.u16()?);
        if count > max {
            return Err(too_many);
        }
        Ok(count)
    }

    fn u16(&mut self) -> Result<u16, ScopeError> {
        let raw = self.take(2)?;
        Ok(u16::from_be_bytes([raw[0], raw[1]]))
    }

    fn u32(&mut self) -> Result<u32, ScopeError> {
        let raw = self.take(4)?;
        Ok(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    /// `U16(len) || selector_bytes`, returned raw for order checks.
    fn selector_bytes(&mut self) -> Result<Vec<u8>, ScopeError> {
        let len = usize::from(self.u16()?);
        Ok(self.take(len)?.to_vec())
    }

    /// A 16-byte key, refused when `new` rejects it (all zero).
    fn key<K>(&mut self, new: impl FnOnce([u8; 16]) -> Option<K>) -> Result<K, ScopeError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        new(bytes).ok_or(ScopeError::InvalidKey)
    }

    /// `V16(host)`, validated.
    fn host(&mut self) -> Result<Host, ScopeError> {
        let len = usize::from(self.u16()?);
        let raw = self.take(len)?;
        let text = std::str::from_utf8(raw).map_err(|_| ScopeError::InvalidHost)?;
        Host::new(text)
    }

    const fn finish(&self) -> Result<(), ScopeError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(ScopeError::TrailingBytes)
        }
    }
}
