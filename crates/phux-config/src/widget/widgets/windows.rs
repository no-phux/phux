//! `windows` widget — the tmux-style tab bar.
//!
//! Renders one styled segment per window from [`WidgetContext::windows`],
//! the active one in the `active` style and the rest in `inactive`,
//! joined by `separator`. Each segment's text comes from `format` with
//! `{index}` (0-based position, the `select-window` selector) and
//! `{name}` (the editable label) substituted. The index can carry its own
//! `index` ink, and a window's agent badge precedes its name.

use std::collections::BTreeMap;

use crate::widget::{
    Cell, CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError,
    WidgetKindSpec, WidgetOptSpec, WindowInfo, display_width, reject_unknown_opts, style_opt,
};

/// Widget kind, used in error messages.
const KIND: &str = "windows";

/// Doc spec — the factory validates against this same const, so the
/// documented option surface is the enforced one (phux-i0e8.11.3).
pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The tmux-style tab bar: one segment per window, the active \
              one in the `active` style and the rest in `inactive`, joined \
              by `separator`. A window whose focused pane runs an agent \
              shows that agent's badge glyph before its name. A zoomed \
              active window gets a ` Z` marker, a \
              window waiting on a human answer a ` !` marker, a window \
              holding a retained (exited) pane a dim ` x` / ` xN` marker, \
              and every tab is a click target committing `select-window` \
              for its index — in any slot, top or bottom bar. Overflow \
              arrows select the nearest hidden window; long active labels \
              keep these arrows when space permits.",
    options: &[
        WidgetOptSpec {
            name: "active",
            aliases: &[],
            doc: "style table, default bold reverse-video — style of the \
                  active window's segment.",
        },
        WidgetOptSpec {
            name: "inactive",
            aliases: &[],
            doc: "style table, default dim — style of inactive windows' \
                  segments.",
        },
        WidgetOptSpec {
            name: "index",
            aliases: &[],
            doc: "style table, default none — ink layered over the \
                  segment's style for the `{index}` part only, so the \
                  selector can recede behind the name while keeping the \
                  tab's background.",
        },
        WidgetOptSpec {
            name: "separator",
            aliases: &[],
            doc: "string, default `\" \"` — literal text between segments.",
        },
        WidgetOptSpec {
            name: "format",
            aliases: &[],
            doc: "string, default `\"{index}:{name}\"` — per-segment \
                  template; `{index}` (0-based position, the \
                  `select-window` selector) and `{name}` (the editable \
                  label) are substituted.",
        },
    ],
};

/// `windows` (tab-bar) widget.
#[derive(Debug, Clone)]
pub struct WindowsWidget {
    /// Style applied to the active window's segment.
    pub active: CellStyle,
    /// Style applied to inactive windows' segments.
    pub inactive: CellStyle,
    /// Ink layered over the segment style for the `{index}` part.
    pub index: CellStyle,
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
            index: CellStyle::default(),
            separator: " ".to_owned(),
            format: "{index}:{name}".to_owned(),
        }
    }
}

impl WindowsWidget {
    /// The cells of window `i`'s tab, markers and hit stamps included.
    ///
    /// `format` is walked part by part rather than substituted into one
    /// string, so `{index}` can take its own ink and the badge its own
    /// colour while every cell keeps the segment's background: a tab is one
    /// bed with several inks on it, not several beds.
    fn segment(&self, i: usize, w: &WindowInfo) -> Vec<Cell> {
        let base = if w.active {
            self.active.clone()
        } else {
            self.inactive.clone()
        };
        let index_style = base.layered(&self.index);
        let mut segment = Vec::new();
        let mut rest = self.format.as_str();
        while !rest.is_empty() {
            let next = [rest.find("{index}"), rest.find("{name}")]
                .into_iter()
                .flatten()
                .min();
            let Some(at) = next else {
                push_text(&mut segment, rest, &base);
                break;
            };
            push_text(&mut segment, &rest[..at], &base);
            rest = &rest[at..];
            if let Some(after) = rest.strip_prefix("{index}") {
                push_text(&mut segment, &i.to_string(), &index_style);
                rest = after;
            } else if let Some(after) = rest.strip_prefix("{name}") {
                push_name(&mut segment, w, &base);
                rest = after;
            }
        }
        // phux-foz.12: stamp every cell of the segment (markers
        // included) as a hit target for window `i`, so a click on the
        // tab commits `select-window { index = i }`. Separator cells
        // stay inert.
        for cell in &mut segment {
            cell.hit = Some(CellHit::Window(i));
        }
        segment
    }
}

