//! Which rows a [`SelectList`] shows, where its cursor sits among them,
//! and the box they are laid out in.

use phux_config::keybind::ResolvedAction;
use ratatui::layout::Rect;

use super::fuzzy::{fuzzy_match, fuzzy_rank};
use super::{CHROME_ROWS, SelectItem, SelectList};
use crate::render::overlay::widgets::{MODAL_PAD, centered_panel, modal_inner_width};

/// Whether `next` is `prev` with only the session display name changed
/// (same action, `host`, and `id`).
fn same_renamed_session(prev: &ResolvedAction, next: &ResolvedAction) -> bool {
    if prev.action != next.action || !is_session_action(&prev.action) {
        return false;
    }
    let Some(id) = prev.args.get("id").and_then(toml::Value::as_integer) else {
        return false;
    };
    if next.args.get("id").and_then(toml::Value::as_integer) != Some(id) {
        return false;
    }
    if prev.args.get("host") != next.args.get("host") {
        return false;
    }
    prev.args.len() == next.args.len()
        && prev
            .args
            .iter()
            .all(|(key, value)| key == "name" || next.args.get(key) == Some(value))
}

/// Actions whose rows name a session the picker can follow across a rename.
fn is_session_action(action: &str) -> bool {
    action == "switch-session" || action == "switch-host"
}

impl SelectList {
    /// Replace the items in place, keeping the query. The selection follows
    /// the selected row's action (a renamed session row, its id), else keeps
    /// its position, snapped.
    pub fn replace_items(&mut self, items: Vec<SelectItem>) {
        let row = self.cursor_row(&self.filtered_indices()).unwrap_or(0);
        let kept = self.selected_action();
        self.items = items.into();
        self.declared.set(false);
        let indices = self.filtered_indices();
        let followed = kept.as_ref().and_then(|action| {
            self.row_of_action(&indices, action)
                .or_else(|| self.row_of_renamed_session(&indices, action))
        });
        self.model.cursor = followed.map_or_else(
            || self.snap_from_row(&indices, row),
            |followed| Some(indices[followed]),
        );
    }

    /// The highlighted item, when it is a selectable row.
    pub(super) fn cursor_item(&self) -> Option<&SelectItem> {
        let item = self.items.get(self.model.cursor?)?;
        (!item.is_header()).then_some(item)
    }

    /// The action of the highlighted row, when it is a selectable item.
    pub(super) fn selected_action(&self) -> Option<ResolvedAction> {
        self.cursor_item().map(|item| item.action.clone())
    }

    /// The cursor's visible row (an index into `indices`), if it is shown.
    pub(super) fn cursor_row(&self, indices: &[usize]) -> Option<usize> {
        let cursor = self.model.cursor?;
        indices.iter().position(|&idx| idx == cursor)
    }

    /// The visible row (an index into `indices`) of the selectable item
    /// committing `action`, if any.
    pub(super) fn row_of_action(
        &self,
        indices: &[usize],
        action: &ResolvedAction,
    ) -> Option<usize> {
        indices
            .iter()
            .position(|&idx| !self.items[idx].is_header() && self.items[idx].action == *action)
    }

    /// The visible row of the same session as `action` after a rename: only
    /// the display `name` may differ.
    pub(super) fn row_of_renamed_session(
        &self,
        indices: &[usize],
        action: &ResolvedAction,
    ) -> Option<usize> {
        indices.iter().position(|&idx| {
            let item = &self.items[idx];
            !item.is_header() && same_renamed_session(action, &item.action)
        })
    }

    /// Indices of items to display: every row in source order for an empty
    /// query, else the matching selectable rows best-first (stable).
    pub(super) fn filtered_indices(&self) -> Vec<usize> {
        let q = self.query().to_lowercase();
        if q.is_empty() {
            return (0..self.items.len()).collect();
        }
        fuzzy_rank(
            &q,
            self.items
                .iter()
                .enumerate()
                .filter(|(_, item)| !item.is_header())
                .map(|(i, item)| (i, item.filter_text())),
        )
    }

    /// Whether the visible row `row` is selectable.
    pub(super) fn row_selectable(&self, indices: &[usize], row: usize) -> bool {
        indices
            .get(row)
            .is_some_and(|&idx| !self.items[idx].is_header())
    }

    /// Whether any visible row can hold the cursor.
    pub(super) fn has_selectable(&self) -> bool {
        let indices = self.filtered_indices();
        (0..indices.len()).any(|row| self.row_selectable(&indices, row))
    }

