//! `text` options — a literal string, for separators, labels, and
//! decoration.

use std::collections::BTreeMap;

use crate::widget::{
    WidgetError, WidgetKindSpec, WidgetOptSpec, invalid, reject_unknown_opts, string_opt,
};

const KIND: &str = "text";

/// Doc and validation spec of the `text` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
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
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `text` options.
#[derive(Debug, Clone)]
pub struct TextOpts {
    /// The literal text.
    pub value: String,
}

/// Validate and resolve `text` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown option or a missing or
/// mistyped `value`.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<TextOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let value = string_opt(KIND, opts, "value")?.ok_or_else(|| {
        invalid(
            KIND,
            "`value` is required — a `text` widget with nothing to render is always a mistake"
                .to_owned(),
        )
    })?;
    Ok(TextOpts { value })
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::widget::WidgetError;
    use std::collections::BTreeMap;

    fn message(pairs: &[(&str, toml::Value)]) -> String {
        let opts: BTreeMap<String, toml::Value> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        match parse(&opts) {
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
