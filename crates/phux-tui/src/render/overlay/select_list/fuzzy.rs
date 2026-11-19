//! Scored fuzzy matching for the list filter.

/// Indices of the `(index, haystack)` pairs a lowercased `query` fuzzy-matches,
/// best first; ties keep input order.
pub(in crate::render::overlay) fn fuzzy_rank(
    query: &str,
    haystacks: impl Iterator<Item = (usize, String)>,
) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = haystacks
        .filter_map(|(i, hay)| fuzzy_score(query, &hay.to_lowercase()).map(|score| (score, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Scored subsequence match of a lowercased `needle` in `haystack` (`None`
/// when not a subsequence). Only the induced ordering is contractual.
#[must_use]
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    fuzzy_match(needle, haystack).map(|(score, _)| score)
}

/// [`fuzzy_score`] plus the char positions in `haystack` it matched, in
/// order (empty for an empty needle).
pub(super) fn fuzzy_match(needle: &str, haystack: &str) -> Option<(i32, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let hay: Vec<char> = haystack.chars().collect();
    let mut score: i32 = 0;
    let mut run: i32 = 0;
    let mut hay_idx: usize = 0;
    let mut last_match: Option<usize> = None;
    let mut hits = Vec::with_capacity(needle.len());

    for nc in needle.chars() {
        let found = hay[hay_idx..].iter().position(|&hc| hc == nc)?;
        let pos = hay_idx + found;
        hits.push(pos);

        // Gap penalty: earlier, tighter matches win.
        let gap = last_match.map_or(pos, |prev| pos - prev - 1);
        score -= i32::try_from(gap).unwrap_or(i32::MAX).min(20);

        // Word-boundary / prefix bonus.
        let at_boundary = pos == 0
            || hay
                .get(pos - 1)
                .is_some_and(|c| matches!(c, '-' | '_' | ' ' | ':' | '/'));
        if at_boundary {
            score += 10;
        }

        if last_match.is_some_and(|prev| prev + 1 == pos) {
            run += 1;
            score += 5 + run * 5;
        } else {
            run = 0;
        }

        last_match = Some(pos);
        hay_idx = pos + 1;
    }
    Some((score, hits))
}
