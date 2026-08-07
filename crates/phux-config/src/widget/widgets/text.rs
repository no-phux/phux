//! `text` widget — a literal styled string.
//!
//! The simplest custom-bar building block: everything a user writes in
//! `value` is rendered verbatim, with the universal `style` table applied
//! by the registry like any other widget. It exists so a bar can carry
//! separators, labels, and fixed decoration without needing a widget kind
//! per piece of punctuation.

use std::collections::BTreeMap;

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec,
    reject_unknown_opts,
};

/// Widget kind, used in error messages.
const KIND: &str = "text";

/// Doc spec — the factory validates against this same const, so the
/// documented option surface is the enforced one (phux-i0e8.11.3).
pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "A literal string, rendered verbatim. The building block for \
              separators, labels, and fixed decoration in a custom bar.",
    options: &[WidgetOptSpec {
        name: "value",
        aliases: &[],
        doc: "string, REQUIRED — the literal text to render. May be empty, \
              which renders nothing; there is no default, because a `text` \
              widget with no `value` is always a mistake rather than a \
              request for a blank.",
    }],
};

/// `text` widget: renders [`Self::value`] verbatim.
#[derive(Debug, Clone)]
pub struct TextWidget {
    /// The literal text rendered on every repaint.
    pub value: String,
}

impl TextWidget {
    /// Construct a `TextWidget` rendering `value` verbatim.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self { value }
    }
}

impl StatusWidget for TextWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        WidgetCells::from_text(&self.value)
    }

    // No `poll_interval`: the content is fixed at build time, so this
    // widget never asks for a repaint of its own.
}

/// Factory: builds a [`TextWidget`] from a TOML `opts` map.
///
/// Accepted keys (per [`SPEC`], rendered into `docs/reference/widgets.md`):
/// - `value` (string, required) — the literal text.
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option, a missing
/// `value`, or a `value` that is not a string.
pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let value = match opts.get("value") {
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`value` must be a string, got {}", other.type_str()),
            });
        }
        None => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: "`value` is required — a `text` widget with nothing \
                          to render is always a mistake"
                    .to_owned(),
            });
        }
    };
    Ok(Box::new(TextWidget { value }))
}

#[cfg(test)]
mod tests {
    use super::{KIND, factory};
    use crate::widget::WidgetError;
    use std::collections::BTreeMap;

    fn opts(pairs: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn a_missing_value_is_refused_by_name() {
        let err = factory(&opts(&[])).expect_err("`value` is required");
        let WidgetError::InvalidOption { kind, message } = err else {
            panic!("expected InvalidOption");
        };
        assert_eq!(kind, KIND);
        assert!(message.contains("`value` is required"), "got {message}");
    }

    #[test]
    fn a_non_string_value_names_the_type_it_got() {
        let err = factory(&opts(&[("value", toml::Value::Integer(7))]))
            .expect_err("`value` must be a string");
        let WidgetError::InvalidOption { message, .. } = err else {
            panic!("expected InvalidOption");
        };
        assert!(message.contains("got integer"), "got {message}");
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let err = factory(&opts(&[
            ("value", toml::Value::String("x".to_owned())),
            ("valu", toml::Value::String("typo".to_owned())),
        ]))
        .expect_err("unknown options are refused");
        let WidgetError::InvalidOption { message, .. } = err else {
            panic!("expected InvalidOption");
        };
        assert!(message.contains("unknown option"), "got {message}");
    }

    /// An empty `value` is accepted and renders nothing. Distinguished
    /// from a MISSING `value`, which is an error: the difference is
    /// "deliberately blank" versus "forgot to say what to render".
    #[test]
    fn an_empty_value_is_accepted() {
        factory(&opts(&[("value", toml::Value::String(String::new()))]))
            .expect("an explicitly empty value is a legitimate blank");
    }
}
