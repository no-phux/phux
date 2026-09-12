//! phux-config: TOML config + status-bar widget contract.
//!
//! This crate owns the typed schema for `~/.config/phux/config.toml`
//! (see `docs/consumers/tui.md` §4). Higher-level crates load a [`Config`] via
//! [`parse_str`] and consume the typed view; widget rendering, keybind
//! resolution, and hook dispatch all read from this schema.
//!
//! Parse errors carry `line:col` locations derived from the TOML byte
//! span so end-user diagnostics can point at the offending token. When
//! the underlying error has no span (deserialize failures on a merged
//! layer stack), the error carries no position rather than a
//! fabricated one (phux-i0e8.3.5).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod check; // phux-q9wj.3 (`phux config check`: schema-unknown keys)
pub mod connector;
pub mod distro;
mod error;
pub mod instance; // phux-zomb.2 (profile-scoped runtime/state: dev never touches production)
pub mod integration; // phux-ark7 (ADR-0042): agent integration templates + launch
mod layer;
pub mod overlay; // ADR-0037 (overlay detection: shared by `pair` and the server's auto-listen)
pub mod plugin;
pub mod remote;
pub mod satellite;
mod schema;
pub mod session_name; // phux-c2td.6 (`${random-name}` adjective-noun generator)
pub mod settings; // phux-u1tq.3 (scalar settings catalogue, provenance snapshot, comment-preserving writer)
pub mod socket; // phux-93b (shared default socket path: daemon + thin clients)
pub mod toml_registry;
pub mod vocab; // phux-i0e8.3.1 (validation vocabulary: action + hook event names)

// Wave 5 modules — each owned by its respective subtask:
pub mod keybind; // phux-nz4.3
pub mod loader; // phux-nz4.2
pub mod scaffold; // phux-ijp (config init: commented projection of default.toml)
pub mod widget; // phux-nz4.4 (note: schema::Widget is the TOML enum; widget::Widget is the trait)

pub use check::{CheckReport, Fault, Finding};
pub use connector::ConnectorConfigEntry;
pub use error::{ConfigError, byte_offset_to_line_col};
pub use layer::{
    ConfigProvenance, KeyOrigin, LayerSource, MAX_EXTENDS_DEPTH, merged_config_with_provenance,
};
pub use remote::RemoteConfigEntry;
pub use satellite::SatelliteConfigEntry;
pub use schema::{
    Action, ChromeCfg, Config, CwdInheritance, DEFAULT_AGENT_LOG_BYTES, DEFAULT_HISTORY_BYTES,
    DefaultsCfg, ExperimentalCfg, HookEntry, KeybindingsCfg, MAX_AGENT_LOG_BYTES,
    MAX_HISTORY_BYTES, ParamAction, ScrollbackLimits, SidebarCfg, SidebarPosition, StatusCfg,
    StatusPosition, ThemeCfg, VoiceCfg, Widget, WidgetSpec, WindowSize,
};
pub use session_name::{NameRng, RANDOM_NAME_PLACEHOLDER, random_name, template_has_random_name};
pub use settings::{
    Applies, CATALOG, Edit, EditOutcome, SettingEntry, SettingKind, SettingSection, SettingSpec,
    SettingsSnapshot,
};
pub use widget::{
    Cell, CellStyle, SessionNameWidget, SpacerWidget, StatusBar, StatusWidget, TextWidget,
    TimeWidget, WidgetCells, WidgetContext, WidgetError, WidgetFactory, WidgetRegistry, WindowInfo,
    WindowsWidget, row_to_string,
};

use std::path::Path;

/// Default phux configuration, shipped with the binary.
///
/// The loader layers the user's on-disk config on top of this — each
/// leaf the user sets wins; everything else is inherited from here.
/// Pure parsing via [`parse_str`] does NOT apply this; only
/// [`parse_with_defaults`] (used by [`loader::load_from`]) does.
///
/// Embedded at compile time. The doctest below pins that it parses.
///
/// ```
/// let cfg = phux_config::parse_str(
///     phux_config::DEFAULT_CONFIG_TOML,
///     std::path::Path::new("default.toml"),
/// ).expect("embedded defaults must parse");
/// assert_eq!(cfg.keybindings.prefix, "C-a");
/// ```
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default.toml");

/// Parse a TOML config from a string.
///
/// `path` is used only for error reporting — it is embedded in
/// [`ConfigError::Parse`] so messages display the source file.
///
/// This does NOT apply [`DEFAULT_CONFIG_TOML`]; callers wanting the
/// user-facing "shipped defaults + user overrides" behavior should use
/// [`parse_with_defaults`] (or [`loader::load_from`], which routes
/// through it).
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] if the input is not valid TOML or
/// does not deserialize into the schema (including unknown fields,
/// which are rejected by `serde(deny_unknown_fields)`).
pub fn parse_str(input: &str, path: &Path) -> Result<Config, ConfigError> {
    match toml::from_str::<Config>(input) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            let position = e
                .span()
                .map(|range| byte_offset_to_line_col(input, range.start));
            Err(ConfigError::Parse {
                path: path.to_path_buf(),
                position,
                message: e.message().to_owned(),
            })
        }
    }
}

