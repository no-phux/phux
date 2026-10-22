//! Cell-level helpers: config styles and colours as ratatui styles, the
//! row buffer, VT emission, and the right-edge chip and notice overlays.

use std::io::{self, Write};

use std::str::FromStr;

use phux_config::widget::{Cell as WidgetCell, CellHit, CellStyle};
use ratatui::buffer::{Buffer, Cell as RatatuiCell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::render::theme::Theme;

use super::Notice;

/// A config [`CellStyle`] as a ratatui [`Style`]; an unparseable color or an
/// unknown `theme.<slot>` degrades to the terminal default with a warning.
pub(super) fn to_ratatui_style(style: &CellStyle, theme: &Theme) -> Style {
    let mut s = Style::default();
    if let Some(fg) = parse_color(style.fg.as_deref(), theme) {
        s = s.fg(fg);
    }
    if let Some(bg) = parse_color(style.bg.as_deref(), theme) {
        s = s.bg(bg);
    }
    let mut m = Modifier::empty();
    m.set(Modifier::BOLD, style.bold);
    m.set(Modifier::DIM, style.dim);
    m.set(Modifier::ITALIC, style.italic);
    m.set(Modifier::UNDERLINED, style.underline);
    m.set(Modifier::REVERSED, style.reverse);
    s.add_modifier(m)
}

/// Parse a color string (`"red"`, `"#cdd6f4"`, `"12"`, or a theme slot as
/// `"theme.accent"`) into a ratatui [`Color`]. `None`/unparseable/unknown
/// slot ⇒ `None` (terminal default).
pub(super) fn parse_color(spec: Option<&str>, theme: &Theme) -> Option<Color> {
    let s = spec?;
    if let Some(slot) = s.strip_prefix("theme.") {
        let color = theme.slot(slot);
        if color.is_none() {
            tracing::warn!(
                color = s,
                "unknown theme slot in status-bar color; using default"
            );
        }
        return color;
    }
    Color::from_str(s).map_or_else(
        |_| {
            tracing::warn!(color = s, "unrecognized status-bar color; using default");
            None
        },
        Some,
    )
}

/// The persistent error strip's style: bold selection ink on the theme's
/// `error` slot, so the diagnostic reads as an alarm strip rather than
/// blending into normal chrome.
pub(super) fn alarm_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.selection_fg)
        .bg(theme.error)
        .add_modifier(Modifier::BOLD)
}

/// `message` in a fresh `cols`-wide row buffer, styled `style`, truncated
/// and space-padded to the full width. Shared by the error line and the
/// notice toast, live and in the snapshot compose.
pub(super) fn full_row_buffer(message: &str, style: Style, cols: u16) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
    let mut tmp = [0u8; 4];
    let mut col: u16 = 0;
    for ch in message.chars() {
        // Refuses control characters and explicit bidi overrides: a
        // notice carries a config-reload error, which quotes the user's
        // own file, and the strip is emitted as one uninterrupted run.
        let Some(width) = crate::render::cell_width(ch) else {
            continue;
        };
        let width = u16::try_from(width).unwrap_or(1);
        if width == 0 {
            // A combining mark belongs to the cell before it.
            if col > 0 {
                let cell = &mut buffer[(col - 1, 0)];
                let joined = format!("{}{}", cell.symbol(), ch);
                cell.set_symbol(&joined);
            }
            continue;
        }
        // A glyph that would straddle the last column is dropped whole, or
        // the row would emit `cols + 1` columns and wrap into the panes.
        if col.saturating_add(width) > cols {
            break;
        }
        let cell = &mut buffer[(col, 0)];
        cell.set_symbol(ch.encode_utf8(&mut tmp));
        cell.set_style(style);
        // The columns a wide glyph claims are styled but never emitted:
        // `write_buffer` skips them, having already advanced past them.
        for claimed in 1..width {
            let cell = &mut buffer[(col + claimed, 0)];
            cell.set_symbol(" ");
            cell.set_style(style);
        }
        col = col.saturating_add(width);
    }
    while col < cols {
        let cell = &mut buffer[(col, 0)];
        cell.set_symbol(" ");
        cell.set_style(style);
        col = col.saturating_add(1);
    }
    buffer
}

