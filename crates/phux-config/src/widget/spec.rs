//! The declarative widget surface: per-kind doc specs, the universal
//! options, and the pure option validators `phux config check` and the TUI's
//! factories share.

use std::collections::BTreeMap;

use crate::schema::WidgetSpec;
use crate::vocab;
use crate::widget::{CellStyle, WidgetError, kinds};

/// Universal widget-level [`CellStyle`] table.
const STYLE_OPT: &str = "style";
/// Universal bounds on the terminal width: outside them the widget
/// renders nothing, so one lineup can adapt to the terminal.
const MIN_COLS_OPT: &str = "min-cols";
const MAX_COLS_OPT: &str = "max-cols";
const UNIVERSAL_OPTS: [&str; 3] = [STYLE_OPT, MIN_COLS_OPT, MAX_COLS_OPT];

/// Documentation spec for one built-in widget kind. The factory validates its
/// options against the same const, so the documented surface
/// (`docs/reference/widgets.md`) and the enforced one cannot drift.
#[derive(Debug, Clone, Copy)]
pub struct WidgetKindSpec {
    /// Registered kind string (the `kind = "..."` value).
    pub kind: &'static str,
    /// One-paragraph human description of what the widget renders.
    pub summary: &'static str,
    /// The kind-specific options the factory accepts.
    pub options: &'static [WidgetOptSpec],
    /// Pure check of the kind-specific options (universal ones stripped):
    /// the same rules the factory builds from, minus the build.
    pub validate: OptsValidator,
}

/// Validates one kind's options without building a widget.
pub type OptsValidator = fn(&BTreeMap<String, toml::Value>) -> Result<(), WidgetError>;

/// One accepted option of a widget kind.
#[derive(Debug, Clone, Copy)]
pub struct WidgetOptSpec {
    /// Canonical option key (kebab-case).
    pub name: &'static str,
    /// Alternative accepted spellings (e.g. `max_len` for `max-len`).
    pub aliases: &'static [&'static str],
    /// Type, default, and behavior in one line, rendered verbatim.
    pub doc: &'static str,
}

impl WidgetKindSpec {
    fn accepts(&self, key: &str) -> bool {
        self.options
            .iter()
            .any(|opt| opt.name == key || opt.aliases.contains(&key))
    }

    /// Did-you-mean candidates: every option spelling plus the universals.
    fn candidate_keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self
            .options
            .iter()
            .flat_map(|opt| std::iter::once(opt.name).chain(opt.aliases.iter().copied()))
            .collect();
        keys.extend_from_slice(&UNIVERSAL_OPTS);
        keys
    }
}

/// Doc specs of every built-in widget kind, in ASCII order of kind.
pub const BUILTIN_WIDGET_SPECS: &[&WidgetKindSpec] = &[
    &kinds::button::SPEC,
    &kinds::cwd::SPEC,
    &kinds::exec::SPEC,
    &kinds::exit_status::SPEC,
    &kinds::help_hints::SPEC,
    &kinds::session_name::SPEC,
    &kinds::spacer::SPEC,
    &kinds::switch::SPEC,
    &kinds::text::SPEC,
    &kinds::time::SPEC,
    &kinds::windows::SPEC,
];

/// The built-in spec registered under `kind`.
#[must_use]
pub fn builtin_spec(kind: &str) -> Option<&'static WidgetKindSpec> {
    BUILTIN_WIDGET_SPECS
        .iter()
        .copied()
        .find(|spec| spec.kind == kind)
}

/// The universal options every widget accepts, parsed.
#[derive(Debug, Clone, Default)]
pub struct UniversalOpts {
    /// Widget-level style, applied to the cells the widget left unstyled.
    pub style: Option<CellStyle>,
    /// Narrowest terminal the widget renders on.
    pub min_cols: Option<u16>,
    /// Widest terminal the widget renders on.
    pub max_cols: Option<u16>,
}

