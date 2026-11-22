//! The list filter, on the shared [`phux_design::palette`] ranker: the same
//! matching and highlights as every other phux palette.

use phux_design::palette::{Fields, Query, rank, score};

/// Indices of the `(index, haystack)` pairs `query` matches, best first;
/// ties keep input order.
pub(in crate::render::overlay) fn fuzzy_rank(
    query: &str,
    haystacks: impl Iterator<Item = (usize, String)>,
) -> Vec<usize> {
    let query = Query::parse(query);
    let pairs: Vec<(usize, String)> = haystacks.collect();
    rank(
        &query,
        pairs.iter().map(|(_, hay)| {
            (
                Fields {
                    title: hay,
                    ..Fields::default()
                },
                0,
            )
        }),
    )
    .into_iter()
    .map(|(at, _)| pairs[at].0)
    .collect()
}

/// `query`'s score against `haystack` (`None` when some word of it does not
/// match). Only the ordering it induces is contractual.
#[must_use]
pub fn fuzzy_score(query: &str, haystack: &str) -> Option<i32> {
    score(
        &Query::parse(query),
        &Fields {
            title: haystack,
            ..Fields::default()
        },
    )
    .map(|hit| hit.score)
}
