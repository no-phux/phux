//! `switch` options — a clickable chip that opens the fleet switcher.

use std::collections::BTreeMap;

use crate::widget::{
    CellStyle, WidgetError, WidgetKindSpec, WidgetOptSpec, reject_unknown_opts, string_opt,
    style_opt,
};

const KIND: &str = "switch";

/// Doc and validation spec of the `switch` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "A clickable chip that opens the agent-fleet switcher (the \
              same overlay `prefix A` opens). Every cell of the chip, \
              padding included, is a click target. Pair it with \
              `max-cols` to surface it only on terminals too narrow for \
              the sidebar and the full tab strip.",
    options: &[
        WidgetOptSpec {
            name: "label",
            aliases: &[],
            doc: "string, default `\"agents\"` — the chip's text. Rendered \
                  with one space of padding on each side.",
        },
        WidgetOptSpec {
            name: "chip",
            aliases: &[],
            doc: "style table, default bold reverse-video — the chip's \
                  style. Reverse video by default so the affordance reads \
                  as a button on any palette.",
        },
    ],
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `switch` options.
#[derive(Debug, Clone)]
pub struct SwitchOpts {
    /// The chip's text, padded by one space each side.
    pub label: String,
    /// Style of every chip cell.
    pub chip: CellStyle,
}

impl Default for SwitchOpts {
    fn default() -> Self {
        Self {
            label: "agents".to_owned(),
            chip: CellStyle {
                bold: true,
                reverse: true,
                ..CellStyle::default()
            },
        }
    }
}

/// Validate and resolve `switch` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown or mistyped option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<SwitchOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let defaults = SwitchOpts::default();
    Ok(SwitchOpts {
        label: string_opt(KIND, opts, "label")?.unwrap_or(defaults.label),
        chip: style_opt(KIND, opts, "chip")?.unwrap_or(defaults.chip),
    })
}
