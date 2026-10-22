//! Composing and painting the bar: the right-edge chips' geometry, the
//! widget row, VT emission, snapshots, and hit testing.

use std::io::{self, Write};
use std::time::Duration;

use phux_config::widget::Cell as WidgetCell;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::cells::{
    alarm_style, badge_span, fill_buffer, full_row_buffer, mark_window_drop, notice_span,
    overlay_badge_into_buffer, overlay_notice_into_buffer, paint_attention_overlay,
    paint_notice_overlay, write_buffer,
};
use super::{BarInset, ComposePolicy, Position, StatusBarContext, StatusBarPainter};

impl StatusBarPainter {
    /// The right-edge key chip: the prefix chord while it is pending, else
    /// the active key mode's name, padded.
    fn key_chip(&self) -> Option<String> {
        if self.prefix_pending {
            return Some(format!(" {} ", self.prefix));
        }
        self.key_mode.as_ref().map(|mode| format!(" {mode} "))
    }

    /// The fill cell between the widgets and the key chip, so the clock
    /// never runs straight into it; `None` without a chip or room for one.
    pub(super) fn key_gap(&self, cols: u16) -> Option<u16> {
        let chip = self.key_chip()?;
        badge_span(&chip, cols, 0).start.checked_sub(1)
    }

    /// Columns the widgets compose into: all of them, less the key chip and
    /// its gap while one is up, so a right-aligned widget
    /// moves left of the chip whole instead of being cut under it. A bar
    /// too narrow for the chip keeps every column.
    fn widget_cols(&self, cols: u16) -> u16 {
        let Some(chip) = self.key_chip() else {
            return cols;
        };
        let reserve = u16::try_from(chip.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_add(1);
        if reserve >= cols {
            cols
        } else {
            cols - reserve
        }
    }

    /// Cells the attention chip sits in from the right edge: the badge width
    /// plus a gap, or `0` with no badge.
    fn attention_offset(&self) -> u16 {
        self.supervisory.as_ref().map_or(0, |badge| {
            u16::try_from(badge.chars().count())
                .unwrap_or(u16::MAX)
                .saturating_add(1)
        })
    }

    /// The separator is painted, not merely skipped over: widget text must
    /// not leak between two status chips. Same geometry for VT and snapshots.
    pub(super) fn badge_gap(&self, cols: u16) -> Option<u16> {
        let badge = self.supervisory.as_deref().filter(|s| !s.is_empty())?;
        let hint = self.attention.as_deref().filter(|s| !s.is_empty())?;
        let attention = badge_span(hint, cols, self.attention_offset());
        (!attention.is_empty()).then(|| badge_span(badge, cols, 0).start.saturating_sub(1))
    }

    /// True when no widgets are configured (never for the error line).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.error.is_none() && self.bar.is_empty()
    }

    /// The repaint cadence: `Some(1s)` for any non-empty bar (and the error
    /// line, so it survives pane output), `None` otherwise.
    #[must_use]
    pub fn min_poll_interval(&self) -> Option<Duration> {
        if self.is_empty() {
            None
        } else {
            Some(Duration::from_secs(1))
        }
    }

    /// Paint the bar for a `cols × rows` viewport within `inset`; unchanged
    /// output and dims write nothing.
    #[cfg(test)]
    pub fn paint<W: Write>(
        &mut self,
        out: &mut W,
        inset: BarInset,
        cols: u16,
        rows: u16,
        ctx: &StatusBarContext<'_>,
    ) -> io::Result<()> {
        self.paint_outcome(out, inset, cols, rows, ctx, ComposePolicy::Always)
            .map(drop)
    }

