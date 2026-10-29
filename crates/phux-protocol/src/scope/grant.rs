//! [`ScopeGrant`] and the canonical [`TerminalScopeSet`].

use super::reader::{Reader, push_count, push_selector};
use super::selector::Selector;
use super::verbs::{
    all_verbs, checked_verbs, merged_held, parse_schema_verbs, upgraded, write_verbs,
};
use super::{MAX_GRANTS, ScopeError, ScopeGrammarError, ScopeSchema};
use crate::kinds::Verbs;
use std::fmt;

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
pub(super) fn check_order(previous: Option<&[u8]>, current: &[u8]) -> Result<(), ScopeError> {
    let Some(previous) = previous else {
        return Ok(());
    };
    match previous.cmp(current) {
        std::cmp::Ordering::Less => Ok(()),
        std::cmp::Ordering::Equal => Err(ScopeError::Duplicate),
        std::cmp::Ordering::Greater => Err(ScopeError::Unsorted),
    }
}
