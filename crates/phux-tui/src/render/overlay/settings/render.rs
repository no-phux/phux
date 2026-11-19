//! The page's own paint: rows, the detail panel, the query line.

use super::{
    Applies, BADGE_COLS, ChromeBreakpoints, Color, Control, DETAIL_ROWS, DETAIL_ROWS_COMPACT,
    FromStr, LayerSource, Line, Modifier, Rect, RowDecl, SettingKind, SettingSpec, SettingsOverlay,
    Span, Status, Style, centered_panel, clip_left, clip_text, display_width, render_value,
    wrap_words,
};

impl SettingsOverlay {
    // ---- rendering ------------------------------------------------------

    /// The modal rect: 70% of the viewport, at least 60x16, full-bleed on a
    /// starved axis. Smaller than a page, larger than a picker.
    pub(super) fn modal_area(outer: Rect, bp: ChromeBreakpoints) -> Rect {
        centered_panel(outer, 7, 60, 16, bp)
    }

    /// Detail-panel rows for this viewport.
    pub(super) const fn detail_rows(outer: Rect, bp: ChromeBreakpoints) -> u16 {
        if bp.is_row_starved(outer.height) {
            DETAIL_ROWS_COMPACT
        } else {
            DETAIL_ROWS
        }
    }

    /// The origin badge for `key`.
    pub(super) fn badge(&self, key: &str) -> (String, Style) {
        match self.origin_at(key) {
            None => ("default".to_owned(), Style::default().fg(self.theme.dim)),
            Some(LayerSource::User(_)) => {
                ("you".to_owned(), Style::default().fg(self.theme.accent))
            }
            Some(LayerSource::Extended(path)) => {
                let name = path
                    .file_stem()
                    .map_or_else(|| "layer".to_owned(), |s| s.to_string_lossy().into_owned());
                (
                    clip_text(&name, BADGE_COLS - 1),
                    Style::default().fg(self.theme.section_header),
                )
            }
            Some(LayerSource::Defaults) => {
                ("default".to_owned(), Style::default().fg(self.theme.dim))
            }
        }
    }

