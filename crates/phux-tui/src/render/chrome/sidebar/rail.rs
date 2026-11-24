//! The collapsed rail (`[sidebar] rail`): what the strip keeps while hidden.
//!
//! [`RAIL_WIDTH`] columns: an outer marker column, the glyph column, and the
//! separator rule. Top to bottom: how many agents need you (opens the agent
//! fleet), one dot per session in the colour its Sessions row draws (the
//! active session marked on the outer column), an overflow mark (opens the
//! session picker), and the expand chevron on the last row
//! (`toggle-sidebar`). It speaks the strip's [`SidebarRow`] vocabulary, so a
//! click resolves through the same [`SidebarTargets`](super::SidebarTargets).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect as RataRect;
use ratatui::style::{Modifier, Style};

use crate::layout::Rect;

use super::compose::paint_separator;
use super::layout::{collapse_visible, row_hit};
use super::painter::SidebarPainter;
use super::rows::{SidebarHit, SidebarRow, SidebarRule};
use super::{MIN_FOOTER_HEIGHT, RAIL_WIDTH, SidebarCounts};

/// The chevron on a left-docked rail's bottom corner; it points the way
/// the strip opens. A right-docked rail mirrors it.
pub const EXPAND_GLYPH: &str = "›";
/// The outer-column mark beside the active session's dot.
pub const ACTIVE_MARK: &str = "▌";
/// The mark under the last dot when more sessions exist than rows.
pub const OVERFLOW_MARK: &str = "…";

/// Whether a strip `width` columns wide paints as the rail: too narrow for
/// any row text, so a glyph column is all it can say.
#[must_use]
pub const fn is_rail(width: u16) -> bool {
    width <= RAIL_WIDTH
}

/// The rail's rows: the agents badge and a gap when the Agents panel is on
/// (`agents-share > 0`) and three body rows fit, then session dots. Cut
/// short, the dots end at the active session and a last row marks the rest.
/// The footer row is blank; the whole row is the expand target.
pub(super) fn rail_model(counts: SidebarCounts, h: u16) -> Vec<SidebarRow> {
    let h = usize::from(h);
    let footer = h >= usize::from(MIN_FOOTER_HEIGHT);
    let body = h - usize::from(footer);
    let mut rows = Vec::with_capacity(h);
    if counts.agents_share > 0 && body >= 3 {
        rows.extend([SidebarRow::NeedsYouHeader, SidebarRow::Blank]);
    }
    let room = body - rows.len();
    let overflow = counts.roster > room;
    let shown = if overflow {
        room.saturating_sub(1)
    } else {
        counts.roster
    };
    let first = counts
        .active_session
        .filter(|active| *active < counts.roster)
        .map_or(0, |active| (active + 1).saturating_sub(shown));
    rows.extend((first..first + shown).map(SidebarRow::RosterEntry));
    if overflow && room > 0 {
        rows.push(SidebarRow::RosterOverflow);
    }
    rows.resize(h, SidebarRow::Blank);
    rows
}

/// Resolve a strip-local row of a `h`-row rail. Every column is part of the
/// target (the rail has no resize handle); the footer row expands.
pub(super) fn rail_hit(counts: SidebarCounts, h: u16, local_y: u16) -> Option<SidebarHit> {
    if h >= MIN_FOOTER_HEIGHT && local_y == h - 1 {
        return Some(SidebarHit::Collapse);
    }
    row_hit(*rail_model(counts, h).get(usize::from(local_y))?)
}

impl SidebarPainter {
    /// Compose the rail into a fresh `rect`-sized buffer, row-for-row from
    /// [`rail_model`].
    pub(super) fn compose_rail(&self, rect: Rect, rule: SidebarRule, junctions: &[u16]) -> Buffer {
        let area = RataRect::new(0, 0, rect.w, rect.h);
        let mut buf = Buffer::empty(area);
        buf.set_style(
            area,
            Style::default().fg(self.theme.text).bg(self.theme.surface),
        );
        let rule_x = rule.column(rect.w);
        let (glyph_x, outer_x) = match rule {
            SidebarRule::Trailing => (rule_x.saturating_sub(1), 0),
            SidebarRule::Leading => (1, rect.w.saturating_sub(1)),
        };
        let marks = outer_x != glyph_x && outer_x != rule_x;
        for (y, row) in (0u16..).zip(rail_model(self.counts(), rect.h)) {
            if glyph_x == rule_x {
                break;
            }
            let (glyph, style) = match row {
                SidebarRow::NeedsYouHeader => self.agents_badge(),
                SidebarRow::RosterEntry(j) => {
                    let Some(session) = self.roster.get(j) else {
                        continue;
                    };
                    let (dot, color) = self.roster_dot(session);
                    let mut style = Style::default().fg(color);
                    if session.active {
                        style = style.add_modifier(Modifier::BOLD);
                        if marks {
                            set(&mut buf, outer_x, y, ACTIVE_MARK, self.theme.accent);
                        }
                    }
                    (dot.to_owned(), style)
                }
                SidebarRow::RosterOverflow => (
                    OVERFLOW_MARK.to_owned(),
                    Style::default().fg(self.theme.dim),
                ),
                _ => continue,
            };
            if let Some(cell) = buf.cell_mut((glyph_x, y)) {
                cell.set_symbol(&glyph);
                cell.set_style(style);
            }
        }
        paint_separator(&mut buf, rect, rule, &self.theme, junctions);
        if collapse_visible(rect) {
            let chevron = match rule {
                SidebarRule::Trailing => EXPAND_GLYPH,
                SidebarRule::Leading => super::COLLAPSE_GLYPH,
            };
            set(&mut buf, rule_x, rect.h - 1, chevron, self.theme.dim);
        }
        buf
    }

    /// How many agents need a human, whatever the Agents filter admits: a
    /// digit (`+` past nine) in the blocked tone, or a dim `0`.
    fn agents_badge(&self) -> (String, Style) {
        let n = self.agents.iter().filter(|a| a.needs_human()).count();
        match n {
            0 => ("0".to_owned(), Style::default().fg(self.theme.dim)),
            1..=9 => (
                n.to_string(),
                Style::default()
                    .fg(self.theme.agent_blocked)
                    .add_modifier(Modifier::BOLD),
            ),
            _ => (
                "+".to_owned(),
                Style::default()
                    .fg(self.theme.agent_blocked)
                    .add_modifier(Modifier::BOLD),
            ),
        }
    }
}

fn set(buf: &mut Buffer, x: u16, y: u16, symbol: &str, fg: ratatui::style::Color) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(symbol);
        cell.set_style(Style::default().fg(fg));
    }
}