/// The `{name}` part of a tab: badge, label, then the state markers.
fn push_name(segment: &mut Vec<Cell>, w: &WindowInfo, base: &CellStyle) {
    if let Some(badge) = &w.badge {
        push_text(segment, &badge.glyph, &base.layered(&badge.style));
        push_text(segment, " ", base);
    }
    let mut text = w.name.clone();
    // phux-x2hm: a zoomed active window gets tmux's `Z` marker.
    if w.zoomed {
        text.push_str(" Z");
    }
    // phux-foz.1: a window holding a pane that asked for a human
    // answer (ADR-0035) gets a `!` marker so it is findable from
    // any window. Plain ASCII, matching the `Z` marker convention.
    if w.attention {
        text.push_str(" !");
    }
    // ADR-0124 / phux-fpgl.33: a window holding a retained (exited)
    // pane gets a compact `x` marker plus the exit status, so it is
    // findable without focusing that pane.
    if let Some(marker) = w.exited_marker() {
        text.push_str(&marker);
    }
    push_text(segment, &text, base);
}

/// Append `text` to `cells` in `style` (`None` for an all-default style).
fn push_text(cells: &mut Vec<Cell>, text: &str, style: &CellStyle) {
    if text.is_empty() {
        return;
    }
    let style = (!style.is_plain()).then(|| style.clone());
    cells.extend(WidgetCells::from_styled(text, style).cells);
}

impl WindowsWidget {
    /// The separator cells placed between two tabs (empty when the
    /// configured separator is).
    fn separator_cells(&self) -> Vec<Cell> {
        if self.separator.is_empty() {
            Vec::new()
        } else {
            WidgetCells::from_styled(&self.separator, None).cells
        }
    }

    /// A one-cell navigation target for the nearest hidden tab.
    fn overflow_mark(&self, glyph: char, target: usize) -> Vec<Cell> {
        let style = self.inactive.clone();
        let style = if style.is_plain() { None } else { Some(style) };
        let mut cells = WidgetCells::from_styled(&glyph.to_string(), style).cells;
        for cell in &mut cells {
            cell.hit = Some(CellHit::Window(target));
        }
        cells
    }

    /// Width of the strip that shows tabs `lo..=hi` out of `total`,
    /// including separators and whichever overflow marks that range
    /// implies.
    fn windowed_width(&self, seg_widths: &[usize], lo: usize, hi: usize) -> usize {
        // CELLS, not chars, and for the same reason `separator_cells`
        // goes through `WidgetCells::from_styled`: the separator is
        // user-configurable text. `separator = "\u{ff5c}"` is one char and
        // two columns, so counting chars under-reported every gap by a
        // column, `render_within` handed back more cells than its budget,
        // and the `debug_assert` below fired in debug and test builds.
        // The two measurements have to be the same function or they will
        // drift again.
        let sep = display_width(&self.separator);
        let tabs: usize = seg_widths[lo..=hi].iter().sum();
        let seps = sep.saturating_mul(hi - lo);
        let marks = usize::from(lo > 0) + usize::from(hi + 1 < seg_widths.len());
        tabs + seps + marks
    }

    /// Grow around the active tab, preferring the next tab on ties.
    fn visible_range(&self, widths: &[usize], active: usize, budget: usize) -> (usize, usize) {
        let (mut lo, mut hi) = (active, active);
        loop {
            let grew_hi =
                hi + 1 < widths.len() && self.windowed_width(widths, lo, hi + 1) <= budget;
            if grew_hi {
                hi += 1;
            }
            let grew_lo = lo > 0 && self.windowed_width(widths, lo - 1, hi) <= budget;
            if grew_lo {
                lo -= 1;
            }
            if !grew_hi && !grew_lo {
                return (lo, hi);
            }
        }
    }

    /// Consume the measured segments, adding navigation at the visible edges.
    fn render_range(&self, segments: Vec<Vec<Cell>>, lo: usize, hi: usize) -> WidgetCells {
        let total = segments.len();
        let sep = self.separator_cells();
        let mut cells = Vec::new();
        if lo > 0 {
            cells.extend(self.overflow_mark('\u{2039}', lo - 1));
        }
        for (n, segment) in segments.into_iter().skip(lo).take(hi - lo + 1).enumerate() {
            if n > 0 {
                cells.extend(sep.iter().cloned());
            }
            cells.extend(segment);
        }
        if hi + 1 < total {
            cells.extend(self.overflow_mark('\u{203a}', hi + 1));
        }
        WidgetCells { cells }
    }

