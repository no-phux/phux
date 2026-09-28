//! Reusable selectable-list overlay.
//!
//! A themed [`Modal`] with a query line over a filtered, scrollable list
//! whose rows commit a [`ResolvedAction`] through `run_action()`. It backs the command palette and every picker.
//!
//! Keys: Up/`C-p`, Down/`C-n` (and `j`/`k` while the query is empty),
//! `PageUp`/`PageDown`, `Home`/`End`, the wheel ([`WHEEL_SCROLL_ROWS`]);
//! text filters, Backspace edits, Enter commits, Esc dismisses.
//!
//! Only the rows that fit are painted, windowed around the selection
//! ([`scroll_into_view`]), with a scrollbar when the list overflows. A
//! non-empty query is a scored fuzzy match ([`fuzzy_score`]) ranked
//! best-first; [`SelectKind::Header`] rows group an empty query and vanish
//! once the user types.

use std::cell::Cell;

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::widgets::{Modal, centered_panel, modal_inner_width, paint_scrollbar, scroll_into_view};
use super::{OverlayCommand, RenderOverlay};
use crate::render::clip_text;
use crate::render::{ChromeBreakpoints, Theme};

/// Blank columns between a row's label and its secondary.
const GAP: usize = 1;

/// Modal rows that are not list rows: two borders and the query line.
const CHROME_ROWS: u16 = 3;

/// Rows the selection moves per wheel detent (matches copy-mode).
pub const WHEEL_SCROLL_ROWS: usize = 3;

/// A selectable row or a non-selectable section header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    /// A normal selectable row that commits its [`SelectItem::action`].
    Item,
    /// A dim, non-selectable section label, hidden once the user types.
    Header,
}

/// One row in a [`SelectList`] — either a selectable item or a header.
#[derive(Debug, Clone)]
pub struct SelectItem {
    /// Primary display label (left column).
    pub label: String,
    /// Optional right-aligned, dimmed secondary (a chord, a count).
    pub secondary: Option<String>,
    /// The action committed when chosen (ignored for headers).
    pub action: ResolvedAction,
    /// Whether this row is selectable or a section header.
    pub kind: SelectKind,
    /// Indent the label two spaces to nest under the header above it.
    pub indented: bool,
    /// Paint the label in the theme's `attention` slot (unless selected).
    pub attention: bool,
}

impl SelectItem {
    /// An item displaying `label` that commits `action`.
    #[must_use]
    pub fn new(label: impl Into<String>, action: ResolvedAction) -> Self {
        Self {
            label: label.into(),
            secondary: None,
            action,
            kind: SelectKind::Item,
            indented: false,
            attention: false,
        }
    }

    /// A non-selectable header with a placeholder action it never commits.
    #[must_use]
    pub fn header(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            secondary: None,
            action: ResolvedAction {
                action: String::new(),
                args: std::collections::BTreeMap::new(),
            },
            kind: SelectKind::Header,
            indented: false,
            attention: false,
        }
    }

    /// Attach a right-aligned, dimmed secondary label.
    #[must_use]
    pub fn secondary(mut self, secondary: impl Into<String>) -> Self {
        self.secondary = Some(secondary.into());
        self
    }

    /// Nest this row under the header above it.
    #[must_use]
    pub const fn indented(mut self) -> Self {
        self.indented = true;
        self
    }

    /// Mark this row as needing the user's attention.
    #[must_use]
    pub const fn attention(mut self) -> Self {
        self.attention = true;
        self
    }

    /// `true` for a non-selectable [`SelectKind::Header`] row.
    #[must_use]
    pub const fn is_header(&self) -> bool {
        matches!(self.kind, SelectKind::Header)
    }

    /// The text a query matches: label plus secondary (so chords filter too).
    fn filter_text(&self) -> String {
        self.secondary
            .as_ref()
            .map_or_else(|| self.label.clone(), |sec| format!("{} {sec}", self.label))
    }
}

