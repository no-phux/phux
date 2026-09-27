//! `text` widget — a literal string, for separators, labels, and decoration.

use std::collections::BTreeMap;

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec, invalid,
    reject_unknown_opts, string_opt,
};

const KIND: &str = "text";

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
    /// The literal text.
    pub value: String,
}

impl StatusWidget for TextWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        WidgetCells::from_text(&self.value)
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let value = string_opt(KIND, opts, "value")?.ok_or_else(|| {
        invalid(
            KIND,
            "`value` is required — a `text` widget with nothing to render is always a mistake"
                .to_owned(),
        )
    })?;
    Ok(Box::new(TextWidget { value }))
}

#[cfg(test)]
mod tests {
    use super::factory;
    use crate::widget::WidgetError;
    use std::collections::BTreeMap;

    fn message(pairs: &[(&str, toml::Value)]) -> String {
        let opts: BTreeMap<String, toml::Value> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        match factory(&opts) {
            Err(WidgetError::InvalidOption { kind, message }) => {
                assert_eq!(kind, "text");
                message
            }
            Err(other) => panic!("unexpected {other:?}"),
            Ok(_) => String::new(),
        }
    }

    /// A missing `value` ("forgot to say what to render") is an error; an
    /// empty one ("deliberately blank") is not.
    #[test]
    fn value_is_required_but_may_be_empty() {
        assert!(message(&[]).contains("`value` is required"));
        assert!(message(&[("value", 7.into())]).contains("got integer"));
        assert!(
            message(&[("value", "x".into()), ("valu", "typo".into())]).contains("unknown option")
        );
        assert_eq!(message(&[("value", "".into())]), "");
    }
}