    /// Keep navigation beside a clipped active label whenever there is room
    /// for at least its index and ellipsis. Tiny grids prioritize the label.
    fn clipped_anchor(
        &self,
        mut segments: Vec<Vec<Cell>>,
        active: usize,
        budget: usize,
    ) -> WidgetCells {
        let marks = usize::from(active > 0) + usize::from(active + 1 < segments.len());
        let navigable = budget >= marks + 2;
        let mut anchor = WidgetCells {
            cells: std::mem::take(&mut segments[active]),
        };
        anchor.clip(budget - if navigable { marks } else { 0 });
        if !navigable {
            return anchor;
        }
        segments[active] = anchor.cells;
        self.render_range(segments, active, active)
    }
}

impl StatusWidget for WindowsWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let sep = self.separator_cells();
        let mut cells: Vec<Cell> = Vec::new();
        for (i, w) in ctx.windows.iter().enumerate() {
            if i > 0 {
                cells.extend(sep.iter().cloned());
            }
            cells.extend(self.segment(i, w));
        }
        WidgetCells { cells }
    }

    /// Drop whole tabs, never parts of one.
    ///
    /// A tab bar clipped mid-label (`0:alpha 1:`) is actively misleading:
    /// it reads as a window named `1:`, hides that windows 2 and 3 exist,
    /// and leaves a click target pointing at a window whose name you
    /// cannot see. So instead of cutting the strip we choose *which tabs
    /// to show*: the active one always, then its neighbours outward while
    /// they fit, with a `‹` / `›` mark standing in for whatever is hidden
    /// on that side. The active tab is the anchor because it is the one
    /// piece of information the bar exists to convey — where you are.
    ///
    /// Below the width of even the active tab plus its marks, the tab's
    /// own label is clipped (with the shared ellipsis): the leading
    /// `{index}` survives longest, which is exactly the part you need to
    /// type `prefix <n>` and get somewhere.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        if budget == 0 || ctx.windows.is_empty() {
            return WidgetCells { cells: Vec::new() };
        }

        let segments: Vec<Vec<Cell>> = ctx
            .windows
            .iter()
            .enumerate()
            .map(|(i, w)| self.segment(i, w))
            .collect();
        let widths: Vec<usize> = segments.iter().map(Vec::len).collect();
        let last = segments.len() - 1;

        // Fits whole? Nothing to decide.
        if self.windowed_width(&widths, 0, last) <= budget {
            return self.render_range(segments, 0, last);
        }

        let active = ctx.windows.iter().position(|w| w.active).unwrap_or(0);

        // Not even the anchor fits: clip the active tab itself, keeping
        // its leading index legible for as long as possible.
        if self.windowed_width(&widths, active, active) > budget {
            return self.clipped_anchor(segments, active, budget);
        }

        // Grow outward from the anchor, alternating sides so the visible
        // run stays centred on where you are. Preferring `hi` on ties
        // means the *next* window — the one `prefix n` moves to — is the
        // first neighbour you get back as the terminal widens.
        let (lo, hi) = self.visible_range(&widths, active, budget);
        let result = self.render_range(segments, lo, hi);
        debug_assert!(result.len() <= budget, "windows widget overran its budget");
        result
    }

    // No `poll_interval` — the tab bar repaints when the layout changes,
    // which the client drives via the status-bar repaint path.
}

/// Factory: builds a [`WindowsWidget`] from a TOML `opts` map.
///
/// Accepted keys (all optional; omitted keys keep the default preset):
/// - `active` / `inactive` / `index` (inline table) — a [`CellStyle`]:
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
    reject_unknown_opts(&SPEC, opts)?;
    let defaults = WindowsWidget::default();
    let active = style_opt(KIND, opts, "active")?.unwrap_or(defaults.active);
    let inactive = style_opt(KIND, opts, "inactive")?.unwrap_or(defaults.inactive);
    let index = style_opt(KIND, opts, "index")?.unwrap_or(defaults.index);
    let separator = string_opt(opts, "separator")?.unwrap_or(defaults.separator);
    let format = string_opt(opts, "format")?.unwrap_or(defaults.format);
    Ok(Box::new(WindowsWidget {
        active,
        inactive,
        index,
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
