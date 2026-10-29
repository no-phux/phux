//! Compose the strip into a buffer and emit only the changed rows.

use std::io::{self, Write};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect as RataRect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::layout::Rect;
use crate::render::Theme;
use crate::render::{clip_text, display_width};

use super::agent_filter;
use super::agent_filter::{AgentScope, FilterChip, HostFilter};
use super::layout::{collapse_visible, row_model, text_width};
use super::painter::SidebarPainter;
use super::rows::{SidebarRow, SidebarRule};
use super::{
    AGENTS_EMPTY_LABEL, AGENTS_NONE_LABEL, COLLAPSE_GLYPH, GUTTER, ICON_COLUMNS, NEEDS_YOU_HEADER,
    NEW_LABEL, PLUGIN_EMPTY, SPACES_HEADER, SidebarCounts,
};

impl SidebarPainter {
    /// Render the sections + affordances + separator into a fresh
    /// `rect`-sized buffer, row-for-row from [`row_model`].
    pub(super) fn compose(&self, rect: Rect, rule: SidebarRule, junctions: &[u16]) -> Buffer {
        let area = RataRect::new(0, 0, rect.w, rect.h);
        let mut buf = Buffer::empty(area);
        buf.set_style(
            area,
            Style::default().fg(self.theme.text).bg(self.theme.surface),
        );
        // One-cell gutters protect names from both the edge and separator.
        let text_w = text_width(rect.w);
        let counts = self.counts();
        let model = row_model(counts, rect.h);
        let hidden = hidden_counts(counts, &model);
        if text_w > 0 {
            let lines: Vec<Line<'static>> = model
                .iter()
                .map(|row| self.row_line(*row, hidden, text_w))
                .collect();
            let content_x = rule.content_x();
            Paragraph::new(lines).render(
                RataRect::new(content_x + GUTTER, 0, text_w, rect.h),
                &mut buf,
            );
        }
        self.paint_selection_bed(&mut buf, rect, rule, &model);
        paint_separator(&mut buf, rect, rule, &self.theme, junctions);
        self.paint_window_drop_marker(&mut buf, rect, rule);
        buf
    }

