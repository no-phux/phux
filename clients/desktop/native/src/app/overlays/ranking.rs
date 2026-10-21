//! Go To filtering and ranking, and the revision that says when its rows
//! must be folded again.

use super::{Row, Section, Target, goto_filter, goto_frecency, goto_search, goto_sections};
use crate::app::desk::Desk;

pub(super) fn filter_sections(
    mut sections: Vec<Section>,
    filter: goto_filter::GotoFilter,
) -> Vec<Section> {
    if filter == goto_filter::GotoFilter::All {
        return sections;
    }
    for section in &mut sections {
        section
            .rows
            .retain(|row| filter.matches(row.agent.as_ref()));
    }
    sections.retain(|section| !section.rows.is_empty());
    sections
}

pub(super) fn goto_id(row: &Row) -> Option<goto_filter::GotoId> {
    match &row.target {
        Target::Work(destination) => Some(goto_filter::GotoId::Work(destination.clone())),
        Target::Command => row.command.map(goto_filter::GotoId::Command),
        Target::Setting(page) => Some(goto_filter::GotoId::Setting(*page)),
        Target::Preference(key, _) => Some(goto_filter::GotoId::Preference(key.clone())),
        Target::Pane(id) => Some(goto_filter::GotoId::Pane(id.clone())),
        Target::Tab(id) => Some(goto_filter::GotoId::Tab(id.clone())),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn goto_candidates(sections: &[Section]) -> Vec<goto_filter::GotoCandidate> {
    sections
        .iter()
        .flat_map(|section| section.rows.iter())
        .filter_map(|row| {
            Some(goto_filter::GotoCandidate {
                id: goto_id(row)?,
                agent: row.agent.clone(),
            })
        })
        .collect()
}

pub(super) fn section_ids(sections: &[Section]) -> Vec<Vec<goto_filter::GotoId>> {
    sections
        .iter()
        .map(|section| section.rows.iter().filter_map(goto_id).collect())
        .collect()
}

/// Rank every row against `query` as one flat list; an empty query keeps the sections.
pub(super) fn rank_sections(sections: Vec<Section>, query: &str) -> Vec<Section> {
    goto_search::rank_sections(sections, query)
}

/// Fold Go To rows when their metadata revision changes, then rank the query.
pub(super) fn prepare_goto(desk: &mut Desk, query: &str) {
    let revision = search_revision(desk);
    let filter = desk.goto.filter;
    if desk.goto_search.needs_rebuild(revision) {
        let sections = goto_sections(desk);
        desk.goto_search.rebuild(revision, sections);
    }
    let generation = desk.goto_frecency.generation();
    if desk.goto_search.rank_current(query, filter, generation) {
        return;
    }
    let boosts = desk.goto_frecency.boosts(goto_frecency::now());
    let boot = desk
        .link
        .as_ref()
        .and_then(|link| link.connection.server_id());
    desk.goto_search
        .ensure_ranked(query, filter, generation, |row| {
            goto_boost(&boosts, boot.as_deref(), row)
        });
}

pub(super) fn goto_boost(
    boosts: &std::collections::HashMap<goto_frecency::Key, i64>,
    boot: Option<&str>,
    row: &Row,
) -> i64 {
    let Some(boot) = boot else {
        return 0;
    };
    let key = match &row.target {
        Target::Pane(id) => goto_frecency::Key::pane(boot, &id.to_string()),
        Target::Tab(id) => goto_frecency::Key::tab(boot, id),
        _ => None,
    };
    key.and_then(|key| boosts.get(&key).copied()).unwrap_or(0)
}

/// Connection drains cover panes and agents. Tabs are local, so they mix in too.
pub(super) fn search_revision(desk: &Desk) -> goto_search::SearchRevision {
    let Some(link) = desk.link.as_ref() else {
        return goto_search::SearchRevision {
            connection: desk.fleet.revision.rotate_left(32),
            workspace: 0,
        };
    };
    goto_search::SearchRevision {
        connection: link
            .connection
            .revision()
            .wrapping_add(desk.fleet.revision.rotate_left(32)),
        workspace: workspace_stamp(&link.workspace),
    }
}

/// Tab rows and the focused terminal follow the active tab, so it mixes in too.
pub(super) fn workspace_stamp(workspace: &crate::workspace::Workspace) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    hash = mix_bytes(hash, workspace.active_id.as_bytes());
    for tab in &workspace.tabs {
        hash = mix_bytes(hash, tab.id.as_bytes());
        hash = mix_bytes(hash, tab.focused_id.as_bytes());
        hash = mix_bytes(hash, tab.title.as_deref().unwrap_or("").as_bytes());
        hash = mix_node(hash, &tab.root);
    }
    hash
}

pub(super) fn mix_node(mut hash: u64, node: &crate::workspace::layout::LayoutNode) -> u64 {
    match node {
        crate::workspace::layout::LayoutNode::Leaf { placement } => {
            hash = mix_bytes(hash, placement.id.as_bytes());
            mix_bytes(hash, placement.terminal_label.as_bytes())
        }
        crate::workspace::layout::LayoutNode::Split {
            id, first, second, ..
        } => {
            hash = mix_bytes(hash, id.as_bytes());
            hash = mix_node(hash, first);
            mix_node(hash, second)
        }
    }
}

pub(super) fn mix_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    hash ^= bytes.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(super) fn query_terms(query: &str) -> Vec<String> {
    goto_search::PreparedQuery::parse(query).into_terms()
}
