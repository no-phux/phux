//! Prepared search text for Go To.
//!
//! Ranking reads folded fields built once for a metadata revision. A query or
//! chip change reuses them. A connection drain or a local tab change rebuilds
//! them. The query and each field are capped, so a keystroke never folds or
//! scans an unbounded string. Display text stays whole; only the searched
//! tail is dropped.
//!
//! Case folding is Unicode lowercase, the same fold the palette already uses
//! to highlight matches. Bounds sit on character and grapheme edges.

use unicode_segmentation::UnicodeSegmentation;

use super::goto_filter::{GotoCandidate, GotoFilter};
use super::{Row, Section, goto_id};

/// A query longer than this is searched only through its first characters.
pub(super) const MAX_QUERY_CHARS: usize = 256;
/// Terms past this are not required. A person does not type a sentence of tokens.
pub(super) const MAX_TERMS: usize = 16;
/// Each searched field keeps this many graphemes. The rest is still shown.
pub(super) const MAX_HAYSTACK_GRAPHEMES: usize = 512;
/// Defensive cap on the folded bytes of one field. Lowercase and emoji stay
/// inside it for a full grapheme budget; a pathological expansion is cut on a
/// grapheme edge rather than stored whole.
const MAX_BYTES_PER_GRAPHEME: usize = 32;

/// What a prepared Go To index was built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SearchRevision {
    /// [`crate::connection::Connection::revision`], bumped on each drain.
    pub(super) connection: u64,
    /// Local tabs. A drain does not cover a title edit or a split.
    pub(super) workspace: u64,
}

/// A trimmed, folded, bounded query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PreparedQuery {
    /// Full folded query, used for an exact or prefix title bonus.
    folded: String,
    terms: Vec<String>,
}

impl PreparedQuery {
    pub(super) fn parse(text: &str) -> Self {
        let trimmed = text.trim();
        let capped = char_prefix(trimmed, MAX_QUERY_CHARS);
        let folded = fold_one(capped, MAX_QUERY_CHARS);
        let mut terms: Vec<String> = folded.split_whitespace().map(str::to_owned).collect();
        terms.truncate(MAX_TERMS);
        Self { folded, terms }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub(super) fn into_terms(self) -> Vec<String> {
        self.terms
    }

    fn score_fields(&self, title: &str, metadata: &str) -> Option<i64> {
        let score = self.terms.iter().try_fold(0, |total, token| {
            let title_score = literal_score(title, token)
                .map(|score| score * 4)
                .or_else(|| subsequence_score(title, token));
            // Long descriptions are useful search terms, not a fuzzy-match canvas.
            let metadata_score = literal_score(metadata, token);
            title_score
                .into_iter()
                .chain(metadata_score)
                .max()
                .map(|score| total + score)
        })?;
        let priority = if title == self.folded {
            10_000
        } else if title.starts_with(&self.folded) {
            2_000
        } else {
            0
        };
        Some(score + priority)
    }
}

/// Folded title and metadata for one row. Both are grapheme-capped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PreparedFields {
    title: String,
    metadata: String,
}

impl PreparedFields {
    pub(super) fn from_row(row: &Row) -> Self {
        let mut metadata = Fold::new(MAX_HAYSTACK_GRAPHEMES);
        // "{category} {detail} {keywords}", then the agent fields the row
        // already carries.
        let agent = row.agent.as_ref();
        let parts = [
            Some(row.category),
            Some(row.detail.as_deref().unwrap_or("")),
            Some(&row.keywords),
            agent.and_then(|agent| agent.kind.as_deref()),
            agent.map(|agent| agent.name.as_str()),
            agent
                .map(|agent| agent.word())
                .filter(|word| !word.is_empty()),
        ];
        for (index, part) in parts.into_iter().flatten().enumerate() {
            if index > 0 {
                metadata.push(" ");
            }
            metadata.push(part);
        }
        Self {
            title: fold_one(&row.title, MAX_HAYSTACK_GRAPHEMES),
            metadata: metadata.finish(),
        }
    }

    fn score(&self, query: &PreparedQuery) -> Option<i64> {
        query.score_fields(&self.title, &self.metadata)
    }
}

struct PreparedHit {
    fields: PreparedFields,
    row: Row,
}

struct PreparedSection {
    label: Option<&'static str>,
    rows: Vec<PreparedHit>,
}

/// Prepared Go To rows, rebuilt when metadata revisions change.
#[derive(Default)]
pub(crate) struct GotoSearch {
    revision: Option<SearchRevision>,
    prepared: Vec<PreparedSection>,
    /// Folded query the current `ranked` rows were scored with. Bounded, so a
    /// long paste is not retained as the cache key.
    ranked_folded: String,
    ranked_filter: Option<GotoFilter>,
    /// [`super::goto_frecency::Store::generation`] the `ranked` rows used.
    ranked_generation: u64,
    ranked: Vec<Section>,
}