    /// Paint the bar and report whether this call emitted the row rather than
    /// taking the unchanged-content cache fast path.
    pub(crate) fn paint_outcome<W: Write>(
        &mut self,
        out: &mut W,
        inset: BarInset,
        cols: u16,
        rows: u16,
        ctx: &StatusBarContext<'_>,
        compose: ComposePolicy,
    ) -> io::Result<bool> {
        if cols == 0 || rows == 0 {
            return Ok(false);
        }
        // The bar yields the sidebar's columns. Everything below
        // composes and hit-tests against this span, not the viewport — an
        // inset wider than the terminal leaves nothing to paint.
        let (x, cols) = inset.span(cols);
        if cols == 0 {
            return Ok(false);
        }
        if let Some(outcome) = self.paint_error_takeover(out, x, cols, rows)? {
            return Ok(outcome);
        }
        // The badge rides the bar, so an empty bar with no windows paints
        // nothing at all.
        if self.bar.is_empty() && self.windows.is_empty() {
            return Ok(false);
        }
        // Pane-output paints never change the bar; skip the widget pipeline.
        if self.cached_compose_stands_in(compose, x, cols, rows) {
            return Ok(false);
        }
        let ctx = self.ctx_with_window_list(ctx);
        phux_client::perf::BAR_COMPOSES.add(1);
        let mut new_row = self.bar.render(&ctx.as_widget(), self.widget_cols(cols));
        mark_window_drop(&mut new_row, self.drop_at);
        if !self.needs_repaint(x, cols, rows, &new_row) {
            return Ok(false);
        }
        let row_index = self.row_index(rows);
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
        fill_buffer(&mut buffer, &new_row, cols, self.theme.surface, &self.theme);
        write_buffer(out, &buffer, row_index, x, cols)?;
        self.paint_row_overlays(out, row_index, x, cols)?;
        self.last_row = Some((x, cols, new_row));
        self.last_viewport = Some((cols, rows));
        Ok(true)
    }

