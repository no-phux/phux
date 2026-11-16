//! `windows` options — the tmux-style tab bar.

use std::collections::BTreeMap;

use crate::widget::{
    CellStyle, WidgetError, WidgetKindSpec, WidgetOptSpec, reject_unknown_opts, string_opt,
    style_opt,
};

const KIND: &str = "windows";

/// Doc and validation spec of the `windows` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
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
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `windows` options.
#[derive(Debug, Clone)]
pub struct WindowsOpts {
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

impl Default for WindowsOpts {
    fn default() -> Self {
        Self {
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

/// Validate and resolve `windows` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown or mistyped option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<WindowsOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let defaults = WindowsOpts::default();
    Ok(WindowsOpts {
        active: style_opt(KIND, opts, "active")?.unwrap_or(defaults.active),
        inactive: style_opt(KIND, opts, "inactive")?.unwrap_or(defaults.inactive),
        index: style_opt(KIND, opts, "index")?.unwrap_or(defaults.index),
        separator: string_opt(KIND, opts, "separator")?.unwrap_or(defaults.separator),
        format: string_opt(KIND, opts, "format")?.unwrap_or(defaults.format),
    })
}
