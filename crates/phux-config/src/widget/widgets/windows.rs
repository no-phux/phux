//! `windows` widget — the tmux-style tab bar.
//!
//! Renders one styled segment per window from [`WidgetContext::windows`],
//! the active one in the `active` style and the rest in `inactive`,
//! joined by `separator`. Each segment's text comes from `format` with
//! `{index}` (0-based position, the `select-window` selector) and
//! `{name}` (the editable label) substituted.

use std::collections::BTreeMap;

use crate::widget::{
    Cell, CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError,
    reject_unknown_opts, style_opt,
};

/// Widget kind, used in error messages.
const KIND: &str = "windows";

/// `windows` (tab-bar) widget.
#[derive(Debug, Clone)]
pub struct WindowsWidget {
    /// Style applied to the active window's segment.
    pub active: CellStyle,
    /// Style applied to inactive windows' segments.
    pub inactive: CellStyle,
    /// Literal text placed between segments.
    pub separator: String,
    /// Per-segment template; `{index}` and `{name}` are substituted.
    pub format: String,
}

impl Default for WindowsWidget {
    fn default() -> Self {
        Self {
            // Theme-agnostic, eye-catching default: the active tab is
            // bold reverse-video; inactive tabs are dimmed.
            active: CellStyle {
                bold: true,
                reverse: true,
                ..CellStyle::default()
            },
            inactive: CellStyle {
                dim: true,
                ..CellStyle::default()
            },
            separator: " ".to_owned(),
            format: "{index}:{name}".to_owned(),
        }
    }
}

impl WindowsWidget {
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "`{index}`/`{name}` are this widget's own template placeholders, not std format args"
    )]
    fn segment_text(&self, index: usize, name: &str) -> String {
        self.format
            .replace("{index}", &index.to_string())
            .replace("{name}", name)
    }
}

impl StatusWidget for WindowsWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let mut cells: Vec<Cell> = Vec::new();
        for (i, w) in ctx.windows.iter().enumerate() {
            if i > 0 && !self.separator.is_empty() {
                cells.extend(WidgetCells::from_styled(&self.separator, None).cells);
            }
            // phux-x2hm: a zoomed active window gets tmux's `Z` marker.
            let mut text = self.segment_text(i, &w.name);
            if w.zoomed {
                text.push_str(" Z");
            }
            // phux-foz.1: a window holding a pane that asked for a human
            // answer (ADR-0035) gets a `!` marker so it is findable from
            // any window. Plain ASCII, matching the `Z` marker convention.
            if w.attention {
                text.push_str(" !");
            }
            let style = if w.active {
                self.active.clone()
            } else {
                self.inactive.clone()
            };
            let style = if style.is_plain() { None } else { Some(style) };
            // phux-foz.12: stamp every cell of the segment (markers
            // included) as a hit target for window `i`, so a click on the
            // tab commits `select-window { index = i }`. Separator cells
            // above stay inert.
            let mut segment = WidgetCells::from_styled(&text, style).cells;
            for cell in &mut segment {
                cell.hit = Some(CellHit::Window(i));
            }
            cells.extend(segment);
        }
        WidgetCells { cells }
    }

    // No `poll_interval` — the tab bar repaints when the layout changes,
    // which the client drives via the status-bar repaint path.
}

/// Factory: builds a [`WindowsWidget`] from a TOML `opts` map.
///
/// Accepted keys (all optional; omitted keys keep the default preset):
/// - `active` / `inactive` (inline table) — a [`CellStyle`]:
///   `fg`/`bg` (color strings), `bold`/`dim`/`italic`/`underline`/`reverse`
///   (bools).
/// - `separator` (string) — text between segments (default `" "`).
/// - `format` (string) — segment template with `{index}`/`{name}`
///   (default `"{index}:{name}"`).
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option, a value
/// of the wrong type, or a style table with an unknown field.
pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(KIND, opts, &["active", "inactive", "separator", "format"])?;
    let defaults = WindowsWidget::default();
    let active = style_opt(KIND, opts, "active")?.unwrap_or(defaults.active);
    let inactive = style_opt(KIND, opts, "inactive")?.unwrap_or(defaults.inactive);
    let separator = string_opt(opts, "separator")?.unwrap_or(defaults.separator);
    let format = string_opt(opts, "format")?.unwrap_or(defaults.format);
    Ok(Box::new(WindowsWidget {
        active,
        inactive,
        separator,
        format,
    }))
}

/// Parse an optional string option.
fn string_opt(
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
) -> Result<Option<String>, WidgetError> {
    match opts.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(WidgetError::InvalidOption {
            kind: KIND.to_owned(),
            message: format!("`{key}` must be a string, got {}", other.type_str()),
        }),
    }
}