/// Reverse-video + underline the tab under a live drag so the drop slot
/// is visible before release. Hit stamps stay put, so a release still
/// resolves against the same cells.
pub(super) fn mark_window_drop(row: &mut [WidgetCell], drop_at: Option<usize>) {
    let Some(index) = drop_at else {
        return;
    };
    for cell in row {
        if cell.hit == Some(CellHit::Window(index)) {
            let mut style = cell.style.clone().unwrap_or_default();
            style.reverse = true;
            style.underline = true;
            cell.style = Some(style);
        }
    }
}

/// Copy a composed widget row into a ratatui [`Buffer`]. Cells a widget
/// left without a background take `fill` (`Reset` shows the host's), so the
/// bar reads as one surface with the sidebar.
pub(super) fn fill_buffer(
    buffer: &mut Buffer,
    row: &[WidgetCell],
    cols: u16,
    fill: Color,
    theme: &Theme,
) {
    if fill != Color::Reset {
        buffer.set_style(Rect::new(0, 0, cols, 1), Style::default().bg(fill));
    }
    let mut tmp = [0u8; 4];
    for (col, cell) in row.iter().enumerate().take(usize::from(cols)) {
        // `col < cols (u16)` from the `.take(usize::from(cols))` bound, so
        // the narrowing back to `u16` is provably lossless.
        let Ok(x) = u16::try_from(col) else {
            break;
        };
        let target: &mut RatatuiCell = &mut buffer[(x, 0)];
        if cell.text.is_empty() {
            target.set_symbol(" ");
        } else {
            let mut s = String::with_capacity(cell.text.len());
            for ch in &cell.text {
                s.push_str(ch.encode_utf8(&mut tmp));
            }
            target.set_symbol(&s);
        }
        // Carry per-cell style (fg/bg/attrs) across the
        // ratatui boundary; `write_buffer` emits it as SGR.
        if let Some(style) = &cell.style {
            target.set_style(to_ratatui_style(style, theme));
        }
    }
}

/// Emit row 0 of `buffer` at (`row_index`, `x`) as raw VT: CUP, SGR reset,
/// per-cell symbols, SGR reset. The cursor is left for the caller.
pub(super) fn write_buffer<W: Write>(
    out: &mut W,
    buffer: &Buffer,
    row_index: u16,
    x: u16,
    cols: u16,
) -> io::Result<()> {
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = x.saturating_add(1);
    // No `?25l` here: a hide without a guaranteed show once stranded the
    // cursor invisible when the caller had nothing to restore.
    write!(out, "\x1b[{one_based_row};{one_based_col}H\x1b[0m")?;
    let mut prev_styled = None;
    let mut x = 0;
    while x < cols {
        let cell = &buffer[(x, 0)];
        // Per-cell SGR (shared with the overlay painter).
        crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
        let sym = cell.symbol();
        // One run from one CUP must advance exactly `cols` columns, so the
        // cell a wide glyph claims is skipped rather than written.
        let advance = if sym.is_empty() {
            out.write_all(b" ")?;
            1
        } else {
            out.write_all(sym.as_bytes())?;
            u16::try_from(crate::render::display_width(sym))
                .unwrap_or(1)
                .max(1)
        };
        x = x.saturating_add(advance);
    }
    // SGR reset on exit so the next paint inherits no attributes from us.
    out.write_all(b"\x1b[0m")?;
    out.flush()
}

/// ADR-0033: the supervisory badge's columns, right-aligned `right_offset`
/// cells in from the bar's own right edge (ASCII-only, so chars are cells).
pub(super) fn badge_span(badge: &str, cols: u16, right_offset: u16) -> std::ops::Range<u16> {
    let end = cols.saturating_sub(right_offset);
    let width = u16::try_from(badge.chars().count())
        .unwrap_or(u16::MAX)
        .min(end);
    end - width..end
}

