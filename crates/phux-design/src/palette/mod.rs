//! The command palette's matcher and ranker, shared by every phux client.
//!
//! One behavior everywhere: type any words of a title, in any order, as
//! initials or fragments ("tab left", "ns" for New session), and the best
//! rows come first with exactly the letters that matched lit. A row may also
//! answer to aliases (a synonym, its action's canonical name) and a detail
//! line, which rank lower and never light the title.
//!
//! The painters stay per client; this only decides order and highlights.

mod align;

/// A parsed query: the words a row must all match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    words: Vec<Word>,
    /// The whole query, lowercase and single-spaced, for exact and prefix
    /// tiers.
    whole: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    chars: Vec<char>,
    /// Smart case: a word with an upper-case letter matches case exactly.
    case_sensitive: bool,
}

impl Query {
    /// Split `text` into words on whitespace.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let words: Vec<Word> = text
            .split_whitespace()
            .map(|word| Word {
                case_sensitive: word.chars().any(char::is_uppercase),
                chars: word.chars().collect(),
            })
            .collect();
        let whole = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        Self { words, whole }
    }

    /// Whether nothing was typed (every row matches, in catalog order).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// The text of one row that a query can match.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fields<'a> {
    /// What the row says; matches here rank highest and are lit.
    pub title: &'a str,
    /// Other names the row answers to (synonyms, a canonical action name).
    pub aliases: &'a [&'a str],
    /// A secondary line (a path, a question, a session name).
    pub detail: Option<&'a str>,
}

/// How well a row matched, and which title characters to light.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hit {
    /// Higher is better. Only the order it induces is contractual.
    pub score: i32,
    /// Char indices into the title that matched, ascending and unique.
    pub title_positions: Vec<usize>,
}

/// An alias match is worth this fraction (in percent) of a title match.
const ALIAS_WEIGHT: i32 = 60;
/// A detail match is worth this fraction (in percent) of a title match.
const DETAIL_WEIGHT: i32 = 40;
/// The query is the whole title.
const TIER_EXACT: i32 = 400;
/// The title starts with the whole query.
const TIER_PREFIX: i32 = 160;
/// The query is a whole alias.
const TIER_ALIAS_EXACT: i32 = 220;
/// Longer titles lose ties: a point per this many characters.
const LENGTH_STEP: i32 = 4;
/// The most length can cost.
const LENGTH_CAP: i32 = 16;

/// Score `fields` against `query`: every word must match the title, an alias
/// or the detail. `None` when some word matches none of them. An empty query
/// matches everything with score 0.
#[must_use]
pub fn score(query: &Query, fields: &Fields<'_>) -> Option<Hit> {
    if query.is_empty() {
        return Some(Hit::default());
    }
    let title: Vec<char> = fields.title.chars().collect();
    let aliases: Vec<Vec<char>> = fields.aliases.iter().map(|a| a.chars().collect()).collect();
    let detail: Option<Vec<char>> = fields.detail.map(|d| d.chars().collect());
    let mut total = 0;
    let mut lit = Vec::new();
    for word in &query.words {
        let (points, positions) = best_place(word, &title, &aliases, detail.as_deref())?;
        total += points;
        lit.extend(positions);
    }
    lit.sort_unstable();
    lit.dedup();
    total += tiers(query, fields);
    let length = i32::try_from(title.len()).unwrap_or(i32::MAX) / LENGTH_STEP;
    total -= length.min(LENGTH_CAP);
    Some(Hit {
        score: total,
        title_positions: lit,
    })
}

/// The best place for one word: the title (lit), else an alias or the
/// detail at a discount (unlit).
fn best_place(
    word: &Word,
    title: &[char],
    aliases: &[Vec<char>],
    detail: Option<&[char]>,
) -> Option<(i32, Vec<usize>)> {
    let in_title = align::align(&word.chars, title, word.case_sensitive);
    let in_alias = aliases
        .iter()
        .filter_map(|alias| align::align(&word.chars, alias, word.case_sensitive))
        .map(|(s, _)| s * ALIAS_WEIGHT / 100)
        .max();
    let in_detail = detail
        .and_then(|d| align::align(&word.chars, d, word.case_sensitive))
        .map(|(s, _)| s * DETAIL_WEIGHT / 100);
    let elsewhere = in_alias.into_iter().chain(in_detail).max();
    let title_points = in_title.as_ref().map(|(points, _)| *points);
    if let Some(other) = elsewhere
        && title_points.is_none_or(|points| other > points)
    {
        return Some((other, Vec::new()));
    }
    in_title
}

/// Bonuses for the whole query: exactly the title, its start, or an alias.
fn tiers(query: &Query, fields: &Fields<'_>) -> i32 {
    let title = fields.title.to_lowercase();
    if title == query.whole {
        return TIER_EXACT;
    }
    let alias_exact = fields
        .aliases
        .iter()
        .any(|alias| alias.to_lowercase() == query.whole);
    let prefix = title.starts_with(&query.whole);
    match (prefix, alias_exact) {
        (_, true) => TIER_ALIAS_EXACT,
        (true, false) => TIER_PREFIX,
        (false, false) => 0,
    }
}

/// The matching rows best first, each with its [`Hit`]. `boost` adds a
/// caller's preference (recent use, attention) to a row's score; ties keep
/// candidate order, so an empty query returns the catalog as given.
#[must_use]
pub fn rank<'a>(
    query: &Query,
    candidates: impl IntoIterator<Item = (Fields<'a>, i32)>,
) -> Vec<(usize, Hit)> {
    let mut hits: Vec<(usize, Hit)> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, (fields, boost))| {
            score(query, &fields).map(|mut hit| {
                hit.score += boost;
                (index, hit)
            })
        })
        .collect();
    hits.sort_by(|(ia, a), (ib, b)| b.score.cmp(&a.score).then(ia.cmp(ib)));
    hits
}

/// Group `ordered` rows (best first) under their sections.
///
/// Sections come in the order each one's best row appears, and rows keep
/// their order within a section, so the top result always leads.
#[must_use]
pub fn group<S: PartialEq + Clone>(
    ordered: &[usize],
    section: impl Fn(usize) -> S,
) -> Vec<(S, Vec<usize>)> {
    let mut groups: Vec<(S, Vec<usize>)> = Vec::new();
    for &row in ordered {
        let key = section(row);
        match groups.iter_mut().find(|(s, _)| *s == key) {
            Some((_, rows)) => rows.push(row),
            None => groups.push((key, vec![row])),
        }
    }
    groups
}

#[cfg(test)]
mod tests;
