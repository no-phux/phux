//! `spacer` options — elastic blank space that pushes its neighbours apart.

use std::collections::BTreeMap;

use crate::widget::{WidgetError, WidgetKindSpec, reject_unknown_opts};

/// Doc and validation spec of the `spacer` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: "spacer",
    summary: "Elastic blank space. Takes no width of its own and then \
              absorbs an even share of whatever columns the row has left \
              over, so the widgets on either side of it are pushed apart. \
              Renders nothing on a row with no room to spare, which makes \
              it the first thing to yield on a narrow terminal rather than \
              something that has to be configured away. Style it (`style = \
              { bg = ... }`) to paint the gap rather than leave it blank.",
    options: &[],
    validate: parse,
};

/// Validate `spacer` options: it takes none of its own.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for any kind-specific option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<(), WidgetError> {
    reject_unknown_opts(&SPEC, opts)
}