    /// Lay the selection bed under the active window's row, edge to edge
    /// of the strip (gutters included, the rule excluded), so the selected
    /// row is one band rather than a highlight hugging its glyphs.
    fn paint_selection_bed(
        &self,
        buf: &mut Buffer,
        rect: Rect,
        rule: SidebarRule,
        model: &[SidebarRow],
    ) {
        let Some(active) = self.windows.iter().position(|w| w.active) else {
            return;
        };
        let Some(y) = model
            .iter()
            .position(|row| *row == SidebarRow::WindowName(active))
            .and_then(|y| u16::try_from(y).ok())
        else {
            return;
        };
        let rule_x = rule.column(rect.w);
        for x in (0..rect.w).filter(|x| *x != rule_x) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_bg(self.theme.selection_bg);
            }
        }
    }

    /// Reverse the window row under a live drag so the drop slot is
    /// visible before release. Leaves the separator rule alone so the
    /// handle stays a handle.
    fn paint_window_drop_marker(&self, buf: &mut Buffer, rect: Rect, rule: SidebarRule) {
        let Some(index) = self.drop_at else {
            return;
        };
        let model = row_model(self.counts(), rect.h);
        let Some(y) = model
            .iter()
            .position(|row| *row == SidebarRow::WindowName(index))
            .and_then(|y| u16::try_from(y).ok())
        else {
            return;
        };
        if y >= rect.h {
            return;
        }
        let rule_x = rule.column(rect.w);
        for x in 0..rect.w {
            if x == rule_x {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::REVERSED),
                );
            }
        }
    }

    fn row_line(&self, row: SidebarRow, hidden: SidebarCounts, text_w: u16) -> Line<'static> {
        match row {
            SidebarRow::NeedsYouHeader => {
                self.header_line(NEEDS_YOU_HEADER, self.needs_you.len(), text_w)
            }
            SidebarRow::SpacesHeader => self.header_line(SPACES_HEADER, self.roster.len(), text_w),
            SidebarRow::PluginEmpty(_) => self.empty_line(PLUGIN_EMPTY, text_w),
            SidebarRow::NeedsYouEmpty => self.empty_line(
                if self.agent_filter.scope == AgentScope::Attention {
                    AGENTS_EMPTY_LABEL
                } else {
                    AGENTS_NONE_LABEL
                },
                text_w,
            ),
            SidebarRow::AgentFilter => self.agent_filter_line(text_w),
            SidebarRow::WindowName(i) => self
                .windows
                .get(i)
                .map_or_else(|| Line::from(""), |w| self.name_line(w, text_w)),
            SidebarRow::NeedsYou(j) => self
                .needs_you
                .get(j)
                .map_or_else(|| Line::from(""), |e| self.agent_line(e, text_w)),
            SidebarRow::RosterEntry(j) => self
                .roster
                .get(j)
                .map_or_else(|| Line::from(""), |s| self.roster_line(s, text_w)),
            SidebarRow::RosterHost(j) => self.roster.get(j).map_or_else(
                || Line::from(""),
                |s| {
                    s.group.as_ref().map_or_else(
                        || self.host_line(s, text_w),
                        |group| self.project_line(group, text_w),
                    )
                },
            ),
            SidebarRow::NeedsYouOverflow => self.overflow_line(hidden.needs_you, text_w),
            SidebarRow::RosterOverflow => self.sessions_overflow_line(hidden, text_w),
            SidebarRow::Blank => Line::from(""),
            SidebarRow::NewWindow => self.affordance_line(NEW_LABEL, text_w),
            SidebarRow::PluginHeader(s) => self.plugin.get(s).map_or_else(
                || Line::from(""),
                |section| self.header_line(&section.title, section.entries.len(), text_w),
            ),
            SidebarRow::PluginEntry(s, j) => self
                .plugin
                .get(s)
                .and_then(|section| section.entries.get(j))
                .map_or_else(
                    || Line::from(""),
                    |row| self.plugin_row_line(&row.text, text_w),
                ),
            SidebarRow::PluginOverflow(s) => {
                let hidden = self.plugin.get(s).map_or(0, |section| {
                    section
                        .entries
                        .len()
                        .saturating_sub(usize::from(section.rows).saturating_sub(1))
                });
                self.overflow_line(hidden, text_w)
            }
        }
    }

    /// The Agents filter row: each scope chip, the active one in the focus
    /// register on the selection bed, then the `@` machine chip when the
    /// agents span machines. Laid from [`agent_filter::filter_chips`], the
    /// same layout the hit test reads.
    fn agent_filter_line(&self, text_w: u16) -> Line<'static> {
        let chips = agent_filter::filter_chips(text_w, self.counts().agent_host_chip);
        let active = Style::default()
            .fg(self.theme.accent)
            .bg(self.theme.selection_bg)
            .add_modifier(Modifier::BOLD);
        let idle = Style::default().fg(self.theme.dim);
        let mut spans = Vec::with_capacity(chips.len() * 2);
        let mut x = 0u16;
        for (start, w, chip) in chips {
            if start > x {
                spans.push(Span::raw(" ".repeat(usize::from(start - x))));
            }
            let (label, on) = match chip {
                FilterChip::Scope(scope) => {
                    (scope.label().to_owned(), self.agent_filter.scope == scope)
                }
                FilterChip::Host => (
                    self.agent_filter.host.label(),
                    self.agent_filter.host != HostFilter::Any,
                ),
            };
            let label = clip_text(&label, usize::from(w));
            x = start + u16::try_from(display_width(&label)).unwrap_or(w);
            spans.push(Span::styled(label, if on { active } else { idle }));
        }
        Line::from(spans)
    }

    /// One plugin section row: a quiet bullet, then the rendered template.
    fn plugin_row_line(&self, text: &str, text_w: u16) -> Line<'static> {
        let label = clip_text(text, usize::from(text_w).saturating_sub(ICON_COLUMNS));
        Line::from(vec![
            Span::styled("· ", Style::default().fg(self.theme.dim)),
            Span::styled(label, Style::default().fg(self.theme.text)),
        ])
    }

    pub(super) fn sessions_overflow_line(
        &self,
        hidden: SidebarCounts,
        text_w: u16,
    ) -> Line<'static> {
        let mut parts = Vec::new();
        if hidden.roster > 0 {
            parts.push(format!("+{} sessions", hidden.roster));
        }
        if hidden.windows > 0 {
            parts.push(format!("+{} tabs", hidden.windows));
        }
        self.affordance_line(&parts.join(", "), text_w)
    }
}

