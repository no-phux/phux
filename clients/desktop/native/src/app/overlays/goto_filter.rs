//! Go To's agent-state filter.
//!
//! Each choice is one shared [`AgentInfo`] predicate. A terminal or tab with no
//! agent stays listed only under All. Changing the filter does not rewrite the
//! query; the highlight follows the same entry when it is still listed.

use crate::connection::AgentInfo;
use phux_protocol::ResourceId;

/// The Go To filter choices, in menu order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum GotoFilter {
    #[default]
    All,
    NeedsYou,
    Working,
    Idle,
    Finished,
}

impl GotoFilter {
    pub(crate) const ALL: [Self; 5] = [
        Self::All,
        Self::NeedsYou,
        Self::Working,
        Self::Idle,
        Self::Finished,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Idle => "Idle",
            Self::Finished => "Finished",
        }
    }

    /// Whether a row stays listed. `None` is a terminal or tab with no agent.
    pub(crate) fn matches(self, agent: Option<&AgentInfo>) -> bool {
        match self {
            Self::All => true,
            Self::NeedsYou => agent.is_some_and(AgentInfo::needs_you),
            Self::Working => agent.is_some_and(AgentInfo::working),
            Self::Idle => agent.is_some_and(AgentInfo::idle),
            Self::Finished => agent.is_some_and(AgentInfo::finished),
        }
    }
}

/// Stable Go To row. The highlight follows this, not the row index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GotoId {
    Work(crate::app::fleet::Destination),
    Command(&'static str),
    Setting(crate::app::settings_panel::Page),
    Pane(ResourceId),
    Tab(String),
}

/// One palette candidate before a chip hides it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GotoCandidate {
    pub(crate) id: GotoId,
    pub(crate) agent: Option<AgentInfo>,
}

/// Query plus the entry a filter change should keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GotoCursor {
    pub(crate) query: String,
    /// `None` means that entry was filtered out: show the first visible row.
    pub(crate) selected: Option<GotoId>,
}

/// Filter state for the Go To palette.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GotoPalette {
    pub(crate) filter: GotoFilter,
    pub(crate) selected: Option<GotoId>,
    /// The next render moves the highlight to [`Self::selected`], or the first
    /// visible row when that entry was filtered out.
    pub(crate) align: bool,
}

pub(crate) fn visible_ids(entries: &[GotoCandidate], filter: GotoFilter) -> Vec<GotoId> {
    entries
        .iter()
        .filter(|entry| filter.matches(entry.agent.as_ref()))
        .map(|entry| entry.id.clone())
        .collect()
}

/// Keep `cursor.query`. Keep `cursor.selected` when that entry still matches
/// `filter`; otherwise clear it so the caller selects the first visible row.
pub(crate) fn apply_filter(cursor: &mut GotoCursor, entries: &[GotoCandidate], filter: GotoFilter) {
    let visible = visible_ids(entries, filter);
    cursor.selected = cursor
        .selected
        .as_ref()
        .filter(|selected| visible.contains(*selected))
        .cloned();
}

/// Section and row of `selected`, or the first visible row when `selected` is
/// absent or no longer listed. `None` when nothing is visible.
pub(crate) fn selection_path(
    sections: &[Vec<GotoId>],
    selected: Option<&GotoId>,
) -> Option<(usize, usize)> {
    if let Some(selected) = selected {
        for (section, rows) in sections.iter().enumerate() {
            if let Some(row) = rows.iter().position(|id| id == selected) {
                return Some((section, row));
            }
        }
    }
    sections
        .iter()
        .enumerate()
        .find_map(|(section, rows)| (!rows.is_empty()).then_some((section, 0)))
}

#[cfg(test)]
mod tests {
    use super::{
        GotoCandidate, GotoCursor, GotoFilter, GotoId, apply_filter, selection_path, visible_ids,
    };
    use crate::connection::{AgentAttention, AgentInfo, AgentState};
    use phux_protocol::ResourceId;

