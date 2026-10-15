//! Project groups in the Sessions zone (ADR-0168): the group header row and
//! the member rows' indentation. Grouping itself is the projection's job;
//! this paints what it was handed.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{
    RosterGroup, SessionRosterEntry, SidebarPainter, justify, roster_segment, segment_starts,
};
use crate::render::{clip_text, display_width};

/// Glyph before an expanded group's name.
pub(super) const EXPANDED_GLYPH: &str = "▾";
/// Glyph before a collapsed group's name.
pub(super) const COLLAPSED_GLYPH: &str = "▸";

/// Leading cells of a roster row: none outside a group, one inside, and a
/// branch mark for a linked worktree.
pub(super) const fn roster_indent(s: &SessionRosterEntry) -> &'static str {
    match (&s.group, s.linked) {
        (None, _) => "",
        (Some(_), false) => " ",
        (Some(_), true) => " └",
    }
}

/// The machine a grouped row lives on when that is not the attached server,
/// so a group spanning machines says where each row is.
pub(super) fn foreign_member_host(s: &SessionRosterEntry) -> Option<&str> {
    (s.group.is_some() && (s.switch_host.is_some() || s.satellite)).then_some(s.host.as_str())
}

impl SidebarPainter {
    /// Bits of [`super::SidebarCounts::project_starts`]: segment starts whose
    /// entry is in a group.
    pub(super) fn project_starts(&self) -> u128 {
        let starts = segment_starts(self.roster.iter().map(roster_segment));
        self.roster
            .iter()
            .take(128)
            .enumerate()
            .filter(|(j, s)| s.group.is_some() && starts & (1u128 << j) != 0)
            .fold(0, |mask, (j, _)| mask | (1u128 << j))
    }

    /// A project group header: the collapse glyph and name in the chord
    /// tone, then flush right the panes that need you and, while collapsed,
    /// how many sessions it is hiding.
    pub(super) fn project_line(&self, group: &RosterGroup, text_w: u16) -> Line<'static> {
        let glyph = if group.collapsed {
            COLLAPSED_GLYPH
        } else {
            EXPANDED_GLYPH
        };
        let mut right = Vec::new();
        if group.needs_you > 0 {
            right.push(Span::styled(
                format!(
                    "{}{}",
                    crate::render::chrome::AGENT_BLOCKED_GLYPH,
                    group.needs_you
                ),
                Style::default().fg(self.theme.agent_blocked),
            ));
        }
        if group.collapsed && group.hidden > 0 {
            if !right.is_empty() {
                right.push(Span::raw(" "));
            }
            right.push(Span::styled(
                format!("+{}", group.hidden),
                Style::default().fg(self.theme.dim),
            ));
        }
        let right_w: usize = right.iter().map(|span| display_width(&span.content)).sum();
        let budget = usize::from(text_w)
            .saturating_sub(2 + right_w + usize::from(right_w > 0))
            .max(usize::from(text_w) / 2);
        justify(
            vec![
                Span::styled(format!("{glyph} "), Style::default().fg(self.theme.dim)),
                Span::styled(
                    clip_text(&group.name, budget),
                    Style::default()
                        .fg(self.theme.chord)
                        .add_modifier(Modifier::BOLD),
                ),
            ],
            right,
            text_w,
        )
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::super::{SidebarHit, SidebarRow, hit_test, row_model};
    use super::*;
    use crate::layout::Rect;
    use crate::render::Theme;

    fn entry(name: &str, group: Option<&RosterGroup>, linked: bool) -> SessionRosterEntry {
        SessionRosterEntry {
            name: name.to_owned(),
            host: "mini".to_owned(),
            selectable: true,
            group: group.cloned(),
            linked,
            ..SessionRosterEntry::default()
        }
    }

    fn group(collapsed: bool, needs_you: usize, hidden: usize) -> RosterGroup {
        RosterGroup {
            key: "github.com/no-phux/phux".to_owned(),
            name: "phux".to_owned(),
            collapsed,
            needs_you,
            hidden,
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_group_opens_its_own_segment_and_its_header_toggles_it() {
        let g = group(false, 2, 0);
        let mut painter = SidebarPainter::new(Theme::default());
        painter.set_roster(vec![
            entry("phux", Some(&g), false),
            entry("feat", Some(&g), true),
            entry("scratch", None, false),
        ]);
        let counts = painter.counts();
        assert_eq!(counts.project_starts, 0b001);
        assert_eq!(counts.host_starts, 0b101, "the host segment reopens after");
        let rect = Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 20,
        };
        let model = row_model(counts, rect.h);
        let header = model
            .iter()
            .position(|row| *row == SidebarRow::RosterHost(0))
            .expect("group header row");
        let y = u16::try_from(header).expect("row fits");
        assert_eq!(hit_test(rect, counts, 2, y), Some(SidebarHit::Project(0)));
        let host = model
            .iter()
            .position(|row| *row == SidebarRow::RosterHost(2))
            .expect("host header row");
        let y = u16::try_from(host).expect("row fits");
        assert_eq!(hit_test(rect, counts, 2, y), Some(SidebarHit::Roster(2)));
        let targets = painter.click_targets();
        assert_eq!(targets.projects, [Some(g.key.clone()), Some(g.key), None]);
    }

    #[test]
    fn the_header_shows_the_rollup_and_what_a_collapse_hides() {
        let painter = SidebarPainter::new(Theme::default());
        let open = text(&painter.project_line(&group(false, 2, 0), 20));
        assert!(open.starts_with("▾ phux"), "{open:?}");
        assert!(open.trim_end().ends_with("●2"), "{open:?}");
        let shut = text(&painter.project_line(&group(true, 0, 3), 20));
        assert!(shut.starts_with("▸ phux"), "{shut:?}");
        assert!(shut.trim_end().ends_with("+3"), "{shut:?}");
    }

    #[test]
    fn members_indent_and_name_another_machine() {
        let g = group(false, 0, 0);
        assert_eq!(roster_indent(&entry("a", None, false)), "");
        assert_eq!(roster_indent(&entry("a", Some(&g), false)), " ");
        assert_eq!(roster_indent(&entry("a", Some(&g), true)), " └");
        let remote = SessionRosterEntry {
            switch_host: Some("laptop".to_owned()),
            host: "laptop".to_owned(),
            ..entry("a", Some(&g), false)
        };
        assert_eq!(foreign_member_host(&remote), Some("laptop"));
        assert_eq!(foreign_member_host(&entry("a", Some(&g), false)), None);
        let painter = SidebarPainter::new(Theme::default());
        let line = text(&painter.roster_line(&remote, 24));
        assert!(line.contains("a @laptop"), "{line:?}");
    }
}