/// A themed, filterable, selectable list rendered as an overlay.
#[derive(Debug, Clone)]
pub struct SelectList {
    /// Modal title (e.g. `"command palette"`).
    title: String,
    /// All items, unfiltered (filtering is recomputed per keystroke).
    items: Vec<SelectItem>,
    /// Current query text.
    query: String,
    /// Selection index into the *filtered* list, clamped on every change.
    selected: usize,
    /// Theme snapshot (copied, so the overlay stays `'static`).
    theme: Theme,
    /// `Some` for a live projection that accepts in-place refreshes tagged
    /// with this key ([`RenderOverlay::refresh_items`]); `None` ignores them.
    live_key: Option<&'static str>,
    /// First visible filtered row. Interior-mutable because the viewport
    /// height is only known at paint time and `render` takes `&self` (the
    /// bargain ratatui's `ListState::offset` makes).
    scroll: Cell<usize>,
    /// Viewport rows at the last paint, for page moves (0 before the first).
    page: Cell<usize>,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
}

impl SelectList {
    /// A list titled `title` over `items`, selection on the first selectable
    /// row, empty query.
    #[must_use]
    pub fn new(title: impl Into<String>, items: Vec<SelectItem>, theme: &Theme) -> Self {
        let mut list = Self {
            title: title.into(),
            items,
            query: String::new(),
            selected: 0,
            theme: *theme,
            live_key: None,
            scroll: Cell::new(0),
            page: Cell::new(0),
            breakpoints: ChromeBreakpoints::default(),
        };
        // Grouped pickers open on a header; start on the first selectable row.
        let indices = list.filtered_indices();
        list.snap_to_selectable(&indices);
        list
    }

    /// Opt this list into live row refreshes tagged `key`.
    #[must_use]
    pub const fn with_live_key(mut self, key: &'static str) -> Self {
        self.live_key = Some(key);
        self
    }

    /// Replace the items in place, keeping the query. The selection follows
    /// the selected row's action (a row inserted above the cursor must not
    /// change what Enter commits), else keeps its position, snapped.
    pub fn replace_items(&mut self, items: Vec<SelectItem>) {
        let kept = self.selected_action();
        self.items = items;
        let indices = self.filtered_indices();
        if let Some(row) = kept.and_then(|action| self.row_of_action(&indices, &action)) {
            self.selected = row;
            return;
        }
        self.snap_to_selectable(&indices);
    }

    /// The action of the highlighted row, when it is a selectable item.
    fn selected_action(&self) -> Option<ResolvedAction> {
        let indices = self.filtered_indices();
        let item = &self.items[*indices.get(self.selected)?];
        (!item.is_header()).then(|| item.action.clone())
    }

    /// The visible row (an index into `indices`) of the selectable item
    /// committing `action`, if any.
    fn row_of_action(&self, indices: &[usize], action: &ResolvedAction) -> Option<usize> {
        indices
            .iter()
            .position(|&idx| !self.items[idx].is_header() && self.items[idx].action == *action)
    }