    /// Paint the error line when it owns the row; `None` when the widget
    /// pipeline does.
    fn paint_error_takeover<W: Write>(
        &mut self,
        out: &mut W,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<Option<bool>> {
        if self.error.is_some() {
            return self.paint_error_line(out, x, cols, rows).map(Some);
        }
        Ok(None)
    }

    /// The caller's context plus the painter-owned window list, cwd, and exit
    /// code, injected before compose so `last_row` holds what is on screen.
    fn ctx_with_window_list<'a>(&'a self, ctx: &StatusBarContext<'a>) -> StatusBarContext<'a> {
        StatusBarContext {
            prefix: &self.prefix,
            windows: &self.windows,
            cwd: self.focused_cwd.as_deref().unwrap_or(""),
            last_exit: self.last_exit,
            ..*ctx
        }
    }

    /// Whether the cached strip can stand in for a compose: a pane-output
    /// paint with a cache for exactly this span and viewport.
    fn cached_compose_stands_in(
        &self,
        compose: ComposePolicy,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> bool {
        if matches!(compose, ComposePolicy::Always) {
            return false;
        }
        let Some((prev_x, prev_cols, _)) = self.last_row.as_ref() else {
            return false;
        };
        *prev_x == x && *prev_cols == cols && self.last_viewport == Some((cols, rows))
    }

    /// Whether the fresh row differs from the cached paint in content,
    /// origin, or viewport.
    fn needs_repaint(&self, x: u16, cols: u16, rows: u16, new_row: &[WidgetCell]) -> bool {
        let viewport_changed = self.last_viewport != Some((cols, rows));
        let row_changed = match &self.last_row {
            Some((prev_x, w, prev)) => *prev_x != x || *w != cols || prev.as_slice() != new_row,
            None => true,
        };
        viewport_changed || row_changed
    }

    /// The viewport row the bar occupies, given its configured position.
    const fn row_index(&self, rows: u16) -> u16 {
        match self.position {
            Position::Bottom => rows.saturating_sub(1),
            Position::Top => 0,
        }
    }

    /// Overlay the badge, the attention chip, and the notice toast on the
    /// freshly painted row (the full-row repaint already erased stale ones).
    fn paint_row_overlays<W: Write>(
        &self,
        out: &mut W,
        row_index: u16,
        x: u16,
        cols: u16,
    ) -> io::Result<()> {
        if let Some(gap) = self.badge_gap(cols) {
            let mut blank = Buffer::empty(Rect::new(0, 0, 1, 1));
            blank.set_style(
                Rect::new(0, 0, 1, 1),
                Style::default().bg(self.theme.surface),
            );
            write_buffer(out, &blank, row_index, x + gap, 1)?;
        }
        if let Some(badge) = &self.supervisory {
            paint_attention_overlay(out, badge, row_index, x, cols, 0, self.theme.accent)?;
        }
        if let Some(hint) = &self.attention {
            paint_attention_overlay(
                out,
                hint,
                row_index,
                x,
                cols,
                self.attention_offset(),
                self.theme.attention,
            )?;
        }
        if let Some((notice, _)) = &self.notice {
            paint_notice_overlay(out, notice, row_index, x, cols, &self.theme)?;
        }
        // The key chip (pending prefix, or the active key mode) outranks every
        // other right-edge chip: it is what the next keystroke depends on.
        if let Some(chip) = self.key_chip() {
            if let Some(gap) = self.key_gap(cols) {
                let mut blank = Buffer::empty(Rect::new(0, 0, 1, 1));
                blank.set_style(
                    Rect::new(0, 0, 1, 1),
                    Style::default().bg(self.theme.surface),
                );
                write_buffer(out, &blank, row_index, x + gap, 1)?;
            }
            paint_attention_overlay(out, &chip, row_index, x, cols, 0, self.theme.accent)?;
        }
        Ok(())
    }

    /// Compose the bar row into an `inset`-wide buffer without emitting VT or
    /// touching the cache, for `phux snapshot --rendered`. Returns
    /// `(buffer, x, row_index)`, or `None` when nothing would paint.
    pub(crate) fn compose_buffer(
        &self,
        inset: BarInset,
        cols: u16,
        rows: u16,
        ctx: &StatusBarContext<'_>,
    ) -> Option<(Buffer, u16, u16)> {
        if cols == 0 || rows == 0 {
            return None;
        }
        let (x, cols) = inset.span(cols);
        if cols == 0 {
            return None;
        }
        let row_index: u16 = match self.position {
            Position::Bottom => rows.saturating_sub(1),
            Position::Top => 0,
        };
        if let Some(message) = &self.error {
            return Some((
                full_row_buffer(message, alarm_style(&self.theme), cols),
                x,
                row_index,
            ));
        }
        // Match `paint`: the badge only composes onto a non-empty bar row.
        if self.bar.is_empty() && self.windows.is_empty() {
            return None;
        }
        let ctx = StatusBarContext {
            prefix: &self.prefix,
            windows: &self.windows,
            cwd: self.focused_cwd.as_deref().unwrap_or(""),
            last_exit: self.last_exit,
            ..*ctx
        };
        let mut row = self.bar.render(&ctx.as_widget(), self.widget_cols(cols));
        mark_window_drop(&mut row, self.drop_at);
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, 1));
        fill_buffer(&mut buffer, &row, cols, self.theme.surface, &self.theme);
        if let Some(gap) = self.badge_gap(cols) {
            buffer[(gap, 0)].reset();
            buffer[(gap, 0)].set_bg(self.theme.surface);
        }
        // ADR-0033: overlay the supervisory badge into the snapshot buffer so
        // `phux snapshot --rendered` shows the same chip the live paint draws.
        if let Some(badge) = &self.supervisory {
            overlay_badge_into_buffer(
                &mut buffer,
                badge,
                cols,
                0,
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            );
        }
        // The attention hint composes left of the badge, themed
        // like the live paint.
        if let Some(hint) = &self.attention {
            overlay_badge_into_buffer(
                &mut buffer,
                hint,
                cols,
                self.attention_offset(),
                Style::default()
                    .fg(self.theme.attention)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            );
        }
        // Mirror the live paint. The transient notice is a
        // compact toast over the normal row and temporarily outranks the
        // persistent right-edge chips beneath it.
        if let Some((notice, _)) = &self.notice {
            overlay_notice_into_buffer(&mut buffer, notice, cols, &self.theme);
        }
        if let Some(chip) = self.key_chip() {
            if let Some(gap) = self.key_gap(cols) {
                buffer[(gap, 0)].reset();
                buffer[(gap, 0)].set_bg(self.theme.surface);
            }
            overlay_badge_into_buffer(
                &mut buffer,
                &chip,
                cols,
                0,
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            );
        }
        Some((buffer, x, row_index))
    }