/// Parse `user_input` and layer it over [`DEFAULT_CONFIG_TOML`] plus
/// any layers its `extends` key names (ADR-0039).
///
/// Merge semantics: every leaf a later layer sets wins. Tables merge
/// recursively (so a layer can add one binding without restating the
/// whole `prefix-table`). Arrays do NOT merge element-wise — they
/// overwrite, because there is no per-element identity for widget
/// lists / hook lists. A key `x-append` holding an array is the
/// explicit opt-in: it appends to the inherited array `x` instead of
/// replacing it.
///
/// `path` is used for error reporting on the user input AND as the
/// base directory for resolving relative `extends` entries; when
/// `user_input` (or a resolved layer) declares `extends`, this
/// function reads those layer files from disk.
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] if the embedded defaults, the user
/// input, or a layer file fail to parse as TOML, or if the merged
/// document fails to deserialize into the schema. Layer resolution
/// failures surface as [`ConfigError::LayerRead`],
/// [`ConfigError::LayerCycle`], or [`ConfigError::Layer`], each naming
/// the offending file.
pub fn parse_with_defaults(user_input: &str, path: &Path) -> Result<Config, ConfigError> {
    let merged = merged_config_table(user_input, path)?;
    deserialize_merged(merged, user_input, path)
}

/// Deserialize an already-merged layer table into the typed [`Config`].
///
/// Shared by [`parse_with_defaults`] and the settings snapshot
/// (`settings::SettingsSnapshot::load`), which already holds the merged
/// table from [`merged_config_with_provenance`] and must not merge twice.
pub(crate) fn deserialize_merged(
    merged: toml::Table,
    user_input: &str,
    path: &Path,
) -> Result<Config, ConfigError> {
    toml::Value::Table(merged).try_into().map_err(|e| {
        // Deserializing the merged table carries no span into the
        // user's text; a spanless error renders with no position
        // rather than a fabricated `1:1` (phux-i0e8.3.5).
        let position = e
            .span()
            .map(|r| byte_offset_to_line_col(user_input, r.start));
        ConfigError::Parse {
            path: path.to_path_buf(),
            position,
            message: e.message().to_owned(),
        }
    })
}

/// Merge the full layer stack — [`DEFAULT_CONFIG_TOML`], any layers
/// named via `extends` (ADR-0039), then `user_input` — and return the
/// resulting TOML table *without* deserializing into [`Config`].
///
/// This is the document-level half of [`parse_with_defaults`]: it is
/// what `phux config show` serializes to render the effective config
/// (the shipped defaults with all layers applied). Keeping it at the
/// table level means the rendered output is valid round-trippable
/// TOML rather than a typed struct re-serialized in schema order.
///
/// `path` is used for error reporting on `user_input` and as the base
/// directory for relative `extends` entries; layer files are read from
/// disk. When `user_input` declares no `extends`, no I/O occurs.
///
/// Callers that also need per-key layer attribution (`phux config show
/// --layers`) should use [`merged_config_with_provenance`], of which
/// this is the table-only projection.
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] if the embedded defaults,
/// `user_input`, or a layer file are not valid TOML;
/// [`ConfigError::LayerRead`] / [`ConfigError::LayerCycle`] /
/// [`ConfigError::Layer`] for layer-resolution and `-append` failures,
/// each naming the offending file.
pub fn merged_config_table(user_input: &str, path: &Path) -> Result<toml::Table, ConfigError> {
    merged_config_with_provenance(user_input, path).map(|(table, _)| table)
}

/// Render a [`DefaultsCfg::session_name_template`] into a concrete
/// session name for an auto-created session (phux-4li.1).
///
/// Substitutes the `${cwd-basename}` placeholder with the final path
/// component of `cwd`, and `${random-name}` with a freshly generated
/// adjective-noun pair (see [`render_session_name_template_with`] for a
/// caller-supplied generator). Session names double as selector tokens
/// (`name:N.M`, see `docs/consumers/tui.md` §3), and `:` is the
/// session→window delimiter, so any `:` in the basename is replaced with
/// `_` to keep an auto-name from colliding with the selector grammar.
/// (`.` is left intact — it only delimits inside the `name:N.M` tail, so
/// a dotted directory name like `my.project` parses cleanly as a bare
/// session name.) Unknown placeholders pass through verbatim.
///
/// May return an empty string when the template renders empty (e.g. the
/// template is exactly `${cwd-basename}` and `cwd` is `/`, which has no
/// final component); the caller decides the fallback.
#[must_use]
pub fn render_session_name_template(template: &str, cwd: &Path) -> String {
    render_session_name_template_with(template, cwd, &mut NameRng::from_entropy())
}