    /// Indices of items to display: every row in source order for an empty
    /// query, else the matching selectable rows best-first (stable).
    fn filtered_indices(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
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
    fn row_selectable(&self, indices: &[usize], row: usize) -> bool {
        indices
            .get(row)
            .is_some_and(|&idx| !self.items[idx].is_header())
    }

    /// Move `selected` onto the nearest selectable row, forward then back.
    fn snap_to_selectable(&mut self, indices: &[usize]) {
        let visible = indices.len();
        self.clamp_selection(visible);
        if visible == 0 {
            return;
        }
        if self.row_selectable(indices, self.selected) {
            return;
        }
        for row in self.selected..visible {
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
        for row in (0..self.selected).rev() {
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
    }

    /// Clamp `selected` to a visible row (0 when nothing matches).
    const fn clamp_selection(&mut self, visible: usize) {
        if visible == 0 {
            self.selected = 0;
        } else if self.selected >= visible {
            self.selected = visible - 1;
        }
    }

    /// Move to the next selectable row, saturating (no wrap).
    fn select_down(&mut self, indices: &[usize]) {
        let visible = indices.len();
        let mut row = self.selected;
        while row + 1 < visible {
            row += 1;
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
    }

    /// Move to the previous selectable row, saturating.
    fn select_up(&mut self, indices: &[usize]) {
        let mut row = self.selected;
        while row > 0 {
            row -= 1;
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
    }

    /// Move down a screenful.
    fn select_page_down(&mut self, indices: &[usize]) {
        for _ in 0..self.page_rows() {
            self.select_down(indices);
        }
    }

    /// Move up a screenful.
    fn select_page_up(&mut self, indices: &[usize]) {
        for _ in 0..self.page_rows() {
            self.select_up(indices);
        }
    }

    /// Rows per page move: the last painted height, or 1 before any paint.
    fn page_rows(&self) -> usize {
        self.page.get().max(1)
    }

    /// Jump to the first selectable row (Home).
    fn select_first(&mut self, indices: &[usize]) {
        self.selected = 0;
        self.snap_to_selectable(indices);
    }

    /// Jump to the last selectable row (End); a trailing header resolves
    /// upward.
    fn select_last(&mut self, indices: &[usize]) {
        self.selected = indices.len().saturating_sub(1);
        self.snap_to_selectable(indices);
    }

    /// The modal rect: half the viewport, min 30x10 (see `centered_panel`).
    fn modal_area(outer: Rect, bp: ChromeBreakpoints) -> Rect {
        centered_panel(outer, 5, 30, 10, bp)
    }

    /// Rows available to the list inside `modal_area`.
    const fn list_height(modal_area: Rect) -> usize {
        modal_area.height.saturating_sub(CHROME_ROWS) as usize
    }

    /// The scrollbar track: the right border column beside the list rows.
    fn scrollbar_track(modal_area: Rect) -> Rect {
        Rect::new(
            modal_area.x + modal_area.width.saturating_sub(1),
            modal_area.y.saturating_add(3),
            1,
            u16::try_from(Self::list_height(modal_area)).unwrap_or(u16::MAX),
        )
    }

    /// Body lines: the query line, then the visible `window` rows (starting at
    /// filtered index `offset`), or a dim "(no matches)".
    fn body_lines(&self, window: &[usize], offset: usize, inner_width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        // Query line: a dim prompt, the text, and a reverse-video caret.
        lines.push(Line::from(vec![
            Span::styled("> ".to_owned(), Style::default().fg(self.theme.dim)),
            Span::styled(
                self.visible_query(inner_width.saturating_sub(3)),
                Style::default().fg(self.theme.text),
            ),
            Span::styled(
                " ",
                Style::default()
                    .fg(self.theme.surface)
                    .bg(self.theme.accent),
            ),
        ]));

        if window.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no matches)".to_owned(),
                Style::default().fg(self.theme.dim),
            )));
            return lines;
        }

        for (row, &idx) in window.iter().enumerate() {
            let item = &self.items[idx];
            if item.is_header() {
                lines.push(self.header_line(item));
                continue;
            }
            let selected = offset + row == self.selected;
            lines.push(self.item_line(item, selected, inner_width));
        }
        lines
    }

    /// The query's tail that fits `width`, so the caret stays visible.
    fn visible_query(&self, width: u16) -> String {
        let query = clip_text(&self.query, usize::MAX);
        let mut remaining = crate::render::display_width(&query);
        for (i, ch) in query.char_indices() {
            let cells = crate::render::cell_width(ch).unwrap_or(0);
            if remaining <= usize::from(width) && cells > 0 {
                return query[i..].to_owned();
            }
            remaining = remaining.saturating_sub(cells);
        }
        String::new()
    }

    /// A dim, non-selectable section-header row.
    fn header_line(&self, item: &SelectItem) -> Line<'static> {
        Line::from(Span::styled(
            item.label.clone(),
            Style::default().fg(self.theme.section_header),
        ))
    }

    /// One row laid out to exactly `inner_width` cells: label left, dimmed
    /// secondary right. Under pressure the secondary yields first, entirely
    /// if need be; the label is the row's identity.
    fn item_line(&self, item: &SelectItem, selected: bool, inner_width: u16) -> Line<'static> {
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
        let padding =
            " ".repeat(width.saturating_sub(crate::render::display_width(&label) + sec_w));

        if selected {
            // Own both colors so the selection reads on light terminals too.
            let text = format!("{label}{padding}{secondary}");
            Line::from(Span::styled(
                text,
                Style::default()
                    .fg(self.theme.selection_fg)
                    .bg(self.theme.selection_bg),
            ))
        } else {
            let label_style = if item.attention {
                Style::default()
                    .fg(self.theme.attention)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.text)
            };
            Line::from(vec![
                Span::styled(label, label_style),
                Span::raw(padding),
                Span::styled(secondary, Style::default().fg(self.theme.dim)),
            ])
        }
    }
}