fn paint_separator(
    buf: &mut Buffer,
    rect: Rect,
    rule: SidebarRule,
    theme: &Theme,
    junctions: &[u16],
) {
    let sep_x = rule.column(rect.w);
    // The rail runs away from the strip, into the panes: rightward from a
    // trailing rule, leftward from a leading one.
    let tee = match rule {
        SidebarRule::Trailing => "├",
        SidebarRule::Leading => "┤",
    };
    for y in 0..rect.h {
        if let Some(cell) = buf.cell_mut((sep_x, y)) {
            cell.set_symbol(if junctions.contains(&y) { tee } else { "│" });
            cell.set_style(Style::default().fg(theme.border));
        }
    }
    // The collapse chevron claims the rule's bottom cell
    // whenever the footer renders (same condition as `hit_test`).
    if collapse_visible(rect)
        && let Some(cell) = buf.cell_mut((sep_x, rect.h - 1))
    {
        cell.set_symbol(COLLAPSE_GLYPH);
        cell.set_style(Style::default().fg(theme.dim));
    }
}

/// Count actual item rows, never host lines or padding, for honest overflow.
pub(super) fn hidden_counts(counts: SidebarCounts, model: &[SidebarRow]) -> SidebarCounts {
    let mut hidden = counts;
    if counts.active_session.is_none_or(|i| i >= counts.roster) {
        hidden.windows = 0;
    }
    for row in model {
        match row {
            SidebarRow::NeedsYou(_) => hidden.needs_you = hidden.needs_you.saturating_sub(1),
            SidebarRow::RosterEntry(_) => hidden.roster = hidden.roster.saturating_sub(1),
            SidebarRow::WindowName(_) => hidden.windows = hidden.windows.saturating_sub(1),
            _ => {}
        }
    }
    hidden
}

/// Lay `left` flush left and `right` flush right on one row. Every row shares
/// this right edge; `right` is dropped whole rather than clipped when it
/// would touch `left`.
pub(super) fn justify(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    text_w: u16,
) -> Line<'static> {
    let width = |spans: &[Span<'_>]| -> usize {
        spans.iter().map(|span| display_width(&span.content)).sum()
    };
    let (lw, rw) = (width(&left), width(&right));
    let w = usize::from(text_w);
    let mut spans = left;
    if rw > 0 && lw + 1 + rw <= w {
        spans.push(Span::raw(" ".repeat(w - lw - rw)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// Emit `buf` at `rect`'s origin with per-cell SGR deltas, repainting whole
/// changed rows (clearing a shortened label's tail). Each row is one run
/// from its own CUP, so the cells spilled into by a wide glyph are skipped:
/// writing them advanced the cursor past the strip into the panes.
pub(super) fn emit_changed<W: Write>(
    out: &mut W,
    buf: &Buffer,
    rect: Rect,
    previous: Option<&Buffer>,
) -> io::Result<()> {
    let mut changed = false;
    for row in 0..rect.h {
        if previous.is_some_and(|old| (0..rect.w).all(|col| old[(col, row)] == buf[(col, row)])) {
            continue;
        }
        changed = true;
        write!(out, "\x1b[{};{}H\x1b[0m", rect.y + row + 1, rect.x + 1)?;
        let mut prev_styled = None;
        let mut col = 0;
        while col < rect.w {
            let cell = &buf[(col, row)];
            crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
            let sym = cell.symbol();
            let advance = if sym.is_empty() {
                out.write_all(b" ")?;
                1
            } else {
                out.write_all(sym.as_bytes())?;
                // Clamp: a symbol the strip believes is zero-width would
                // otherwise spin this loop forever.
                u16::try_from(crate::render::display_width(sym))
                    .unwrap_or(1)
                    .max(1)
            };
            col = col.saturating_add(advance);
        }
        out.write_all(b"\x1b[0m")?;
    }
    if changed { out.flush() } else { Ok(()) }
}
