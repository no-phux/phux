//! `button` options — a clickable label that runs one TUI action.

use std::collections::BTreeMap;

use crate::widget::{
    CellStyle, WidgetError, WidgetKindSpec, WidgetOptSpec, invalid, reject_unknown_opts,
    string_opt, style_opt,
};

const KIND: &str = "button";

/// Doc and validation spec of the `button` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
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
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `button` options.
#[derive(Debug, Clone)]
pub struct ButtonOpts {
    /// The action a click runs; a canonical vocabulary name.
    pub action: &'static str,
    /// The text, padded by one space each side.
    pub label: String,
    /// Style of every button cell.
    pub style: CellStyle,
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

/// Validate and resolve `button` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown option, a missing `action`,
/// an action a bare click cannot run, or a mistyped option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<ButtonOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let name = string_opt(KIND, opts, "action")?
        .ok_or_else(|| invalid(KIND, "`action` is required".to_owned()))?;
    let action = clickable_action(&name).ok_or_else(|| {
        invalid(
            KIND,
            format!(
                "`action = \"{name}\"` is not an action a click can run (unknown, or it needs \
                 arguments)"
            ),
        )
    })?;
    Ok(ButtonOpts {
        action,
        label: string_opt(KIND, opts, "label")?.unwrap_or_else(|| "+".to_owned()),
        style: style_opt(KIND, opts, "style")?.unwrap_or_else(|| CellStyle {
            bold: true,
            ..CellStyle::default()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(pairs: &[(&str, &str)]) -> BTreeMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), toml::Value::String((*v).to_owned())))
            .collect()
    }

    /// A bare vocabulary action is accepted; an unknown one or one that
    /// needs a target is refused.
    #[test]
    fn accepts_only_bare_vocabulary_actions() {
        assert!(parse(&opts(&[("action", "new-session"), ("label", "+ session")])).is_ok());
        assert!(parse(&opts(&[("action", "nope")])).is_err());
        assert!(parse(&opts(&[("action", "select-window")])).is_err());
        assert!(parse(&opts(&[])).is_err());
    }
}