    fn pane(byte: u8) -> GotoId {
        GotoId::Pane(ResourceId::local(byte.into()))
    }

    fn candidate(byte: u8, state: AgentState, attention: AgentAttention) -> GotoCandidate {
        GotoCandidate {
            id: pane(byte),
            agent: Some(AgentInfo {
                name: "agent".into(),
                kind: None,
                state,
                attention,
                changed_at_ms: 0,
            }),
        }
    }

    fn sample() -> Vec<GotoCandidate> {
        vec![
            candidate(1, AgentState::Blocked, AgentAttention::High),
            candidate(2, AgentState::Working, AgentAttention::Normal),
            candidate(3, AgentState::Idle, AgentAttention::None),
            candidate(4, AgentState::Done, AgentAttention::Low),
            GotoCandidate {
                id: GotoId::Tab("layout".into()),
                agent: None,
            },
        ]
    }

    #[test]
    fn chips_follow_agent_predicates_and_hide_rows_without_an_agent() {
        let entries = sample();
        let ids = |filter| visible_ids(&entries, filter);
        assert_eq!(
            ids(GotoFilter::All),
            vec![
                pane(1),
                pane(2),
                pane(3),
                pane(4),
                GotoId::Tab("layout".into())
            ]
        );
        assert_eq!(ids(GotoFilter::NeedsYou), vec![pane(1), pane(4)]);
        assert_eq!(ids(GotoFilter::Working), vec![pane(2)]);
        assert_eq!(ids(GotoFilter::Idle), vec![pane(3)]);
        assert_eq!(ids(GotoFilter::Finished), vec![pane(4)]);
        let loud = candidate(9, AgentState::Working, AgentAttention::High);
        assert!(GotoFilter::NeedsYou.matches(loud.agent.as_ref()));
        assert!(GotoFilter::Working.matches(loud.agent.as_ref()));
        for filter in GotoFilter::ALL {
            assert_eq!(filter.matches(None), filter == GotoFilter::All);
        }
    }

    #[test]
    fn changing_filter_keeps_the_query_and_the_same_entry() {
        let entries = sample();
        let mut cursor = GotoCursor {
            query: "worker".into(),
            selected: Some(pane(2)),
        };
        apply_filter(&mut cursor, &entries, GotoFilter::Working);
        assert_eq!(cursor.query, "worker");
        assert_eq!(cursor.selected, Some(pane(2)));
        // Worker was row 1 beside a blocked agent. The filter leaves it at row 0.
        let full = visible_ids(&entries, GotoFilter::All);
        assert_eq!(selection_path(&[full], Some(&pane(2))), Some((0, 1)));
        let visible = visible_ids(&entries, GotoFilter::Working);
        assert_eq!(
            selection_path(&[visible], cursor.selected.as_ref()),
            Some((0, 0))
        );
    }

    #[test]
    fn filtered_out_entry_selects_the_first_visible_row() {
        let entries = sample();
        let mut cursor = GotoCursor {
            query: "stay".into(),
            selected: Some(pane(2)),
        };
        apply_filter(&mut cursor, &entries, GotoFilter::Idle);
        assert_eq!(cursor.query, "stay");
        assert_eq!(cursor.selected, None);
        let visible = visible_ids(&entries, GotoFilter::Idle);
        assert_eq!(
            selection_path(std::slice::from_ref(&visible), None),
            Some((0, 0))
        );
        assert_eq!(visible[0], pane(3));
    }

    #[test]
    fn selection_follows_identity_across_sections_not_the_old_index() {
        let before = vec![vec![pane(1), pane(2)], vec![GotoId::Tab("layout".into())]];
        assert_eq!(selection_path(&before, Some(&pane(2))), Some((0, 1)));
        let after = vec![vec![pane(2)]];
        assert_eq!(selection_path(&after, Some(&pane(2))), Some((0, 0)));
        assert!(after[0].get(1).is_none());
        assert_eq!(selection_path(&[], None), None);
    }
}