/// [`render_session_name_template`] with the `${random-name}` generator
/// supplied by the caller, so a collision retry can draw fresh candidates
/// from one generator and a test can seed it.
///
/// Every `${random-name}` in one render expands to the same pick. It is
/// expanded before `${cwd-basename}` so a directory whose name happens to
/// contain the placeholder text is never re-expanded.
#[must_use]
pub fn render_session_name_template_with(template: &str, cwd: &Path, rng: &mut NameRng) -> String {
    let basename = cwd
        .file_name()
        .map(|os| os.to_string_lossy().replace(':', "_"))
        .unwrap_or_default();
    expand_random_name(template, rng).replace("${cwd-basename}", &basename)
}

/// Replace `${random-name}` with one generated name; a template without the
/// placeholder is returned unchanged and draws nothing from `rng`.
fn expand_random_name(template: &str, rng: &mut NameRng) -> String {
    if !template_has_random_name(template) {
        return template.to_owned();
    }
    template.replace(RANDOM_NAME_PLACEHOLDER, &random_name(rng))
}

#[cfg(test)]
mod session_name_tests {
    use super::{
        NameRng, random_name, render_session_name_template, render_session_name_template_with,
    };
    use std::path::Path;

    #[test]
    fn random_name_placeholder_expands_to_the_seeded_pick() {
        let expected = random_name(&mut NameRng::seeded(9));
        assert_eq!(
            render_session_name_template_with(
                "${random-name}",
                Path::new("/tmp/x"),
                &mut NameRng::seeded(9)
            ),
            expected
        );
    }

    #[test]
    fn random_name_composes_with_literal_text_and_cwd_basename() {
        let expected = random_name(&mut NameRng::seeded(3));
        assert_eq!(
            render_session_name_template_with(
                "${cwd-basename}-${random-name}",
                Path::new("/home/me/notes"),
                &mut NameRng::seeded(3)
            ),
            format!("notes-{expected}")
        );
    }

    #[test]
    fn repeated_random_name_placeholders_share_one_pick() {
        let rendered = render_session_name_template_with(
            "${random-name}/${random-name}",
            Path::new("/tmp/x"),
            &mut NameRng::seeded(5),
        );
        let (a, b) = rendered.split_once('/').expect("two halves");
        assert_eq!(a, b);
    }

    #[test]
    fn basename_containing_the_placeholder_is_not_expanded() {
        assert_eq!(
            render_session_name_template_with(
                "${cwd-basename}",
                Path::new("/tmp/${random-name}"),
                &mut NameRng::seeded(1)
            ),
            "${random-name}"
        );
    }

    #[test]
    fn template_without_random_name_draws_nothing() {
        let mut rng = NameRng::seeded(11);
        let _ = render_session_name_template_with("default", Path::new("/tmp/x"), &mut rng);
        assert_eq!(random_name(&mut rng), random_name(&mut NameRng::seeded(11)));
    }

    #[test]
    fn literal_template_passes_through_unchanged() {
        // The shipped default is the literal "default" — no placeholder,
        // so behavior is unchanged unless the user opts into a template.
        assert_eq!(
            render_session_name_template("default", Path::new("/Users/phall/workspace/phux")),
            "default"
        );
    }

    #[test]
    fn cwd_basename_substitutes_the_final_component() {
        assert_eq!(
            render_session_name_template(
                "phux-${cwd-basename}",
                Path::new("/Users/phall/workspace/phux")
            ),
            "phux-phux"
        );
        assert_eq!(
            render_session_name_template("${cwd-basename}", Path::new("/home/me/notes")),
            "notes"
        );
    }

    #[test]
    fn colon_in_basename_is_sanitized_to_underscore() {
        // ':' would otherwise read as the session→window selector
        // delimiter.
        assert_eq!(
            render_session_name_template("${cwd-basename}", Path::new("/tmp/a:b")),
            "a_b"
        );
    }

    #[test]
    fn dot_in_basename_is_preserved() {
        assert_eq!(
            render_session_name_template("${cwd-basename}", Path::new("/tmp/my.project")),
            "my.project"
        );
    }

    #[test]
    fn root_cwd_renders_empty_basename() {
        // No final component — caller falls back to a default name.
        assert_eq!(
            render_session_name_template("${cwd-basename}", Path::new("/")),
            ""
        );
    }

    #[test]
    fn unknown_placeholder_passes_through_verbatim() {
        assert_eq!(
            render_session_name_template("${unknown}", Path::new("/tmp/x")),
            "${unknown}"
        );
    }
}
