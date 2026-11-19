//! The rows a [`SelectList`](super::SelectList) shows.

use phux_config::keybind::ResolvedAction;

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
    /// Extra words a query matches but the row never shows (the palette's
    /// canonical action name, so `kill-pane` still finds "Close pane").
    pub keywords: Option<String>,
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
            keywords: None,
        }
    }

    /// A non-selectable header with a placeholder action it never commits.
    #[must_use]
    pub fn header(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            secondary: None,
            action: crate::attach::resolved("", std::collections::BTreeMap::new()),
            kind: SelectKind::Header,
            indented: false,
            attention: false,
            keywords: None,
        }
    }

    /// Attach a right-aligned, dimmed secondary label.
    #[must_use]
    pub fn secondary(mut self, secondary: impl Into<String>) -> Self {
        self.secondary = Some(secondary.into());
        self
    }

    /// Let a query match `keywords` too, without painting them.
    #[must_use]
    pub fn keywords(mut self, keywords: impl Into<String>) -> Self {
        self.keywords = Some(keywords.into());
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

    /// The text a query matches: label, then any [`Self::keywords`], then
    /// the secondary (so chords filter too).
    pub(super) fn filter_text(&self) -> String {
        let mut text = self.label.clone();
        for extra in [&self.keywords, &self.secondary].into_iter().flatten() {
            text.push(' ');
            text.push_str(extra);
        }
        text
    }
}