    /// Paint the error diagnostic as a reverse-video alarm strip.
    fn paint_error_line<W: Write>(
        &mut self,
        out: &mut W,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<bool> {
        // Callers gate on `self.error.is_some()`; an empty string is a
        // valid (if unusual) diagnostic, so default to "" rather than
        // returning early.
        let message = self.error.clone().unwrap_or_default();
        let style = alarm_style(&self.theme);
        self.paint_full_row_message(out, &message, style, x, cols, rows)
    }

    /// Paint `message` full-row in `style`, cached like the widget path
    /// (message changes always go through an invalidating setter).
    fn paint_full_row_message<W: Write>(
        &mut self,
        out: &mut W,
        message: &str,
        style: Style,
        x: u16,
        cols: u16,
        rows: u16,
    ) -> io::Result<bool> {
        let viewport_changed = self.last_viewport != Some((cols, rows));
        let moved = self
            .last_row
            .as_ref()
            .is_some_and(|(px, pw, _)| *px != x || *pw != cols);
        if !viewport_changed && !moved && self.last_row.is_some() {
            return Ok(false);
        }
        let row_index: u16 = match self.position {
            Position::Bottom => rows.saturating_sub(1),
            Position::Top => 0,
        };
        let buffer = full_row_buffer(message, style, cols);
        write_buffer(out, &buffer, row_index, x, cols)?;
        // Mark the cache populated so the span-only key short-circuits the
        // next repaint; the stored row is empty (we don't compose widgets).
        self.last_row = Some((x, cols, Vec::new()));
        self.last_viewport = Some((cols, rows));
        Ok(true)
    }

    /// The async data feeds behind the bar's `exec` widgets.
    /// The driver spawns one bounded interval runner per feed; an
    /// error-line painter (empty bar) has none.
    #[must_use]
    pub fn exec_feeds(&self) -> Vec<phux_config::widget::ExecFeed> {
        self.bar.exec_feeds()
    }

    /// Force the next `paint_outcome` to redraw unconditionally —
    /// e.g. after a SIGWINCH or after the pane renderer wrote the
    /// bottom row.
    pub fn invalidate(&mut self) {
        self.last_row = None;
        self.last_viewport = None;
    }

    /// The window tab under screen column `x`, read from the strip last
    /// painted (so hits match the screen); `None` off the strip or on a
    /// non-tab cell.
    #[must_use]
    pub fn window_hit_at(&self, x: u16) -> Option<usize> {
        match self.hit_at(x)? {
            phux_config::widget::CellHit::Window(i) => Some(i),
            phux_config::widget::CellHit::Switch | phux_config::widget::CellHit::Action(_) => None,
        }
    }

    /// The interactive target under screen column `x`, resolved against the
    /// strip last painted.
    #[must_use]
    pub fn hit_at(&self, x: u16) -> Option<phux_config::widget::CellHit> {
        let (origin, cols, row) = self.last_row.as_ref()?;
        let col = x.checked_sub(*origin)?;
        if self
            .notice
            .as_ref()
            .and_then(|(notice, _)| notice_span(notice, *cols))
            .is_some_and(|span| span.contains(&col))
            || self
                .supervisory
                .as_deref()
                .is_some_and(|s| badge_span(s, *cols, 0).contains(&col))
            || self
                .attention
                .as_deref()
                .is_some_and(|s| badge_span(s, *cols, self.attention_offset()).contains(&col))
            || self.badge_gap(*cols) == Some(col)
            || self
                .key_chip()
                .is_some_and(|chip| badge_span(&chip, *cols, 0).contains(&col))
            || self.key_gap(*cols) == Some(col)
        {
            return None;
        }
        row.get(usize::from(col))?.hit
    }
}