impl GotoSearch {
    pub(super) fn needs_rebuild(&self, revision: SearchRevision) -> bool {
        self.revision != Some(revision)
    }

    pub(super) fn rebuild(&mut self, revision: SearchRevision, sections: Vec<Section>) {
        self.prepared = sections
            .into_iter()
            .map(|section| PreparedSection {
                label: section.label,
                rows: section
                    .rows
                    .into_iter()
                    .map(|row| {
                        let fields = PreparedFields::from_row(&row);
                        PreparedHit { fields, row }
                    })
                    .collect(),
            })
            .collect();
        self.revision = Some(revision);
        // The rows changed. The previous ranking indexes the old fields.
        self.ranked_filter = None;
    }

    /// Whether `ranked` already reflects `query`, `filter`, and the frecency
    /// generation. A chip or a keystroke reuses it; a visit does not.
    pub(super) fn rank_current(&self, query: &str, filter: GotoFilter, generation: u64) -> bool {
        self.ranked_filter == Some(filter)
            && self.ranked_generation == generation
            && self.ranked_folded == PreparedQuery::parse(query).folded
    }

    pub(super) fn ensure_ranked(
        &mut self,
        query: &str,
        filter: GotoFilter,
        generation: u64,
        boost: impl Fn(&Row) -> i64,
    ) {
        let parsed = PreparedQuery::parse(query);
        if self.rank_current(query, filter, generation) {
            return;
        }
        self.ranked = rank_prepared(&self.prepared, &parsed, filter, &boost);
        self.ranked_folded = parsed.folded;
        self.ranked_filter = Some(filter);
        self.ranked_generation = generation;
    }

    pub(super) fn ranked(&self) -> &[Section] {
        &self.ranked
    }

    pub(super) fn cloned_sections(&self) -> Vec<Section> {
        self.ranked.clone()
    }

    /// Every prepared row, before a chip or a query hides it.
    pub(super) fn candidates(&self) -> Vec<GotoCandidate> {
        self.prepared
            .iter()
            .flat_map(|section| &section.rows)
            .filter_map(|hit| {
                Some(GotoCandidate {
                    id: goto_id(&hit.row)?,
                    agent: hit.row.agent.clone(),
                })
            })
            .collect()
    }
}

/// Rank prepared rows. An empty query keeps sections; a typed query is one list.
fn rank_prepared(
    sections: &[PreparedSection],
    parsed: &PreparedQuery,
    filter: GotoFilter,
    boost: &impl Fn(&Row) -> i64,
) -> Vec<Section> {
    if parsed.is_empty() {
        let sections = sections
            .iter()
            .map(|section| Section {
                label: section.label,
                rows: section.rows.iter().map(|hit| hit.row.clone()).collect(),
            })
            .collect();
        return super::filter_sections(sections, filter);
    }
    let mut ranked = Vec::new();
    for section in sections {
        for hit in &section.rows {
            if !filter.matches(hit.row.agent.as_ref()) {
                continue;
            }
            if let Some(score) = hit.fields.score(parsed) {
                // A familiar row rises among matches. A miss stays a miss.
                let score = score.saturating_add(boost(&hit.row));
                ranked.push((score, hit.row.clone()));
            }
        }
    }
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    vec![Section {
        label: None,
        rows: ranked.into_iter().map(|(_, row)| row).collect(),
    }]
}

/// Rank owned rows with the same prepared fields Go To caches.
pub(super) fn rank_sections(sections: Vec<Section>, query: &str) -> Vec<Section> {
    let parsed = PreparedQuery::parse(query);
    if parsed.is_empty() {
        return sections;
    }
    let mut ranked = Vec::new();
    for section in sections {
        for row in section.rows {
            let fields = PreparedFields::from_row(&row);
            if let Some(score) = fields.score(&parsed) {
                ranked.push((score, row));
            }
        }
    }
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    vec![Section {
        label: None,
        rows: ranked.into_iter().map(|(_, row)| row).collect(),
    }]
}

