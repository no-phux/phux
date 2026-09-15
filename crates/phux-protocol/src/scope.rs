//! Closed scope grants for the terminal endpoint (`docs/spec/workload-auth.md`
//! §5).
//!
//! A grant pairs one nonempty [`Verbs`] set with one [`Selector`]. A
//! [`TerminalScopeSet`] is a canonical set of grants, and an
//! [`EffectiveScopeSet`] is the conjunctive intersection of a requested set
//! with a registry ceiling: every clause keeps both selectors, so a Group or
//! Host ceiling is never flattened into a bare Terminal grant.
//!
//! Two spellings exist and both are strict:
//!
//! - the canonical bytes of §5, whose decoder refuses an unsorted or
//!   duplicate selector, an unknown verb bit, a zero verb set, a non-minimal
//!   selector length, a count or length mismatch, truncation, and trailing
//!   bytes, and never normalizes an invalid image before using it; and
//! - the human registry grammar `"<verb>[,<verb>...]@<selector>"` the
//!   `workload-keys` file and `phux workload add-key --scope` carry.
//!
//! Nothing here consults server state. Whether a selector contains a subject
//! depends on the live topology (a Terminal's current Group), so the server
//! passes a containment predicate to [`EffectiveScopeSet::admits`]. The verbs
//! are [`crate::kinds::Verb`], byte-equal to the §5 bits, so the classifier
//! and the grant speak one vocabulary.

use std::fmt;

pub use crate::kinds::{Verb, Verbs};

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
const SUB_LOCAL: u8 = 0x00;
const SUB_SATELLITE: u8 = 0x01;

/// The registry spelling of every verb, in bit order.
const VERB_NAMES: [(&str, Verb); 6] = [
    ("inventory", Verb::Inventory),
    ("observe", Verb::Observe),
    ("create", Verb::Create),
    ("bind", Verb::Bind),
    ("input", Verb::Input),
    ("signal", Verb::Signal),
];

/// The wildcard verb: all six, and only on its own.
const ALL_VERBS: &str = "*";

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
    /// A selector tag outside `0x00..=0x03`.
    #[error("scope selector tag is unknown")]
    UnknownSelectorTag,
    /// A Host or Terminal subtype outside `LOCAL` / `SATELLITE`.
    #[error("scope selector subtype is unknown")]
    UnknownSubtype,
    /// A satellite host that is empty, too long, not UTF-8, or carries a
    /// control character.
    #[error("scope host must be 1..=255 UTF-8 bytes with no NUL or control character")]
    InvalidHost,
    /// A grant or clause with no verb.
    #[error("scope verbs must be nonzero")]
    ZeroVerbs,
    /// A verb bit outside the six v1 verbs (`0xC0`).
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
    /// A verb outside the closed set.
    #[error("a scope names a verb outside inventory|observe|create|bind|input|signal|*")]
    UnknownVerb,
    /// A verb listed twice, or `*` combined with named verbs.
    #[error("a scope repeats a verb or combines `*` with named verbs")]
    RedundantVerb,
    /// A selector outside the closed set.
    #[error(
        "a scope names a selector outside global|host|host:<name>|group:<id>|terminal:<id>|terminal:<host>/<id>"
    )]
    UnknownSelector,
    /// A host that is empty, too long, or carries a control character.
    #[error("a scope host must be 1..=255 bytes with no NUL or control character")]
    InvalidHost,
    /// An id that is not a canonical unsigned 32-bit decimal.
    #[error("a scope id must be a canonical unsigned 32-bit decimal")]
    InvalidId,
}

// -----------------------------------------------------------------------------
// Verbs.
// -----------------------------------------------------------------------------

/// The verbs whose bits are set in `bits`, or `None` when `bits` carries an
/// unknown bit.
const fn verbs_from_bits(bits: u8) -> Option<Verbs> {
    if bits & !Verbs::KNOWN_BITS != 0 {
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

/// All six verbs.
#[must_use]
pub const fn all_verbs() -> Verbs {
    Verbs::of(&Verb::ALL)
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
    }
}

/// Validate a verb set read from a canonical image.
const fn checked_verbs(bits: u8) -> Result<Verbs, ScopeError> {
    if bits == 0 {
        return Err(ScopeError::ZeroVerbs);
    }
    match verbs_from_bits(bits) {
        Some(verbs) => Ok(verbs),
        None => Err(ScopeError::UnknownVerbBits),
    }
}

fn parse_verbs(text: &str) -> Result<Verbs, ScopeGrammarError> {
    if text == ALL_VERBS {
        return Ok(all_verbs());
    }
    let mut verbs = Verbs::EMPTY;
    for name in text.split(',') {
        if name == ALL_VERBS {
            return Err(ScopeGrammarError::RedundantVerb);
        }
        let verb = VERB_NAMES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, verb)| *verb)
            .ok_or(ScopeGrammarError::UnknownVerb)?;
        if verbs.contains(verb) {
            return Err(ScopeGrammarError::RedundantVerb);
        }
        verbs = verbs.union(Verbs::of(&[verb]));
    }
    Ok(verbs)
}

