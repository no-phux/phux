//! `button` widget — a clickable label that runs one TUI action: the `+`
//! after the tab strip, a `+ session` chip, or any argument-free action a
//! user wants one click away.

use std::collections::BTreeMap;

use crate::widget::{
    CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec,
    WidgetOptSpec, reject_unknown_opts, string_opt, style_opt,
};

const KIND: &str = "button";

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "A clickable label that runs one argument-free TUI action \
              (`new-window`, `new-session`, `session-picker`, ...) through \
              the ordinary action dispatcher. Every cell, padding included, \
              is the click target; the button shows whole or not at all.",
    options: &[
        WidgetOptSpec {
            name: "action",
            aliases: &[],
            doc: "string, required — the action a click runs; any name \
                  from the action vocabulary that needs no arguments.",
        },
        WidgetOptSpec {
            name: "label",
            aliases: &[],
            doc: "string, default `\"+\"` — the button's text, rendered \
                  with one space of padding on each side.",
        },
        WidgetOptSpec {
            name: "style",
            aliases: &[],
            doc: "style table, default bold — the button's style.",
        },
    ],
};

/// `button` widget.
#[derive(Debug, Clone)]
pub struct ButtonWidget {
    /// The action a click runs; a canonical vocabulary name.
    pub action: &'static str,
    /// The text, padded by one space each side.
    pub label: String,
    /// Style of every button cell.
    pub style: CellStyle,
}

impl StatusWidget for ButtonWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        let style = (!self.style.is_plain()).then(|| self.style.clone());
        let mut cells = WidgetCells::from_styled(&format!(" {} ", self.label), style);
        for cell in &mut cells.cells {
            cell.hit = Some(CellHit::Action(self.action));
        }
        cells
    }

    /// Whole or not at all: a clipped button is a smaller target claiming
    /// the same columns.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        let cells = self.render(ctx);
        if cells.len() <= budget {
            cells
        } else {
            WidgetCells { cells: Vec::new() }
        }
    }
}

/// Actions a bare click cannot run: each needs an argument naming its target.
const NEEDS_ARGUMENTS: &[&str] = &[
    "select-window",
    "focus-direction",
    "resize-pane",
    "focus-pane",
    "switch-session",
    "switch-host",
    "toggle-project-group",
    "signal-terminal",
    "set-pane",
    "plugin-action",
    "plugin-pane",
    "move-window",
];

/// The canonical `&'static` spelling of `name`, when a click may run it.
fn clickable_action(name: &str) -> Option<&'static str> {
    crate::vocab::ACTION_NAMES
        .iter()
        .copied()
        .find(|known| *known == name)
        .filter(|known| !NEEDS_ARGUMENTS.contains(known))
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let invalid = |message: String| WidgetError::InvalidOption {
        kind: KIND.to_owned(),
        message,
    };
    let name = string_opt(KIND, opts, "action")?
        .ok_or_else(|| invalid("`action` is required".to_owned()))?;
    let action = clickable_action(&name).ok_or_else(|| {
        invalid(format!(
            "`action = \"{name}\"` is not an action a click can run (unknown, or it needs arguments)"
        ))
    })?;
    Ok(Box::new(ButtonWidget {
        action,
        label: string_opt(KIND, opts, "label")?.unwrap_or_else(|| "+".to_owned()),
        style: style_opt(KIND, opts, "style")?.unwrap_or_else(|| CellStyle {
            bold: true,
            ..CellStyle::default()
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> WidgetContext<'static> {
        WidgetContext::new(std::time::SystemTime::UNIX_EPOCH, "", "", &[])
    }

    fn opts(pairs: &[(&str, &str)]) -> BTreeMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), toml::Value::String((*v).to_owned())))
            .collect()
    }

    /// Every padded cell is the action's click target.
    #[test]
    fn renders_a_padded_label_stamped_with_its_action() {
        let widget = ButtonWidget {
            action: "new-window",
            label: "+".to_owned(),
            style: CellStyle::default(),
        };
        let cells = widget.render(&ctx()).cells;
        assert_eq!(cells.len(), 3);
        assert!(
            cells
                .iter()
                .all(|cell| cell.hit == Some(CellHit::Action("new-window")))
        );
        assert!(widget.render_within(&ctx(), 2).cells.is_empty());
    }

    /// The factory takes a bare vocabulary action and refuses an unknown one
    /// or one that needs a target.
    #[test]
    fn factory_accepts_only_bare_vocabulary_actions() {
        assert!(factory(&opts(&[("action", "new-session"), ("label", "+ session")])).is_ok());
        assert!(factory(&opts(&[("action", "nope")])).is_err());
        assert!(factory(&opts(&[("action", "select-window")])).is_err());
        assert!(factory(&opts(&[])).is_err());
    }
}