fn char_prefix(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

fn fold_one(text: &str, max_graphemes: usize) -> String {
    let mut fold = Fold::new(max_graphemes);
    fold.push(text);
    fold.finish()
}

struct Fold {
    text: String,
    graphemes: usize,
    max_graphemes: usize,
    max_bytes: usize,
}

impl Fold {
    fn new(max_graphemes: usize) -> Self {
        Self {
            text: String::new(),
            graphemes: 0,
            max_graphemes,
            max_bytes: max_graphemes.saturating_mul(MAX_BYTES_PER_GRAPHEME),
        }
    }

    /// Fold `text` until the grapheme or byte budget is spent. Stops without
    /// reading the rest, and never ends on a partial grapheme.
    fn push(&mut self, text: &str) {
        for grapheme in text.graphemes(true) {
            if self.graphemes >= self.max_graphemes || self.text.len() >= self.max_bytes {
                return;
            }
            let start = self.text.len();
            for ch in grapheme.chars() {
                for lower in ch.to_lowercase() {
                    self.text.push(lower);
                }
            }
            if self.text.len() > self.max_bytes {
                self.text.truncate(start);
                return;
            }
            self.graphemes += 1;
        }
    }

    fn finish(self) -> String {
        self.text
    }
}

fn literal_score(haystack: &str, token: &str) -> Option<i64> {
    let index = haystack.find(token)?;
    let before = haystack[..index].chars().next_back();
    let word_start = before.is_none_or(|ch| ch.is_whitespace() || "/:._-".contains(ch));
    let offset = haystack[..index].chars().count().min(60) as i64;
    Some(if word_start { 200 } else { 120 } - offset)
}

fn subsequence_score(haystack: &str, token: &str) -> Option<i64> {
    let mut chars = haystack.chars();
    let mut gaps = 0;
    for wanted in token.chars() {
        gaps += chars.position(|ch| ch == wanted)? as i64;
    }
    Some((60 - gaps).max(1))
}

#[cfg(test)]
mod tests {
    use super::{
        GotoSearch, MAX_HAYSTACK_GRAPHEMES, MAX_QUERY_CHARS, MAX_TERMS, PreparedFields,
        PreparedQuery, SearchRevision,
    };
    use crate::app::overlays::goto_filter::GotoFilter;
    use crate::app::overlays::{Row, Section, Target};
    use crate::connection::{AgentAttention, AgentInfo, AgentState};
    use unicode_segmentation::UnicodeSegmentation;

    fn row(title: &str) -> Row {
        Row {
            title: title.into(),
            detail: None,
            keywords: String::new(),
            target: Target::Command,
            command: None,
            category: "",
            chord: None,
            agent: None,
            preview: None,
        }
    }

    fn agent(kind: &str, state: AgentState) -> AgentInfo {
        AgentInfo {
            name: "Claude".into(),
            kind: Some(kind.into()),
            state,
            attention: AgentAttention::Normal,
            changed_at_ms: 0,
        }
    }

    fn titles(search: &GotoSearch) -> Vec<String> {
        search
            .ranked()
            .iter()
            .flat_map(|section| section.rows.iter())
            .map(|row| row.title.to_string())
            .collect()
    }

    fn rank(search: &mut GotoSearch, query: &str, filter: GotoFilter) {
        search.ensure_ranked(query, filter, 0, |_| 0);
    }

    #[test]
    fn folded_query_matches_unicode_title_metadata_and_agent_fields() {
        let mut cafe = row("CAFÉ ΑΒ");
        cafe.detail = Some("Box workspace /repo".into());
        cafe.keywords = "agents".into();
        cafe.category = "Agents";
        cafe.agent = Some(agent("claude", AgentState::Working));
        let fields = PreparedFields::from_row(&cafe);
        let query = PreparedQuery::parse("café αβ box claude");
        assert!(fields.score(&query).is_some());
        assert!(
            PreparedFields::from_row(&row("İstanbul"))
                .score(&PreparedQuery::parse("i\u{307}stanbul"))
                .is_some()
        );
        // One token cannot jump from the title into the metadata.
        let mut split = row("ab");
        split.keywords = "cd".into();
        assert!(
            PreparedFields::from_row(&split)
                .score(&PreparedQuery::parse("abcd"))
                .is_none()
        );
    }

    #[test]
    fn query_and_haystack_stop_on_character_edges() {
        let tail = "zzznomatch";
        let title = format!("{}{tail}", "é".repeat(MAX_HAYSTACK_GRAPHEMES));
        let fields = PreparedFields::from_row(&row(&title));
        assert_eq!(fields.title.graphemes(true).count(), MAX_HAYSTACK_GRAPHEMES);
        assert!(fields.title.is_char_boundary(fields.title.len()));
        assert!(fields.title.ends_with('é'));
        assert!(fields.score(&PreparedQuery::parse(tail)).is_none());
        assert!(fields.metadata.len() <= MAX_HAYSTACK_GRAPHEMES * 32);
        assert!(fields.title.len() <= MAX_HAYSTACK_GRAPHEMES * 32);

        let query = format!("{} {tail}", "a".repeat(MAX_QUERY_CHARS));
        let parsed = PreparedQuery::parse(&query);
        assert!(parsed.folded.is_char_boundary(parsed.folded.len()));
        assert!(!parsed.folded.contains(tail));
        assert!(parsed.terms.iter().all(|term| !term.contains(tail)));

        let kept: Vec<_> = (0..MAX_TERMS).map(|index| format!("t{index}")).collect();
        let long = format!("{} missing", kept.join(" "));
        assert_eq!(PreparedQuery::parse(&long).terms.len(), MAX_TERMS);
        let haystack = kept.join(" ");
        assert!(
            PreparedFields::from_row(&row(&haystack))
                .score(&PreparedQuery::parse(&long))
                .is_some()
        );
        assert!(
            PreparedFields::from_row(&row("missing"))
                .score(&PreparedQuery::parse(&long))
                .is_none()
        );
    }

    #[test]
    fn metadata_revision_rebuilds_fields_and_a_query_or_chip_does_not() {
        let rev = SearchRevision {
            connection: 1,
            workspace: 4,
        };
        let mut search = GotoSearch::default();
        let sections = vec![Section {
            label: Some("Agents"),
            rows: vec![
                {
                    let mut working = row("Café worker");
                    working.agent = Some(agent("claude", AgentState::Working));
                    working.target = Target::Pane(phux_protocol::ResourceId::local(1));
                    working
                },
                {
                    let mut idle = row("Idle pane");
                    idle.agent = Some(agent("codex", AgentState::Idle));
                    idle.target = Target::Pane(phux_protocol::ResourceId::local(2));
                    idle
                },
            ],
        }];
        search.rebuild(rev, sections);
        rank(&mut search, "café", GotoFilter::All);
        assert_eq!(titles(&search), vec!["Café worker"]);
        assert!(!search.needs_rebuild(rev));

        // A new query reuses the prepared row. It does not see a different source.
        rank(&mut search, "missing", GotoFilter::All);
        assert!(titles(&search).is_empty());
        rank(&mut search, "claude", GotoFilter::Working);
        assert_eq!(titles(&search), vec!["Café worker"]);
        rank(&mut search, "", GotoFilter::Idle);
        assert_eq!(titles(&search), vec!["Idle pane"]);
        assert!(!search.needs_rebuild(rev));

        let next = SearchRevision {
            connection: 2,
            workspace: 4,
        };
        assert!(search.needs_rebuild(next));
        search.rebuild(
            next,
            vec![Section {
                label: Some("Terminals"),
                rows: vec![row("Other")],
            }],
        );
        rank(&mut search, "café", GotoFilter::All);
        assert!(titles(&search).is_empty());
        rank(&mut search, "other", GotoFilter::All);
        assert_eq!(titles(&search), vec!["Other"]);
        assert!(search.needs_rebuild(SearchRevision {
            connection: 2,
            workspace: 5,
        }));
    }

    #[test]
    fn frecency_lifts_a_match_and_does_not_invent_one_or_reorder_an_empty_query() {
        let mut search = GotoSearch::default();
        let rev = SearchRevision {
            connection: 1,
            workspace: 1,
        };
        let row_for = |title: &str, id: u32| {
            let mut row = row(title);
            row.target = Target::Pane(phux_protocol::ResourceId::local(id));
            row
        };
        search.rebuild(
            rev,
            vec![Section {
                label: Some("Terminals"),
                rows: vec![
                    row_for("worker pane", 1),
                    row_for("helper pane", 2),
                    row_for("unrelated", 3),
                ],
            }],
        );
        let boost = |row: &Row| match &row.target {
            Target::Pane(id) if *id == phux_protocol::ResourceId::local(2) => 32,
            Target::Pane(id) if *id == phux_protocol::ResourceId::local(3) => 64,
            _ => 0,
        };
        search.ensure_ranked("pane", GotoFilter::All, 0, |_| 0);
        assert_eq!(titles(&search), vec!["worker pane", "helper pane"]);
        search.ensure_ranked("pane", GotoFilter::All, 1, boost);
        assert_eq!(titles(&search), vec!["helper pane", "worker pane"]);
        search.ensure_ranked("", GotoFilter::All, 1, boost);
        assert_eq!(
            titles(&search),
            vec!["worker pane", "helper pane", "unrelated"]
        );

        search.rebuild(
            rev,
            vec![Section {
                label: None,
                rows: vec![row_for("pane", 1), row_for("helper pane", 2)],
            }],
        );
        search.ensure_ranked("pane", GotoFilter::All, 2, boost);
        assert_eq!(titles(&search), vec!["pane", "helper pane"]);
    }
}