/// Emit the attention chip just left of the supervisory badge, reverse and
/// bold in the theme's `attention` color.
pub(super) fn paint_attention_overlay<W: Write>(
    out: &mut W,
    hint: &str,
    row_index: u16,
    x: u16,
    cols: u16,
    right_offset: u16,
    color: Color,
) -> io::Result<()> {
    let avail = cols.saturating_sub(right_offset);
    if avail == 0 || hint.is_empty() {
        return Ok(());
    }
    let visible: String = hint.chars().take(avail as usize).collect();
    let start_col = x.saturating_add(badge_span(hint, cols, right_offset).start);
    let one_based_row = row_index.saturating_add(1);
    let one_based_col = start_col.saturating_add(1);
    write!(out, "\x1b[{one_based_row};{one_based_col}H\x1b[7;1m")?;
    crate::render::sgr::write_sgr_color(out, color, true)?;
    write!(out, "{visible}\x1b[0m")?;
    out.flush()
}

/// Paint a transient notice as a compact right-aligned toast over the bar.
/// The one-cell padding gives the reversed region a chip boundary without
/// adding punctuation to the notice itself.
pub(super) fn paint_notice_overlay<W: Write>(
    out: &mut W,
    notice: &Notice,
    row_index: u16,
    x: u16,
    cols: u16,
    theme: &Theme,
) -> io::Result<()> {
    let Some((buffer, start, width)) = notice_buffer(notice, cols, theme) else {
        return Ok(());
    };
    write_buffer(out, &buffer, row_index, x.saturating_add(start), width)
}

/// Build the clipped cells shared by the live toast and rendered snapshots.
pub(super) fn notice_buffer(
    notice: &Notice,
    cols: u16,
    theme: &Theme,
) -> Option<(Buffer, u16, u16)> {
    let label = format!(" {} ", notice.text);
    let span = notice_span(notice, cols)?;
    let width = span.end - span.start;
    Some((
        full_row_buffer(&label, notice.severity.style(theme), width),
        span.start,
        width,
    ))
}

/// Columns occupied by the right-aligned toast, including its padding.
pub(super) fn notice_span(notice: &Notice, cols: u16) -> Option<std::ops::Range<u16>> {
    let width = u16::try_from(crate::render::display_width(&format!(" {} ", notice.text)))
        .unwrap_or(u16::MAX)
        .min(cols);
    (width > 0).then(|| cols - width..cols)
}

/// Copy the transient toast into the status snapshot's already-composed row.
pub(super) fn overlay_notice_into_buffer(
    buffer: &mut Buffer,
    notice: &Notice,
    cols: u16,
    theme: &Theme,
) {
    let Some((toast, start, width)) = notice_buffer(notice, cols, theme) else {
        return;
    };
    for offset in 0..width {
        buffer[(start + offset, 0)] = toast[(offset, 0)].clone();
    }
}

/// ADR-0033 / phux-foz.1: overlay a badge into a composed bar buffer (the
/// `phux snapshot --rendered` path), right-aligned `right_offset` cells in
/// from the right edge, so the dense-cell snapshot matches the live VT paint.
pub(super) fn overlay_badge_into_buffer(
    buffer: &mut Buffer,
    badge: &str,
    cols: u16,
    right_offset: u16,
    style: Style,
) {
    let avail = cols.saturating_sub(right_offset);
    if avail == 0 || badge.is_empty() {
        return;
    }
    let visible: Vec<char> = badge.chars().take(avail as usize).collect();
    let start = badge_span(badge, cols, right_offset).start;
    let mut tmp = [0u8; 4];
    for (i, ch) in visible.iter().enumerate() {
        let x = start.saturating_add(u16::try_from(i).unwrap_or(0));
        if x >= avail {
            break;
        }
        let cell = &mut buffer[(x, 0)];
        cell.set_symbol(ch.encode_utf8(&mut tmp));
        cell.set_style(style);
    }
}