    /// The item of the selectable row nearest `row` (clamped to the visible
    /// rows), forward then back; `None` when nothing is selectable.
    pub(super) fn snap_from_row(&self, indices: &[usize], row: usize) -> Option<usize> {
        let row = row.min(indices.len().checked_sub(1)?);
        (row..indices.len())
            .chain((0..row).rev())
            .find(|&r| self.row_selectable(indices, r))
            .map(|r| indices[r])
    }

    /// Keep the cursor on a visible selectable row.
    pub(super) fn snap_cursor(&mut self) {
        let indices = self.filtered_indices();
        if self
            .cursor_row(&indices)
            .is_some_and(|row| self.row_selectable(&indices, row))
        {
            return;
        }
        self.model.cursor = self.snap_from_row(&indices, 0);
    }

    /// After the query changed: the rows are re-ranked, so the selection
    /// moves to the best match (the first selectable row), never to whatever
    /// now happens to sit at the old position, and the view rewinds.
    pub(super) fn select_best_match(&mut self) {
        let indices = self.filtered_indices();
        self.model.cursor = self.snap_from_row(&indices, 0);
        self.model.scroll.set(0);
        self.declared.set(false);
    }

    /// The modal rect: a box just tall enough for every row, up to half the
    /// viewport (min 30 wide; see `centered_panel`), centered. A compact
    /// viewport takes the full height either way. The box never shrinks
    /// below the tallest one this list has painted.
    pub(super) fn modal_area(&self, outer: Rect) -> Rect {
        let bp = self.breakpoints;
        let roomy = centered_panel(outer, 5, 30, 10, bp);
        if bp.is_row_starved(outer.height) {
            return roomy;
        }
        let rows = u16::try_from(self.items.len().max(1)).unwrap_or(u16::MAX);
        let fit = rows
            .saturating_add(CHROME_ROWS)
            .max(self.tallest.get())
            .min(roomy.height);
        self.tallest.set(fit);
        let slack = roomy.height - fit;
        Rect::new(roomy.x, roomy.y + slack / 2, roomy.width, fit)
    }

    /// The query field: the first interior row, inside the padding.
    pub(super) fn query_area(modal: Rect) -> Rect {
        Rect::new(
            modal.x.saturating_add(1 + MODAL_PAD),
            modal.y.saturating_add(1),
            modal_inner_width(modal.width),
            u16::from(modal.height > 2),
        )
    }

    /// The list rows: every interior row under the query line.
    pub(super) const fn rows_area(modal: Rect) -> Rect {
        Rect::new(
            modal.x.saturating_add(1 + MODAL_PAD),
            modal.y.saturating_add(CHROME_ROWS - 1),
            modal_inner_width(modal.width),
            modal.height.saturating_sub(CHROME_ROWS),
        )
    }

    /// The scrollbar track: the right border column beside the list rows.
    pub(super) const fn scrollbar_track(modal: Rect) -> Rect {
        let rows = Self::rows_area(modal);
        Rect::new(
            modal.x + modal.width.saturating_sub(1),
            rows.y,
            1,
            rows.height,
        )
    }

    /// The bottom-left border text: the selection's position among the
    /// selectable rows (`3 of 61`), the match count alone when nothing is
    /// selected, or `no matches` when the query filtered every row out.
    pub(super) fn footer(&self, indices: &[usize]) -> String {
        let selectable = indices
            .iter()
            .filter(|&&idx| !self.items[idx].is_header())
            .count();
        if selectable == 0 && !self.query().is_empty() {
            return "no matches".to_owned();
        }
        let Some(row) = self
            .cursor_row(indices)
            .filter(|&row| self.row_selectable(indices, row))
        else {
            return format!("{selectable} rows");
        };
        let at = indices[..=row]
            .iter()
            .filter(|&&idx| !self.items[idx].is_header())
            .count();
        format!("{at} of {selectable}")
    }

    /// Char positions in `item.label` the query matches, when the label
    /// alone is a match (a row kept by its keywords or chord shows no
    /// highlight). `None` with an empty query.
    pub(super) fn label_hits(&self, item: &SelectItem) -> Option<Vec<usize>> {
        if self.query().is_empty() {
            return None;
        }
        let lowered = item.label.to_lowercase();
        // A lowercase form of a different length cannot index the label.
        if lowered.chars().count() != item.label.chars().count() {
            return None;
        }
        fuzzy_match(&self.query().to_lowercase(), &lowered).map(|(_, hits)| hits)
    }
}
