//! The conjunctive [`EffectiveScopeSet`].

use super::grant::{ScopeGrant, TerminalScopeSet, check_order};
use super::reader::{Reader, push_count, push_selector};
use super::selector::Selector;
use super::verbs::{checked_verbs, intersect_verbs, merged_held, unheld};
use super::{MAX_CLAUSES, MAX_EFFECTIVE_BYTES, ScopeError, ScopeSchema};
use crate::kinds::{Verb, Verbs};

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