impl RenderOverlay for SelectList {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = Self::modal_area(area, self.breakpoints);
        let indices = self.filtered_indices();
        let inner_width = modal_inner_width(modal_area.width);

        // Window the rows around the selection; the height is known only here.
        let height = Self::list_height(modal_area);
        self.page.set(height);
        let offset = scroll_into_view(self.scroll.get(), self.selected, indices.len(), height);
        self.scroll.set(offset);
        let window = indices
            .get(offset..(offset + height).min(indices.len()))
            .unwrap_or(&[]);

        let body = self.body_lines(window, offset, inner_width);
        Modal::new(&self.theme, self.title.clone(), body).render_into(modal_area, buf);
        paint_scrollbar(
            buf,
            Self::scrollbar_track(modal_area),
            &self.theme,
            indices.len(),
            offset,
        );
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(Self::modal_area(area, self.breakpoints))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn refresh_items(&mut self, key: &str, items: &[SelectItem]) -> bool {
        if self.live_key != Some(key) {
            return false;
        }
        self.replace_items(items.to_vec());
        true
    }

    /// The wheel moves the selection; the viewport follows it.
    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        if mouse.action != MouseAction::Press {
            return OverlayCommand::Stay;
        }
        let indices = self.filtered_indices();
        match mouse.button {
            MouseButton::Four => {
                for _ in 0..WHEEL_SCROLL_ROWS {
                    self.select_up(&indices);
                }
            }
            MouseButton::Five => {
                for _ in 0..WHEEL_SCROLL_ROWS {
                    self.select_down(&indices);
                }
            }
            _ => {}
        }
        OverlayCommand::Stay
    }

    fn handle_paste(&mut self, text: &str) {
        self.query.push_str(text);
        let indices = self.filtered_indices();
        self.snap_to_selectable(&indices);
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        let indices = self.filtered_indices();
        self.snap_to_selectable(&indices);

        // Ctrl-modified navigation works regardless of query content.
        if key.mods.contains(ModSet::CTRL) {
            match key.key {
                PhysicalKey::N => {
                    self.select_down(&indices);
                    return OverlayCommand::Stay;
                }
                PhysicalKey::P => {
                    self.select_up(&indices);
                    return OverlayCommand::Stay;
                }
                _ => {}
            }
        }

        match key.key {
            PhysicalKey::Escape => OverlayCommand::Dismiss,
            PhysicalKey::Enter => indices
                .get(self.selected)
                .map_or(OverlayCommand::Stay, |&idx| {
                    let item = &self.items[idx];
                    if item.is_header() {
                        OverlayCommand::Stay
                    } else {
                        OverlayCommand::Commit(item.action.clone())
                    }
                }),
            PhysicalKey::ArrowDown => {
                self.select_down(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::ArrowUp => {
                self.select_up(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::PageDown => {
                self.select_page_down(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::PageUp => {
                self.select_page_up(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::Home => {
                self.select_first(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::End => {
                self.select_last(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::Backspace => {
                self.query.pop();
                let indices = self.filtered_indices();
                self.snap_to_selectable(&indices);
                OverlayCommand::Stay
            }
            // `j`/`k` navigate only while the query is empty.
            PhysicalKey::J if self.query.is_empty() => {
                self.select_down(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::K if self.query.is_empty() => {
                self.select_up(&indices);
                OverlayCommand::Stay
            }
            _ => {
                if let Some(t) = &key.text
                    && !t.chars().any(char::is_control)
                {
                    self.query.push_str(t);
                    let indices = self.filtered_indices();
                    self.snap_to_selectable(&indices);
                }
                OverlayCommand::Stay
            }
        }
    }
}

/// Indices of the `(index, haystack)` pairs a lowercased `query` fuzzy-matches,
/// best first; ties keep input order.
pub(super) fn fuzzy_rank(
    query: &str,
    haystacks: impl Iterator<Item = (usize, String)>,
) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = haystacks
        .filter_map(|(i, hay)| fuzzy_score(query, &hay.to_lowercase()).map(|score| (score, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Scored subsequence (fuzzy) match of a lowercased `needle` in `haystack`.
///
/// `None` when it is not a subsequence, `0` for an
/// empty needle. Contiguous runs compound, word-boundary hits earn a bonus,
/// and skipped gaps cost a little. Only the induced ordering is contractual.
#[must_use]
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let mut score: i32 = 0;
    let mut run: i32 = 0;
    let mut hay_idx: usize = 0;
    let mut last_match: Option<usize> = None;

    for nc in needle.chars() {
        let found = hay[hay_idx..].iter().position(|&hc| hc == nc)?;
        let pos = hay_idx + found;

        // Gap penalty: earlier, tighter matches win.
        let gap = last_match.map_or(pos, |prev| pos - prev - 1);
        score -= i32::try_from(gap).unwrap_or(i32::MAX).min(20);

        // Word-boundary / prefix bonus.
        let at_boundary = pos == 0
            || hay
                .get(pos - 1)
                .is_some_and(|c| matches!(c, '-' | '_' | ' ' | ':' | '/'));
        if at_boundary {
            score += 10;
        }

        if last_match.is_some_and(|prev| prev + 1 == pos) {
            run += 1;
            score += 5 + run * 5;
        } else {
            run = 0;
        }

        last_match = Some(pos);
        hay_idx = pos + 1;
    }
    Some(score)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn action(name: &str) -> ResolvedAction {
        ResolvedAction {
            action: name.to_owned(),
            args: BTreeMap::new(),
        }
    }

    fn press(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: None,
        }
    }

    fn key(sl: &mut SelectList, key: PhysicalKey) -> OverlayCommand {
        sl.handle_key(&press(key, None))
    }

    fn type_str(sl: &mut SelectList, s: &str) {
        for ch in s.chars() {
            sl.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
    }

    /// The action Enter commits, or `None` when it commits nothing.
    fn commit(sl: &mut SelectList) -> Option<String> {
        match key(sl, PhysicalKey::Enter) {
            OverlayCommand::Commit(a) => Some(a.action),
            _ => None,
        }
    }

    fn labels(sl: &SelectList) -> Vec<&str> {
        sl.filtered_indices()
            .into_iter()
            .map(|i| sl.items[i].label.as_str())
            .collect()
    }

    fn list(items: Vec<SelectItem>) -> SelectList {
        SelectList::new("command palette", items, &Theme::default())
    }

    fn sample() -> SelectList {
        list(vec![
            SelectItem::new("split-pane", action("split-pane")).secondary("C-a |"),
            SelectItem::new("new-window", action("new-window")).secondary("C-a c"),
            SelectItem::new("detach", action("detach")).secondary("C-a d"),
        ])
    }

    fn grouped() -> SelectList {
        list(vec![
            SelectItem::header("Pane"),
            SelectItem::new("split-pane", action("split-pane")).indented(),
            SelectItem::header("Window"),
            SelectItem::new("new-window", action("new-window")).indented(),
        ])
    }

    /// `n` rows labelled `item-0 ..= item-(n-1)`.
    fn long_list(n: usize) -> SelectList {
        list(
            (0..n)
                .map(|i| SelectItem::new(format!("item-{i}"), action(&format!("act-{i}"))))
                .collect(),
        )
    }

    fn render_buf(sl: &SelectList, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        buf
    }

    fn render_to_string(sl: &SelectList, w: u16, h: u16) -> String {
        let buf = render_buf(sl, w, h);
        let mut out = String::new();
        for y in 0..h {
            let row: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    /// The label of the row painted on the selection surface, or `None` when
    /// no row is highlighted on screen (the cursor walked off the box).
    fn painted_selection(sl: &SelectList, w: u16, h: u16) -> Option<String> {
        let buf = render_buf(sl, w, h);
        (0..h).find_map(|y| {
            let row: String = (0..w)
                .filter(|&x| buf[(x, y)].bg == sl.theme.selection_bg)
                .map(|x| buf[(x, y)].symbol())
                .collect();
            let row = row.trim();
            row.starts_with("item-")
                .then(|| row.split_whitespace().next().unwrap_or_default().to_owned())
        })
    }

    fn painted_row(sl: &SelectList, idx: usize, width: u16) -> String {
        sl.item_line(&sl.items[idx], false, width)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn fuzzy_score_matches_ordered_subsequences_and_ranks_them() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert!(fuzzy_score("spn", "split-pane").is_some());
        assert!(fuzzy_score("nw", "new-window").is_some());
        assert_eq!(fuzzy_score("zzz", "new-window"), None);
        assert_eq!(fuzzy_score("wen", "new-window"), None, "order matters");
        // A contiguous front-anchored run beats a scattered match, and a
        // word-boundary hit beats a mid-word one.
        assert!(fuzzy_score("sp", "split-pane") > fuzzy_score("sp", "previous-pane"));
        assert!(fuzzy_score("p", "split-pane") > fuzzy_score("p", "copy-mode"));
    }

    #[test]
    fn filtering_ranks_best_first_and_keeps_ties_in_source_order() {
        let mut sl = list(vec![
            SelectItem::new("toggle-sidebar", action("toggle-sidebar")),
            SelectItem::new("split-pane", action("split-pane")),
            SelectItem::new("previous-pane", action("previous-pane")),
        ]);
        type_str(&mut sl, "sp");
        assert_eq!(labels(&sl)[0], "split-pane");

        let mut sl = list(vec![
            SelectItem::new("x-alpha", action("a")),
            SelectItem::new("x-bravo", action("b")),
        ]);
        type_str(&mut sl, "x");
        assert_eq!(labels(&sl), vec!["x-alpha", "x-bravo"]);

        // The secondary (a chord) is filter text too; typed `j` is text once
        // the query is non-empty.
        let mut sl = sample();
        type_str(&mut sl, "nw");
        assert_eq!(labels(&sl), vec!["new-window"]);
        let mut sl = sample();
        sl.handle_key(&press(PhysicalKey::A, Some("d")));
        sl.handle_key(&press(PhysicalKey::J, Some("j")));
        assert_eq!(sl.query, "dj");
        assert!(labels(&sl).is_empty());
    }

    #[test]
    fn headers_group_an_empty_query_and_vanish_when_filtering() {
        let mut sl = grouped();
        assert_eq!(sl.filtered_indices(), vec![0, 1, 2, 3]);
        // Opens on the first selectable row; Down skips the next header.
        assert_eq!(commit(&mut sl).as_deref(), Some("split-pane"));
        key(&mut sl, PhysicalKey::ArrowDown);
        assert_eq!(commit(&mut sl).as_deref(), Some("new-window"));
        type_str(&mut sl, "s");
        assert!(
            sl.filtered_indices()
                .iter()
                .all(|&i| !sl.items[i].is_header())
        );

        // A header-only list, a trailing header under End, and an empty
        // filter never commit a placeholder.
        assert_eq!(commit(&mut list(vec![SelectItem::header("Pane")])), None);
        let mut sl = list(vec![
            SelectItem::new("only-item", action("only")),
            SelectItem::header("Empty session"),
        ]);
        key(&mut sl, PhysicalKey::End);
        assert_eq!(commit(&mut sl).as_deref(), Some("only"));
        let mut sl = sample();
        type_str(&mut sl, "zzz");
        assert_eq!(commit(&mut sl), None);
    }

    #[test]
    fn keys_navigate_saturate_and_dismiss() {
        let mut sl = sample();
        let ctrl = |k| {
            let mut ev = press(k, None);
            ev.mods = ModSet::CTRL;
            ev
        };
        for _ in 0..3 {
            sl.handle_key(&ctrl(PhysicalKey::N));
        }
        assert_eq!(commit(&mut sl).as_deref(), Some("detach"), "no wrap");
        sl.handle_key(&ctrl(PhysicalKey::P));
        assert_eq!(commit(&mut sl).as_deref(), Some("new-window"));
        let mut sl = sample();
        sl.handle_key(&press(PhysicalKey::J, Some("j")));
        assert_eq!(
            commit(&mut sl).as_deref(),
            Some("new-window"),
            "j on an empty query"
        );
        assert_eq!(key(&mut sl, PhysicalKey::Escape), OverlayCommand::Dismiss);
    }

    /// The reported bug: walking the cursor down a long list marched it off
    /// the bottom of the box. The viewport follows the selection both ways.
    #[test]
    fn the_viewport_follows_the_selection() {
        let mut sl = long_list(40);
        for _ in 0..20 {
            key(&mut sl, PhysicalKey::ArrowDown);
        }
        assert_eq!(painted_selection(&sl, 40, 16).as_deref(), Some("item-20"));
        assert!(sl.scroll.get() > 0);
        for _ in 0..20 {
            key(&mut sl, PhysicalKey::ArrowUp);
        }
        let text = render_to_string(&sl, 40, 16);
        assert_eq!(sl.scroll.get(), 0);
        assert!(text.contains("item-0"), "{text}");

        // A narrowing filter rewinds a stranded offset.
        for _ in 0..30 {
            key(&mut sl, PhysicalKey::ArrowDown);
        }
        render_to_string(&sl, 40, 16);
        type_str(&mut sl, "item-7");
        let text = render_to_string(&sl, 40, 16);
        assert_eq!(sl.scroll.get(), 0);
        assert!(text.contains("item-7"), "{text}");

        // Only an overflowing list paints a scrollbar.
        assert!(render_to_string(&long_list(40), 40, 16).contains('█'));
        assert!(!render_to_string(&long_list(3), 40, 16).contains('█'));
    }

    #[test]
    fn page_home_end_and_wheel_move_the_selection() {
        let mut sl = long_list(40);
        key(&mut sl, PhysicalKey::PageDown);
        assert_eq!(sl.selected, 1, "no measured viewport: a single-row step");
        // 40x16 is compact: full-bleed, 16 - 3 chrome rows = 13 visible.
        render_to_string(&sl, 40, 16);
        key(&mut sl, PhysicalKey::PageDown);
        assert_eq!(sl.selected, 14);
        key(&mut sl, PhysicalKey::PageUp);
        assert_eq!(sl.selected, 1);
        key(&mut sl, PhysicalKey::End);
        assert_eq!(sl.selected, 39);
        assert_eq!(painted_selection(&sl, 40, 16).as_deref(), Some("item-39"));
        key(&mut sl, PhysicalKey::Home);
        assert_eq!(sl.selected, 0);

        let wheel = |button| MouseEvent {
            action: MouseAction::Press,
            button,
            mods: ModSet::empty(),
            x: 0.0,
            y: 0.0,
        };
        assert_eq!(
            sl.handle_mouse(&wheel(MouseButton::Five)),
            OverlayCommand::Stay
        );
        assert_eq!(sl.selected, WHEEL_SCROLL_ROWS);
        sl.handle_mouse(&wheel(MouseButton::Four));
        sl.handle_mouse(&wheel(MouseButton::Four));
        assert_eq!(sl.selected, 0, "saturates at the top");
    }

    /// Pins the mid-scroll box: windowed rows, the selection inside it, and
    /// the scrollbar thumb away from both ends.
    #[test]
    fn scrolled_list_render_is_stable() {
        let mut sl = long_list(24);
        for _ in 0..15 {
            key(&mut sl, PhysicalKey::ArrowDown);
        }
        insta::assert_snapshot!(render_to_string(&sl, 44, 16));
    }

    /// Above the compact breakpoint the picker floats: a centered box with
    /// panes visible around it, not a screen.
    #[test]
    fn roomy_viewport_render_still_floats() {
        let sl = sample();
        let area = Rect::new(0, 0, 100, 30);
        let bounds = sl.bounds(area).expect("bounded");
        assert_eq!((bounds.width, bounds.height), (50, 15));
        assert!(bounds.x > 0 && bounds.y > 0);
        insta::assert_snapshot!(render_to_string(&sl, 100, 30));
    }

    #[test]
    fn refresh_needs_the_live_key_and_keeps_query_and_target() {
        let fresh = vec![SelectItem::new("fresh-row", action("fresh"))];
        assert!(
            !sample().refresh_items("agent-fleet", &fresh),
            "static list"
        );
        assert!(
            !sample()
                .with_live_key("other")
                .refresh_items("agent-fleet", &fresh)
        );
        let mut sl = sample().with_live_key("agent-fleet");
        assert!(sl.refresh_items("agent-fleet", &fresh));
        assert_eq!(labels(&sl), vec!["fresh-row"]);

        // The query survives and keeps filtering the new rows.
        let mut sl = sample().with_live_key("agent-fleet");
        type_str(&mut sl, "det");
        sl.refresh_items(
            "agent-fleet",
            &[
                SelectItem::new("detach-me", action("a")),
                SelectItem::new("other", action("b")),
            ],
        );
        assert_eq!(labels(&sl), vec!["detach-me"]);
        assert_eq!(commit(&mut sl).as_deref(), Some("a"));

        // Rows inserted above the cursor: the selection follows the action.
        let mut sl = list(vec![
            SelectItem::new("work", action("work")),
            SelectItem::new("scratch", action("scratch")),
        ])
        .with_live_key("session-picker");
        key(&mut sl, PhysicalKey::ArrowDown);
        sl.refresh_items(
            "session-picker",
            &[
                SelectItem::header("This host"),
                SelectItem::new("work", action("work")),
                SelectItem::new("scratch", action("scratch")),
                SelectItem::header("edge"),
                SelectItem::new("build", action("build")),
            ],
        );
        assert_eq!(commit(&mut sl).as_deref(), Some("scratch"));

        // The action is gone: fall back to the same position, snapped and
        // clamped onto a selectable row.
        sl.refresh_items(
            "session-picker",
            &[
                SelectItem::new("c", action("c")),
                SelectItem::new("d", action("d")),
            ],
        );
        assert_eq!(sl.selected, 1);
        sl.refresh_items("session-picker", &[SelectItem::new("only", action("only"))]);
        assert_eq!(commit(&mut sl).as_deref(), Some("only"));
    }

    #[test]
    fn attention_rows_paint_hot_and_selection_owns_its_colors() {
        let theme = Theme::default();
        let sl = SelectList::new(
            "agent fleet",
            vec![
                SelectItem::new("calm", action("a")),
                SelectItem::new("needs-you", action("b")).attention(),
            ],
            &theme,
        );
        assert_eq!(
            sl.item_line(&sl.items[0], false, 40).spans[0].style.fg,
            Some(theme.text)
        );
        let hot = sl.item_line(&sl.items[1], false, 40);
        assert_eq!(hot.spans[0].style.fg, Some(theme.attention));
        assert!(hot.spans[0].style.add_modifier.contains(Modifier::BOLD));
        let selected = sl.item_line(&sl.items[1], true, 40);
        assert_eq!(selected.spans[0].style.fg, Some(theme.selection_fg));
        assert_eq!(selected.spans[0].style.bg, Some(theme.selection_bg));
    }

    /// Every row, at every width, measures exactly the interior width (a
    /// long row once painted through the modal border onto the pane behind).
    #[test]
    fn a_row_never_overruns_the_modal_interior() {
        let sl = list(vec![
            SelectItem::new(
                "a-very-long-window-name-that-will-not-fit-anywhere",
                action("x"),
            )
            .secondary("~/some/deeply/nested/working/directory  feature/branch"),
            SelectItem::new("构建工具", action("x")).secondary("工作目录/main"),
            SelectItem::new("cafe\u{301}", action("x"))
                .secondary("re\u{301}vision")
                .indented(),
        ]);
        for width in 1u16..=80 {
            for item in &sl.items {
                for selected in [false, true] {
                    let painted: String = sl
                        .item_line(item, selected, width)
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect();
                    assert_eq!(
                        crate::render::display_width(&painted),
                        usize::from(width),
                        "{width}: {painted:?}"
                    );
                }
            }
        }
    }

    /// The label is the row's identity: the secondary yields first, unless it
    /// is a short chord, which survives whole while a long label clips.
    #[test]
    fn narrow_rows_keep_the_label_and_short_chords() {
        let sl = list(vec![
            SelectItem::new("builder", action("x")).secondary("working - main"),
            SelectItem::new(
                "Zoom the focused pane to fill the whole window",
                action("z"),
            )
            .secondary("C-a z"),
        ]);
        assert_eq!(painted_row(&sl, 0, 30), "builder         working - main");
        assert!(painted_row(&sl, 0, 16).starts_with("builder"));
        assert_eq!(painted_row(&sl, 0, 7), "builder");
        let chord = painted_row(&sl, 1, 40);
        assert!(
            chord.ends_with("C-a z") && chord.contains(crate::render::ELLIPSIS),
            "{chord:?}"
        );
    }

    #[test]
    fn long_query_keeps_its_tail_without_changing_the_filter() {
        let mut sl = sample();
        sl.query = "prefix-构建-cafe\u{301}".to_owned();
        assert_eq!(sl.visible_query(5), "-cafe\u{301}");
        assert_eq!(sl.visible_query(0), "");
        assert_eq!(sl.visible_query(80), sl.query);
        for width in 0..20 {
            assert!(crate::render::display_width(&sl.visible_query(width)) <= usize::from(width));
        }
    }
}
