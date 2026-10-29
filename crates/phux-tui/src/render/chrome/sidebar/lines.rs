//! One styled line per row kind.

use phux_config::widget::WindowInfo;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::chrome::{AGENT_BLOCKED_GLYPH as AGENT_BLOCKED, AGENT_DONE_GLYPH as AGENT_DONE};
use crate::render::{clip_text, display_width};

use super::compose::justify;
use super::layout::fitting_branch;
use super::painter::SidebarPainter;
use super::projects::{foreign_member_host, roster_indent};
use super::{AgentEntry, ICON_COLUMNS, SessionRosterEntry};

impl SidebarPainter {
    /// A section header: label left, count on the shared right edge.
    pub(super) fn header_line(&self, label: &str, count: usize, text_w: u16) -> Line<'static> {
        let right = if count == 0 {
            Vec::new()
        } else {
            vec![Span::styled(
                count.to_string(),
                Style::default().fg(self.theme.dim),
            )]
        };
        justify(
            vec![Span::styled(
                clip_text(label, usize::from(text_w)),
                Style::default()
                    .fg(self.theme.sidebar_section)
                    .add_modifier(Modifier::BOLD),
            )],
            right,
            text_w,
        )
    }

    /// Render a section's empty-state placeholder: the label
    /// nested one indent under the header, dim + italic so it reads as a
    /// quiet "nothing here yet" rather than a real, selectable row.
    pub(super) fn empty_line(&self, label: &str, text_w: u16) -> Line<'static> {
        let label = clip_text(label, usize::from(text_w).saturating_sub(2));
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default()
                .fg(self.theme.dim)
                .add_modifier(Modifier::ITALIC),
        ))
    }

    /// One window's row under its session: the agent badge (or a plain dot,
    /// filled when active, attention dot when asking), label, and branch
    /// right-aligned. The active row's bed is laid by [`Self::compose`].
    pub(super) fn name_line(&self, w: &WindowInfo, text_w: u16) -> Line<'static> {
        let (glyph, glyph_style) = self.window_glyph(w);
        let exited = w.exited_marker();
        let exited_w = exited.as_ref().map_or(0, |marker| display_width(marker));
        // Reserve 2 cells for the ` !` attention
        // suffix so a long label can't push it off the strip.
        // Same for the retained-exit ` x` / ` xN` marker.
        let label_w = usize::from(text_w)
            .saturating_sub(4) // nested indent + glyph + space
            .saturating_sub(if w.attention { 2 } else { 0 })
            .saturating_sub(exited_w);
        let label = clip_text(&w.name, label_w);
        let style = if w.active {
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if w.exited.is_some() {
            Style::default().fg(self.theme.dim)
        } else {
            Style::default().fg(self.theme.text)
        };
        let mut left = vec![
            Span::raw("  "),
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(label.clone(), style),
        ];
        // A window holding a pane that asked for a
        // human answer (ADR-0035) gets a themed `!` marker.
        if w.attention {
            left.push(Span::styled(
                " !",
                Style::default()
                    .fg(self.theme.attention)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(marker) = exited {
            left.push(Span::styled(marker, Style::default().fg(self.theme.dim)));
        }
        let used = 4 + display_width(&label) + if w.attention { 2 } else { 0 } + exited_w;
        let right = fitting_branch(w, usize::from(text_w).saturating_sub(used))
            .map(|branch| {
                vec![Span::styled(
                    branch.to_owned(),
                    Style::default().fg(self.theme.dim),
                )]
            })
            .unwrap_or_default();
        justify(left, right, text_w)
    }

    /// A window row's glyph and its style.
    fn window_glyph(&self, w: &WindowInfo) -> (String, Style) {
        if w.attention {
            return ("●".to_owned(), Style::default().fg(self.theme.attention));
        }
        if let Some(badge) = &w.badge {
            let mut style = Style::default().fg(badge
                .style
                .fg
                .as_deref()
                .and_then(|fg| fg.parse().ok())
                .unwrap_or(self.theme.dim));
            if badge.style.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            return (badge.glyph.clone(), style);
        }
        if w.active {
            ("●".to_owned(), Style::default().fg(self.theme.accent))
        } else {
            ("○".to_owned(), Style::default().fg(self.theme.dim))
        }
    }

    /// A machine segment header (ADR-0140): the host label in the chord
    /// tone, with `here` flush right on the machine this terminal is
    /// attached to. A machine that is down says so once, on its placeholder
    /// row. Host identity comes from the projection and is never inferred
    /// here.
    pub(super) fn host_line(&self, s: &SessionRosterEntry, text_w: u16) -> Line<'static> {
        let here = s.switch_host.is_none() && !s.satellite;
        let tag = if here { "here" } else { "" };
        let label_budget = usize::from(text_w).saturating_sub(display_width(tag) + 1);
        let label = clip_text(&s.host, label_budget);
        let right = if tag.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(tag, Style::default().fg(self.theme.dim))]
        };
        justify(
            vec![Span::styled(
                label,
                Style::default()
                    .fg(self.theme.chord)
                    .add_modifier(Modifier::BOLD),
            )],
            right,
            text_w,
        )
    }

    /// One agent row: attention-ladder glyph and locator left, name right. An
    /// unseen `done` gets the bold diamond; a seen one relaxes to the idle
    /// ring; `working` gets the half ring.
    pub(super) fn agent_line(&self, e: &AgentEntry, text_w: u16) -> Line<'static> {
        // ONE badge vocabulary for the whole chrome: the same call feeds
        // a pane's own title (`render::chrome::dividers`), so a working
        // agent can never be a `◐` here and something else there.
        let badge = crate::render::chrome::agent_badge(&self.theme, e.state, e.attention, e.seen);
        let host_label = e.host.as_deref().unwrap_or("");
        let host_cols = if host_label.is_empty() {
            0
        } else {
            display_width(host_label).saturating_add(1)
        };
        let avail = usize::from(text_w)
            .saturating_sub(ICON_COLUMNS)
            .saturating_sub(host_cols);
        // A satellite row's locator is already the agent name (there is no
        // attachable session to name). Painting that name again as the
        // suffix would repeat it. The glyph still carries the live state.
        let name = if e.host.is_some() && e.session.as_deref() == Some(e.name.as_str()) {
            ""
        } else {
            e.name.as_str()
        };
        // A cross-session row is labelled by its SESSION, not its window: the
        // row's job is to say where in the fleet to go, and a window name
        // out of its session's context ("edit") locates nothing.
        let locator = e.session.as_ref().unwrap_or(&e.window_name);
        // The destination earns at least half the row; the name gives way.
        let loc_budget = avail.saturating_sub(display_width(name) + 1).max(avail / 2);
        let locator = clip_text(locator, loc_budget);
        let name = clip_text(
            name,
            avail
                .saturating_sub(display_width(&locator))
                .saturating_sub(1),
        );
        let mut glyph_style = Style::default().fg(badge.color);
        if badge.emphatic {
            glyph_style = glyph_style.add_modifier(Modifier::BOLD);
        }
        let mut left = vec![Span::styled(format!("{} ", badge.glyph), glyph_style)];
        if let Some(host) = &e.host {
            left.push(Span::styled(
                format!("{host} "),
                Style::default().fg(self.theme.chord),
            ));
        }
        left.push(Span::styled(locator, Style::default().fg(self.theme.text)));
        let right = if name.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(name, Style::default().fg(self.theme.dim))]
        };
        justify(left, right, text_w)
    }

    /// One roster line: the session's dot, name, and a histogram of the
    /// panes that need a human (`●1 ◆2`) in the same glyphs and colours as
    /// agent badges. A satellite session paints a dim dot and no counts: its
    /// metadata is not subscribable here (`docs/protocol.md#commands-events-resources`).
    pub(super) fn roster_line(&self, s: &SessionRosterEntry, text_w: u16) -> Line<'static> {
        let (dot, color) = self.roster_badge(s);
        let counts = self.roster_histogram(s);
        let counts_w: usize = counts.iter().map(|span| display_width(&span.content)).sum();
        let indent = roster_indent(s);
        let avail = usize::from(text_w).saturating_sub(2 + display_width(indent));
        // Identity keeps at least half the row even under a large histogram.
        let name_budget = avail
            .saturating_sub(counts_w + usize::from(counts_w > 0))
            .max(avail / 2);
        let name = clip_text(&s.name, name_budget);
        let style = if s.active {
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if s.selectable {
            Style::default().fg(self.theme.text)
        } else {
            Style::default().fg(self.theme.dim)
        };
        let mut left = vec![
            Span::styled(indent, Style::default().fg(self.theme.dim)),
            Span::styled(format!("{dot} "), Style::default().fg(color)),
            Span::styled(name, style),
        ];
        if let Some(host) = foreign_member_host(s) {
            let used: usize = left.iter().map(|span| display_width(&span.content)).sum();
            let room = usize::from(text_w).saturating_sub(used + counts_w + 1);
            if room > 2 {
                left.push(Span::styled(
                    clip_text(&format!(" @{host}"), room),
                    Style::default().fg(self.theme.chord),
                ));
            }
        }
        justify(left, counts, text_w)
    }

    /// The counts of the rungs that need a human, each in its own glyph and
    /// colour. Working and settled panes are quiet and not counted.
    fn roster_histogram(&self, s: &SessionRosterEntry) -> Vec<Span<'static>> {
        if s.satellite {
            return Vec::new();
        }
        let rungs = [
            (AGENT_BLOCKED, s.blocked, self.theme.agent_blocked),
            (AGENT_DONE, s.done_unvisited, self.theme.agent_done),
        ];
        let mut spans = Vec::new();
        for (glyph, n, color) in rungs.into_iter().filter(|(_, n, _)| *n > 0) {
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("{glyph}{n}"),
                Style::default().fg(color),
            ));
        }
        spans
    }

    const fn roster_badge(&self, s: &SessionRosterEntry) -> (&'static str, ratatui::style::Color) {
        if s.satellite {
            ("○", self.theme.dim)
        } else {
            match s.top_rank() {
                4 => (AGENT_BLOCKED, self.theme.agent_blocked),
                3 => (AGENT_DONE, self.theme.agent_done),
                // Working is quiet: the dot only lights for what needs you.
                1 | 2 => ("○", self.theme.agent_idle),
                _ => ("○", self.theme.dim),
            }
        }
    }

    /// Render the `+N more` overflow row: dim and indented
    /// like an empty state, because it is chrome rather than a target you
    /// aim at — though clicking it does open the fleet dashboard.
    pub(super) fn overflow_line(&self, hidden: usize, text_w: u16) -> Line<'static> {
        let label = clip_text(
            &format!("+{hidden} more"),
            usize::from(text_w).saturating_sub(2),
        );
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default().fg(self.theme.dim),
        ))
    }

    /// A footer affordance row: the action glyph in the section register,
    /// the word in `dim`.
    pub(super) fn affordance_line(&self, label: &str, text_w: u16) -> Line<'static> {
        let label = clip_text(label, usize::from(text_w));
        Line::from(self.affordance_spans(&label))
    }

    fn affordance_spans(&self, label: &str) -> Vec<Span<'static>> {
        let mut chars = label.chars();
        let glyph = chars.next().map(String::from).unwrap_or_default();
        let rest = chars.as_str().to_owned();
        vec![
            Span::styled(glyph, Style::default().fg(self.theme.chord)),
            Span::styled(rest, Style::default().fg(self.theme.dim)),
        ]
    }
}
