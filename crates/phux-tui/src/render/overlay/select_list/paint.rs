//! How a [`SelectList`](super::SelectList) row and field look.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratcn::{InputStyle, ListStyle};

use super::{GAP, SelectItem};
use crate::render::{Theme, clip_text};

/// A list's colors: ratcn's, with every backdrop on the modal surface so
/// hover and focus never tint the box; the rows paint their own states.
pub(in crate::render::overlay) fn list_style(theme: &ratcn::Theme) -> ListStyle {
    let mut style = ListStyle::from_theme(theme);
    style.foreground = theme.foreground;
    style.background = theme.surface;
    style.focused_background = theme.surface;
    style.hovered_background = theme.surface;
    style.disabled_background = theme.surface;
    // Headers and separators paint their own text; blank cells stay ink.
    style.disabled_foreground = theme.foreground;
    style
}

/// The query field's colors: ratcn's, on the modal surface whatever its
/// focus or hover, so the caret alone marks the field (a shifted well
/// would degrade to a reversed row at 16 colors and below).
pub(super) fn input_style(theme: &ratcn::Theme) -> InputStyle {
    let mut style = InputStyle::from_theme(theme);
    style.background = theme.surface;
    style.focused_background = theme.surface;
    style.hovered_background = theme.surface;
    // The accent caret every phux field has, legible at every depth.
    style.cursor_foreground = theme.surface;
    style.cursor_background = theme.accent;
    style
}

/// A dim, non-selectable section-header row.
pub(super) fn header_line(theme: &Theme, item: &SelectItem) -> Line<'static> {
    Line::from(Span::styled(
        item.label.clone(),
        Style::default().fg(theme.section_header),
    ))
}

/// One row laid out to exactly `inner_width` cells: label left, dimmed
/// secondary right, the query's `hits` in the label highlighted. Under
/// pressure the secondary yields first, entirely if need be; the label is
/// the row's identity.
pub(super) fn item_line(
    theme: &Theme,
    item: &SelectItem,
    hits: Option<&[usize]>,
    selected: bool,
    inner_width: u16,
) -> Line<'static> {
    let width = inner_width as usize;
    if width == 0 {
        return Line::from(String::new());
    }
    let indent = if item.indented { "  " } else { "" };
    let label_full = format!("{indent}{}", item.label);
    let secondary_full = item.secondary.clone().unwrap_or_default();

    // A short secondary (a chord, at most a third of the row) stays whole
    // and the label clips instead: a cut chord is a wrong chord.
    let secondary_w = crate::render::display_width(&secondary_full);
    let secondary = if secondary_w > 0 && secondary_w + GAP <= width / 3 {
        secondary_full
    } else {
        clip_text(
            &secondary_full,
            width.saturating_sub(crate::render::display_width(&label_full) + GAP),
        )
    };
    let sec_w = crate::render::display_width(&secondary);
    let label = clip_text(
        &label_full,
        width.saturating_sub(sec_w + if sec_w > 0 { GAP } else { 0 }),
    );
    let padding = " ".repeat(width.saturating_sub(crate::render::display_width(&label) + sec_w));

    // The matched letters, offset past the indent, so the user sees why a
    // row is here.
    let hits = hits.map(|hits| {
        hits.iter()
            .map(|pos| pos + indent.chars().count())
            .collect::<Vec<_>>()
    });
    if selected {
        // Own both colors so the selection reads on light terminals too.
        let style = theme.on_selection(Style::default().fg(theme.selection_fg));
        let mut spans = highlight_spans(
            &label,
            hits.as_deref(),
            style,
            style.add_modifier(Modifier::BOLD),
        );
        spans.push(Span::styled(format!("{padding}{secondary}"), style));
        Line::from(spans)
    } else {
        let label_style = if item.attention {
            Style::default()
                .fg(theme.attention)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let hit_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        let mut spans = highlight_spans(&label, hits.as_deref(), label_style, hit_style);
        spans.push(Span::raw(padding));
        spans.push(Span::styled(secondary, Style::default().fg(theme.dim)));
        Line::from(spans)
    }
}

/// `text` as spans: the chars at `hits` (sorted char positions) in `hit`,
/// the rest in `base`. Without hits, one span.
pub(super) fn highlight_spans(
    text: &str,
    hits: Option<&[usize]>,
    base: Style,
    hit: Style,
) -> Vec<Span<'static>> {
    let Some(hits) = hits.filter(|hits| !hits.is_empty()) else {
        return vec![Span::styled(text.to_owned(), base)];
    };
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    for (pos, ch) in text.chars().enumerate() {
        let is_hit = hits.binary_search(&pos).is_ok();
        if is_hit != run_hit && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_hit { hit } else { base },
            ));
        }
        run_hit = is_hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hit { hit } else { base }));
    }
    spans
}

/// The hairline between the query and the rows, joined to the box's sides
/// (`├──┤`), so the field reads as a search bar above its results.
pub(super) fn rule_under_query(buf: &mut Buffer, modal: Rect, theme: &Theme) {
    if modal.width < 2 || modal.height < 4 {
        return;
    }
    let y = modal.y + 2;
    let style = Style::default().fg(theme.border).bg(theme.surface);
    let right = modal.x + modal.width - 1;
    for x in modal.x..=right {
        let glyph = if x == modal.x {
            "├"
        } else if x == right {
            "┤"
        } else {
            "─"
        };
        buf[(x, y)].set_symbol(glyph).set_style(style);
    }
}

/// An accent bar in the padding left of the selected row (visible `row`
/// within `rows`): the selection reads at a glance even on a terminal whose
/// selection fill is faint.
pub(super) fn selection_bar(buf: &mut Buffer, modal: Rect, rows: Rect, row: usize, theme: &Theme) {
    let Ok(row) = u16::try_from(row) else {
        return;
    };
    if row >= rows.height || modal.width < 3 {
        return;
    }
    let style = theme.on_selection(Style::default().fg(theme.accent));
    let y = rows.y + row;
    buf[(modal.x + 1, y)].set_symbol("▌").set_style(style);
    // The fill spans both paddings, so the selection is one even band.
    buf[(modal.x + modal.width - 2, y)]
        .set_symbol(" ")
        .set_style(theme.on_selection(Style::default()));
}