    /// A row's value as spans: a swatch for a color, `unset` dimmed when
    /// there is no value.
    pub(super) fn value_spans(&self, spec: &SettingSpec, selected: bool) -> Vec<Span<'static>> {
        let base_fg = if selected {
            self.theme.selection_fg
        } else {
            self.theme.text
        };
        let value = self.effective(spec);
        let Some(value) = value else {
            return vec![Span::styled(
                "unset".to_owned(),
                Style::default()
                    .fg(if selected { base_fg } else { self.theme.dim })
                    .add_modifier(Modifier::ITALIC),
            )];
        };
        let text = render_value(Some(&value));
        let mut spans = Vec::new();
        if spec.kind == SettingKind::Color
            && let Some(color) = value.as_str().and_then(|s| Color::from_str(s).ok())
        {
            spans.push(Span::styled(
                "\u{2588}\u{2588} ",
                Style::default().fg(color),
            ));
        }
        spans.push(Span::styled(text, Style::default().fg(base_fg)));
        spans
    }

    /// One list row laid out to exactly `width` cells: marker, key, value
    /// (right-aligned against the badge), badge. A row with a control
    /// leaves its value cell blank for the control and says where it is.
    pub(super) fn row_decl(
        &self,
        spec: &'static SettingSpec,
        selected: bool,
        width: usize,
        full_key: bool,
    ) -> RowDecl {
        let overridden = self.origin_at(spec.key).is_some();
        let colors = self.row_colors(selected);
        let marker = if overridden { "\u{2022} " } else { "  " };
        let key = if full_key { spec.key } else { spec.leaf() };
        let show_badge = width >= 40;
        let badge_w = if show_badge { BADGE_COLS } else { 0 };
        let control = self.control_for(spec);
        let value_spans = match control.as_ref().map(Control::slot) {
            None => self.value_spans(spec, selected),
            Some(Some(slot)) => vec![Span::raw(" ".repeat(slot))],
            // The editor takes what a key of at most a third leaves.
            Some(None) => {
                let key_w = display_width(key).min(width / 3);
                let slot = width.saturating_sub(2 + badge_w + 1 + key_w);
                vec![Span::raw(" ".repeat(slot))]
            }
        };
        let value_w = spans_width(&value_spans);
        // The key takes what the value and badge leave, and at least a third
        // of the row; the value yields next; the badge is dropped whole on a
        // narrow row.
        let key_budget = width
            .saturating_sub(2 + badge_w + value_w + 1)
            .max(width / 3)
            .min(width.saturating_sub(2));
        let key_text = clip_text(key, key_budget);
        let value_budget = width.saturating_sub(2 + badge_w + display_width(&key_text) + 1);
        let value_spans = clip_value_spans(value_spans, value_w, value_budget, colors.text);
        let value_w = spans_width(&value_spans);
        let pad = width.saturating_sub(2 + display_width(&key_text) + value_w + badge_w);
        let value_x = 2 + display_width(&key_text) + pad;
        let mut spans = vec![
            Span::styled(marker.to_owned(), colors.row.fg(colors.marker)),
            Span::styled(key_text, colors.key(overridden)),
            Span::styled(" ".repeat(pad), colors.row),
        ];
        spans.extend(
            value_spans
                .into_iter()
                .map(|span| Span::styled(span.content, span.style.patch(colors.row))),
        );
        if show_badge {
            let (badge, badge_style) = self.badge(spec.key);
            let badge_style = if selected { colors.row } else { badge_style };
            spans.extend(badge_spans(badge, badge_style, colors.row));
        }
        let cell = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        RowDecl {
            key: spec.key,
            line: Line::from(spans),
            control: control.map(|c| (c, cell(value_x), cell(value_w))),
        }
    }

    /// The palette of one list row: the selection colors when selected.
    pub(super) fn row_colors(&self, selected: bool) -> RowColors {
        if !selected {
            return RowColors {
                row: Style::default(),
                text: self.theme.text,
                marker: self.theme.accent,
            };
        }
        let fg = self.theme.selection_fg;
        RowColors {
            row: self.theme.on_selection(Style::default().fg(fg)),
            text: fg,
            marker: fg,
        }
    }

    /// The detail panel: what the selected setting is, its default, its
    /// domain, when it applies, and the last outcome.
    pub(super) fn detail_lines(
        &self,
        spec: Option<&SettingSpec>,
        width: usize,
        rows: u16,
    ) -> Vec<Line<'static>> {
        let dim = Style::default().fg(self.theme.dim);
        let text = Style::default().fg(self.theme.text);
        if let Err(err) = &self.snapshot {
            return vec![
                Line::from(Span::styled(
                    clip_text(&format!("config does not load: {err}"), width),
                    Style::default().fg(self.theme.error),
                )),
                Line::from(Span::styled(
                    clip_text(
                        "Fix the file, then reopen this page (run: phux config check)",
                        width,
                    ),
                    dim,
                )),
            ];
        }
        let Some(spec) = spec else {
            return vec![Line::from(Span::styled(
                "(no setting matches)".to_owned(),
                dim,
            ))];
        };
        let mut lines = vec![
            // Line 1: key, kind.
            Line::from(vec![
                Span::styled(
                    spec.key.to_owned(),
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", kind_label(spec.kind)), dim),
            ]),
            // Line 2: summary.
            Line::from(Span::styled(clip_text(spec.summary, width), text)),
        ];
        if rows <= DETAIL_ROWS_COMPACT {
            // The compact panel: the outcome line, when there is one, wins
            // over the summary.
            if let Some(status) = &self.status {
                lines.pop();
                lines.push(self.status_line(status, width));
            }
            return lines;
        }
        // Lines 3..: detail, wrapped, as many as fit above the two footer facts.
        let detail_budget = usize::from(rows).saturating_sub(4);
        for line in wrap_words(spec.detail, width)
            .into_iter()
            .take(detail_budget)
        {
            lines.push(Line::from(Span::styled(line, dim)));
        }
        lines.push(self.facts_line(spec, width));
        if let Some(line) = self.outcome_line(spec, width) {
            lines.push(line);
        }
        lines
    }

    /// `default X  ·  origin  ·  applies when`, its tail clipped with an
    /// ellipsis when `width` cannot hold it all.
    pub(super) fn facts_line(&self, spec: &SettingSpec, width: usize) -> Line<'static> {
        let dim = Style::default().fg(self.theme.dim);
        let text = Style::default().fg(self.theme.text);
        let default = render_value(self.default_at(spec).as_ref());
        let origin = match self.origin_at(spec.key) {
            None | Some(LayerSource::Defaults) => "shipped default".to_owned(),
            Some(LayerSource::User(_)) => "set in your config".to_owned(),
            Some(LayerSource::Extended(path)) => format!("set by layer {}", path.display()),
        };
        let mut facts = vec![Span::styled("default ".to_owned(), dim)];
        if spec.kind == SettingKind::Color
            && let Ok(color) = Color::from_str(&default)
        {
            facts.push(Span::styled(
                "\u{2588} ".to_owned(),
                Style::default().fg(color),
            ));
        }
        facts.push(Span::styled(default, text));
        let tail = format!(
            "  \u{b7}  {origin}  \u{b7}  applies {}",
            applies_label(spec.applies)
        );
        let used: usize = facts.iter().map(|span| display_width(&span.content)).sum();
        facts.push(Span::styled(
            clip_text(&tail, width.saturating_sub(used)),
            dim,
        ));
        Line::from(facts)
    }

    /// The last outcome, or a live color preview while a color is edited.
    pub(super) fn outcome_line(&self, spec: &SettingSpec, width: usize) -> Option<Line<'static>> {
        let dim = Style::default().fg(self.theme.dim);
        if let Some(status) = &self.status {
            return Some(self.status_line(status, width));
        }
        self.editor?;
        if spec.kind != SettingKind::Color {
            return None;
        }
        Some(
            Color::from_str(self.model.input.value().trim()).map_or_else(
                |_| {
                    Line::from(Span::styled(
                        "not a color yet: use a name, #rrggbb, or an index 0-255".to_owned(),
                        dim,
                    ))
                },
                |color| {
                    Line::from(vec![
                        Span::styled("preview ".to_owned(), dim),
                        Span::styled(
                            "\u{2588}\u{2588}\u{2588}\u{2588}".to_owned(),
                            Style::default().fg(color),
                        ),
                    ])
                },
            ),
        )
    }

    pub(super) fn status_line(&self, status: &Status, width: usize) -> Line<'static> {
        let (text, style) = match status {
            Status::Saved(t) => (t, Style::default().fg(self.theme.chord)),
            Status::Refused(t) => (t, Style::default().fg(self.theme.error)),
            Status::Note(t) => (t, Style::default().fg(self.theme.dim)),
        };
        Line::from(Span::styled(clip_text(text, width), style))
    }

    /// The header row: the filter prompt on the left, the file on the
    /// right, cut from the left when long so its name stays.
    pub(super) fn header_row(&self, width: usize) -> Line<'static> {
        let query_text = self.query.clone();
        let prompt_w = 2 + display_width(&query_text) + 1;
        let path = clip_left(&self.display_path, width.saturating_sub(prompt_w + 2));
        let pad = width.saturating_sub(prompt_w + display_width(&path));
        Line::from(vec![
            Span::styled("> ".to_owned(), Style::default().fg(self.theme.dim)),
            Span::styled(
                query_text,
                if self.query.is_empty() {
                    Style::default().fg(self.theme.dim)
                } else {
                    Style::default().fg(self.theme.text)
                },
            ),
            Span::styled(
                " ".to_owned(),
                if self.editor.is_none() {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                },
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled(path, Style::default().fg(self.theme.dim)),
        ])
    }

    /// The bottom-left border text: the selection's place in the rows
    /// (`3 of 12`), or the section's row count with nothing selected.
    pub(super) fn footer_status(&self, rows: usize) -> String {
        if rows == 0 {
            return String::new();
        }
        format!("{} of {rows}", self.selected.min(rows - 1) + 1)
    }
}

