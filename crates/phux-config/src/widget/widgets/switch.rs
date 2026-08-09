//! `switch` widget — a clickable chip that opens the fleet switcher.
//!
//! Every navigation surface phux has is a chord: `prefix A` for the
//! fleet, `prefix w` for windows, `prefix s` for sessions. That is the
//! right primary interface and the wrong *only* interface, because a
//! chord is invisible. On a narrow terminal, where the sidebar is gone
//! and the tab strip has collapsed to the active tab, there is nothing
//! left on screen that says "there are other things running" — which is
//! exactly the moment the answer matters most.
//!
//! This widget is that missing signpost: a visible, pointer-sized target
//! that opens the same fleet overlay `prefix A` does. It is one widget in
//! the ordinary lineup, so it is placed, styled, and gated by width like
//! any other — see the shipped `[status]` block, which shows it only at
//! the widths where the rest of the chrome has stopped answering the
//! question.

use std::collections::BTreeMap;

use crate::widget::{
    CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec,
    WidgetOptSpec, reject_unknown_opts, style_opt,
};

const KIND: &str = "switch";

/// Doc spec — the factory validates against this same const, so the
/// documented option surface is the enforced one (phux-i0e8.11.3).
pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
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
            doc: "string, default `\"switch\"` — the chip's text. Rendered \
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
};

/// `switch` widget.
#[derive(Debug, Clone)]
pub struct SwitchWidget {
    /// The chip's text, padded by one space on each side when rendered.
    pub label: String,
    /// Style applied to every cell of the chip.
    pub chip: CellStyle,
}

impl Default for SwitchWidget {
    fn default() -> Self {
        Self {
            label: "switch".to_owned(),
            // Theme-agnostic and unmistakably interactive: reverse video
            // reads as a raised button whatever the terminal palette is,
            // which a foreground color alone does not.
            chip: CellStyle {
                bold: true,
                reverse: true,
                ..CellStyle::default()
            },
        }
    }
}

impl SwitchWidget {
    /// The chip's cells: the label with one space of padding each side,
    /// every cell stamped as a [`CellHit::Switch`] target.
    ///
    /// The padding is not decoration. A one-column-wider target on each
    /// side is the difference between a chip you can hit with a trackpad
    /// and one you have to aim at, and stamping the padding is what makes
    /// the visible chip and the clickable chip the same rectangle.
    fn cells(&self) -> Vec<crate::widget::Cell> {
        let style = self.chip.clone();
        let style = if style.is_plain() { None } else { Some(style) };
        let mut cells = WidgetCells::from_styled(&format!(" {} ", self.label), style).cells;
        for cell in &mut cells {
            cell.hit = Some(CellHit::Switch);
        }
        cells
    }
}

impl StatusWidget for SwitchWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        WidgetCells {
            cells: self.cells(),
        }
    }

    /// A chip is a target, not a text run: a clipped one (`swi…`) is a
    /// smaller target that still claims the same columns, which is worse
    /// than not offering the affordance at all. So it renders whole or
    /// not at all.
    fn render_within(&self, _ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        let cells = self.cells();
        if cells.len() <= budget {
            WidgetCells { cells }
        } else {
            WidgetCells { cells: Vec::new() }
        }
    }
}

/// Factory: builds a [`SwitchWidget`] from a TOML `opts` map.
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option, a `label`
/// that is not a string, or a `chip` table with an unknown field.
pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let defaults = SwitchWidget::default();
    let label = match opts.get("label") {
        None => defaults.label,
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`label` must be a string, got {}", other.type_str()),
            });
        }
    };
    let chip = style_opt(KIND, opts, "chip")?.unwrap_or(defaults.chip);
    Ok(Box::new(SwitchWidget { label, chip }))
}