fn write_verbs(f: &mut fmt::Formatter<'_>, verbs: Verbs) -> fmt::Result {
    if verbs == all_verbs() {
        return f.write_str(ALL_VERBS);
    }
    let names: Vec<&str> = verbs.iter().map(verb_name).collect();
    f.write_str(&names.join(","))
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
            _ => return Err(ScopeError::UnknownSelectorTag),
        };
        if !reader.is_empty() {
            return Err(ScopeError::NonMinimalSelector);
        }
        Ok(selector)
    }

    /// Parse the registry spelling: `global`, `host`, `host:<name>`,
    /// `group:<u32>`, `terminal:<u32>`, or `terminal:<host>/<u32>`.
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
            _ => Err(ScopeGrammarError::UnknownSelector),
        }
    }

    /// A short, identity-free name for the selector's kind (`global`,
    /// `host`, `group`, `terminal`), for diagnostics that must not name a
    /// subject.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::HostLocal | Self::HostSatellite(_) => "host",
            Self::Group(_) => "group",
            Self::TerminalLocal(_) | Self::TerminalSatellite(..) => "terminal",
        }
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
/// distinct, non-Global selectors.
fn overlaps_one_way(outer: &Selector, inner: &Selector) -> bool {
    match (outer, inner) {
        (Selector::HostLocal, Selector::Group(_) | Selector::TerminalLocal(_))
        | (Selector::Group(_), Selector::TerminalLocal(_)) => true,
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
}

impl ScopeGrant {
    /// Parse one registry scope string, `"<verb>[,<verb>...]@<selector>"`.
    ///
    /// # Errors
    ///
    /// The first [`ScopeGrammarError`] rule the string breaks.
    pub fn parse(text: &str) -> Result<Self, ScopeGrammarError> {
        let (verbs, selector) = text.split_once('@').ok_or(ScopeGrammarError::Shape)?;
        let verbs = parse_verbs(verbs)?;
        Ok(Self {
            verbs,
            selector: Selector::parse(selector)?,
        })
    }
}

impl fmt::Display for ScopeGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_verbs(f, self.verbs)?;
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
        let mut grants = Vec::with_capacity(scopes.len());
        for (index, scope) in scopes.iter().enumerate() {
            grants.push(ScopeGrant::parse(scope.as_ref()).map_err(|rule| (index, Some(rule)))?);
        }
        Self::from_grants(grants).map_err(|_| (scopes.len(), None))
    }

    /// The single grant of all six verbs on [`Selector::Global`].
    #[must_use]
    pub fn all_global() -> Self {
        Self {
            grants: vec![ScopeGrant {
                verbs: all_verbs(),
                selector: Selector::Global,
            }],
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
        let mut reader = Reader::new(bytes);
        let count = reader.count(MAX_GRANTS, ScopeError::TooManyGrants)?;
        let mut grants = Vec::with_capacity(count);
        let mut previous: Option<Vec<u8>> = None;
        for _ in 0..count {
            let (grant, key) = read_grant(&mut reader, previous.as_deref())?;
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
) -> Result<(ScopeGrant, Vec<u8>), ScopeError> {
    let raw = reader.selector_bytes()?;
    check_order(previous, &raw)?;
    let selector = Selector::decode(&raw)?;
    let verbs = checked_verbs(reader.u8()?)?;
    Ok((ScopeGrant { verbs, selector }, raw))
}

fn merge_grant(keyed: &mut Vec<(Vec<u8>, ScopeGrant)>, grant: ScopeGrant) {
    let key = grant.selector.to_bytes();
    match keyed.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, existing)) => existing.verbs = existing.verbs.union(grant.verbs),
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
    pub fn admits(&self, verb: Verb, contains: impl Fn(&Selector) -> bool) -> bool {
        self.clauses.iter().any(|clause| {
            clause.verbs.contains(verb) && contains(&clause.requested) && contains(&clause.ceiling)
        })
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
        verbs: checked_verbs(reader.u8()?)?,
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
    })
}

/// A clause's ordering key: its requested and ceiling selector bytes.
type ClauseKey = (Vec<u8>, Vec<u8>);

/// Clauses being merged, each beside its ordering key.
type KeyedClauses = Vec<(ClauseKey, EffectiveClause)>;

fn merge_clause(keyed: &mut KeyedClauses, clause: EffectiveClause) {
    let key = (clause.requested.to_bytes(), clause.ceiling.to_bytes());
    match keyed.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, existing)) => existing.verbs = existing.verbs.union(clause.verbs),
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
