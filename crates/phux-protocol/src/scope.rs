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

mod effective;
mod grant;
mod reader;
mod selector;
mod verbs;

pub use effective::{EffectiveClause, EffectiveScopeSet};
pub use grant::{ScopeGrant, TerminalScopeSet};
pub use selector::{Host, Selector};
pub use verbs::{all_verbs, schema_verbs, verb_name};