/// A short kind description for the detail panel's first line.
pub(super) fn kind_label(kind: SettingKind) -> String {
    match kind {
        SettingKind::Bool => "bool".to_owned(),
        SettingKind::Integer { min, max } => format!("integer {min}..{max}"),
        SettingKind::OptionalInteger { min, max } => format!("integer {min}..{max}, or unset"),
        SettingKind::Text => "text".to_owned(),
        SettingKind::OptionalText => "text, or unset".to_owned(),
        SettingKind::Choice(variants) => variants.join(" | "),
        SettingKind::OptionalBool => "unset | true | false".to_owned(),
        SettingKind::Argv => "command words, or unset".to_owned(),
        SettingKind::Chord => "chord".to_owned(),
        SettingKind::Color => "color: name, #rrggbb, or 0-255".to_owned(),
    }
}

/// When a change lands, in the page's words. `NextSpawn` is spelled as the
/// server behaves: `[defaults]` and `[voice]` are read once at server start.
/// The palette of one settings list row.
#[derive(Debug, Clone, Copy)]
pub(super) struct RowColors {
    /// The row's base style (background and default foreground).
    row: Style,
    /// Key and value text.
    text: Color,
    /// The overridden-key marker.
    marker: Color,
}

impl RowColors {
    /// The key's style: bold when the key is overridden.
    pub(super) const fn key(self, overridden: bool) -> Style {
        let modifier = if overridden {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        self.row.fg(self.text).add_modifier(modifier)
    }
}

/// Total display width of a run of spans.
pub(super) fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

/// The value spans unchanged when they fit `budget`; otherwise one span of
/// their joined text clipped to `budget`, in `fg`.
pub(super) fn clip_value_spans(
    spans: Vec<Span<'static>>,
    width: usize,
    budget: usize,
    fg: Color,
) -> Vec<Span<'static>> {
    if width <= budget {
        return spans;
    }
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    vec![Span::styled(
        clip_text(&joined, budget),
        Style::default().fg(fg),
    )]
}

/// The badge right-aligned in its [`BADGE_COLS`] column.
pub(super) fn badge_spans(
    badge: String,
    badge_style: Style,
    row_style: Style,
) -> [Span<'static>; 2] {
    let badge_pad = BADGE_COLS.saturating_sub(display_width(&badge));
    [
        Span::styled(" ".repeat(badge_pad), row_style),
        Span::styled(badge, badge_style),
    ]
}

pub(super) const fn applies_label(applies: Applies) -> &'static str {
    match applies {
        Applies::LiveReload => "now (reloaded)",
        Applies::NextAttach => "next attach",
        Applies::NextSpawn => "next server start",
    }
}