/// Parse the universal options of `spec` and return them with the
/// kind-specific remainder.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for a malformed universal option, or a
/// `min-cols` above `max-cols`.
pub fn split_universal(
    spec: &WidgetSpec,
) -> Result<(UniversalOpts, BTreeMap<String, toml::Value>), WidgetError> {
    let style = style_opt(&spec.kind, &spec.opts, STYLE_OPT)?;
    let min_cols = cols_opt(&spec.kind, &spec.opts, MIN_COLS_OPT)?;
    let max_cols = cols_opt(&spec.kind, &spec.opts, MAX_COLS_OPT)?;
    if let (Some(min), Some(max)) = (min_cols, max_cols)
        && min > max
    {
        return Err(invalid(
            &spec.kind,
            format!(
                "`{MIN_COLS_OPT}` ({min}) is above `{MAX_COLS_OPT}` ({max}), so this widget \
                 could never render"
            ),
        ));
    }
    let mut opts = spec.opts.clone();
    for key in UNIVERSAL_OPTS {
        opts.remove(key);
    }
    let universal = UniversalOpts {
        style,
        min_cols,
        max_cols,
    };
    Ok((universal, opts))
}

/// Check `spec` against the built-in kinds exactly as the TUI's registry
/// builds it, without building anything.
///
/// # Errors
///
/// [`WidgetError::UnknownKind`] for a kind no built-in spec names, or
/// [`WidgetError::InvalidOption`] from a universal or kind-specific option.
pub fn validate(spec: &WidgetSpec) -> Result<(), WidgetError> {
    let kind =
        builtin_spec(&spec.kind).ok_or_else(|| WidgetError::UnknownKind(spec.kind.clone()))?;
    let (_, opts) = split_universal(spec)?;
    (kind.validate)(&opts)
}

pub(super) fn invalid(kind: &str, message: String) -> WidgetError {
    WidgetError::InvalidOption {
        kind: kind.to_owned(),
        message,
    }
}

/// Parse an optional [`CellStyle`] from an inline-table option.
pub(super) fn style_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
) -> Result<Option<CellStyle>, WidgetError> {
    opts.get(key).map_or(Ok(None), |value| {
        value
            .clone()
            .try_into::<CellStyle>()
            .map(Some)
            .map_err(|e| invalid(kind, format!("`{key}` must be a style table: {e}")))
    })
}

/// Parse an optional string option.
pub(super) fn string_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
) -> Result<Option<String>, WidgetError> {
    match opts.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(invalid(
            kind,
            format!("`{key}` must be a string, got {}", other.type_str()),
        )),
    }
}

/// Parse an optional integer option that must be `> 0`, spelled `key` or
/// `alias`.
pub(super) fn positive_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
    alias: Option<&str>,
) -> Result<Option<usize>, WidgetError> {
    match opts.get(key).or_else(|| alias.and_then(|a| opts.get(a))) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n > 0 => usize::try_from(*n)
            .map(Some)
            .map_err(|_| invalid(kind, format!("`{key}` does not fit in usize: {n}"))),
        Some(toml::Value::Integer(n)) => {
            Err(invalid(kind, format!("`{key}` must be > 0, got {n}")))
        }
        Some(other) => Err(invalid(
            kind,
            format!("`{key}` must be an integer, got {}", other.type_str()),
        )),
    }
}

/// Reject any option key outside the kind's [`WidgetKindSpec`], suggesting
/// the nearest valid one. Every factory calls this first.
pub(super) fn reject_unknown_opts(
    spec: &WidgetKindSpec,
    opts: &BTreeMap<String, toml::Value>,
) -> Result<(), WidgetError> {
    for key in opts.keys() {
        if UNIVERSAL_OPTS.contains(&key.as_str()) || spec.accepts(key) {
            continue;
        }
        let suggestion = vocab::did_you_mean(key, &spec.candidate_keys())
            .map(|hit| format!(" (did you mean `{hit}`?)"))
            .unwrap_or_default();
        return Err(invalid(
            spec.kind,
            format!("unknown option `{key}`{suggestion}"),
        ));
    }
    Ok(())
}

/// Parse an optional `u16` column-count option.
fn cols_opt(
    kind: &str,
    opts: &BTreeMap<String, toml::Value>,
    key: &str,
) -> Result<Option<u16>, WidgetError> {
    match opts.get(key) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) => u16::try_from(*n).map(Some).map_err(|_| {
            invalid(
                kind,
                format!("`{key}` must be a column count in 0..=65535, got {n}"),
            )
        }),
        Some(other) => Err(invalid(
            kind,
            format!("`{key}` must be an integer, got {}", other.type_str()),
        )),
    }
}
