//! The declarative status-bar widget surface: per-kind specs and option
//! validators, the [`CellStyle`] config and plugin manifests deserialize, and
//! plugin contribution merging.
//!
//! [`crate::Widget`] is the parsed TOML entry. The runtime (the widget trait,
//! registry, composer, and renderers) lives in `phux-tui`, whose factories
//! build from the same [`kinds`] parsers `phux config check` validates with.

mod contributions;
pub mod kinds;
mod spec;

pub use contributions::merge_widget_contributions;
pub use spec::{
    BUILTIN_WIDGET_SPECS, OptsValidator, UniversalOpts, WidgetKindSpec, WidgetOptSpec,
    builtin_spec, split_universal, validate,
};
use spec::{invalid, positive_opt, reject_unknown_opts, string_opt, style_opt};

/// Visual style for a status-bar cell as plain data. Colors are strings
/// (`"red"`, `"#cdd6f4"`, `"12"`) the render layer interprets (ADR-0020).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent SGR attribute toggle"
)]
pub struct CellStyle {
    /// Foreground color, or `None` for the terminal default.
    pub fg: Option<String>,
    /// Background color, or `None` for the terminal default.
    pub bg: Option<String>,
    /// Bold.
    pub bold: bool,
    /// Dim / faint.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Reverse video.
    pub reverse: bool,
}

impl CellStyle {
    /// `true` when every field is at its default (no styling).
    #[must_use]
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }

    /// `over` layered onto `self`: colours `over` sets replace, and each
    /// attribute is on when either side turns it on.
    #[must_use]
    pub fn layered(&self, over: &Self) -> Self {
        Self {
            fg: over.fg.clone().or_else(|| self.fg.clone()),
            bg: over.bg.clone().or_else(|| self.bg.clone()),
            bold: self.bold || over.bold,
            dim: self.dim || over.dim,
            italic: self.italic || over.italic,
            underline: self.underline || over.underline,
            reverse: self.reverse || over.reverse,
        }
    }
}

/// Failures from widget validation and construction.
#[derive(Debug, thiserror::Error)]
pub enum WidgetError {
    /// `spec.kind` names no known widget kind.
    #[error("unknown widget kind: {0}")]
    UnknownKind(String),
    /// A factory rejected one of its options.
    #[error("invalid option for widget {kind}: {message}")]
    InvalidOption {
        /// The widget kind that rejected the option.
        kind: String,
        /// Human-readable explanation.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::BUILTIN_WIDGET_SPECS;

    /// Each documented spec is renderable and unambiguous
    /// (`docs/reference/widgets.md`); `phux-tui` pins the kinds to its
    /// registry.
    #[test]
    fn builtin_specs_are_renderable_and_unambiguous() {
        for spec in BUILTIN_WIDGET_SPECS {
            assert!(!spec.summary.trim().is_empty(), "`{}` summary", spec.kind);
            let mut seen = BTreeSet::new();
            for opt in spec.options {
                assert!(
                    !opt.doc.trim().is_empty(),
                    "`{}.{}` doc",
                    spec.kind,
                    opt.name
                );
                for key in std::iter::once(opt.name).chain(opt.aliases.iter().copied()) {
                    assert!(seen.insert(key), "`{}` spells `{key}` twice", spec.kind);
                }
            }
        }
    }
}
