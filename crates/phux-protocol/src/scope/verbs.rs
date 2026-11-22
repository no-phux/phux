//! Verb-set bit codecs, names, and the held-verb helpers.

use super::{
    ALL_VERBS, HELD_PREFIX, HOLDABLE, ScopeError, ScopeGrammarError, ScopeSchema, VERB_NAMES,
};
use crate::kinds::{Verb, Verbs};
use std::fmt;

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
pub(super) fn intersect_verbs(a: Verbs, b: Verbs) -> Verbs {
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
pub(super) const fn checked_verbs(bits: u8, schema: ScopeSchema) -> Result<Verbs, ScopeError> {
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
pub(super) const fn upgraded(verbs: Verbs, held: Verbs, schema: ScopeSchema) -> Verbs {
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
pub(super) fn parse_schema_verbs(
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

pub(super) fn write_verbs(f: &mut fmt::Formatter<'_>, verbs: Verbs, held: Verbs) -> fmt::Result {
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
pub(super) fn unheld(verbs: Verbs, held: Verbs) -> Verbs {
    verbs_from_bits(verbs.bits() & !held.bits()).unwrap_or(Verbs::EMPTY)
}

/// The holds of two merged grants or clauses. A verb one of them carries
/// un-held stays un-held: merging is a union of authority, and a hold only
/// ever restricts.
pub(super) fn merged_held(a: (Verbs, Verbs), b: (Verbs, Verbs)) -> Verbs {
    let free = unheld(a.0, a.1).union(unheld(b.0, b.1));
    unheld(a.0.union(b.0), free)
}
