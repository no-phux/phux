//! `help-hints` options — clickable, prefix-aware navigation hints.

use std::collections::BTreeMap;

use crate::widget::{WidgetError, WidgetKindSpec, reject_unknown_opts};

/// Doc and validation spec of the `help-hints` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: "help-hints",
    summary: "Clickable, prefix-aware navigation (`<prefix>  s Sessions · \
              Space Commands · S Settings · ? Help · [ Copy`), rendered \
              with the configured prefix chord. Drops complete destinations \
              from the right as the bar narrows, and disappears entirely \
              rather than showing a fragment.",
    options: &[],
    validate: parse,
};

/// Validate `help-hints` options: it takes none of its own.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for any kind-specific option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<(), WidgetError> {
    reject_unknown_opts(&SPEC, opts)
}
