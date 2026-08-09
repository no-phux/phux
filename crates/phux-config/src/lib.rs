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
pub mod socket; // phux-93b (shared default socket path: daemon + thin clients)
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
    Action, Config, CwdInheritance, DefaultsCfg, ExperimentalCfg, HookEntry, KeybindingsCfg,
    ParamAction, SidebarCfg, SidebarPosition, StatusCfg, StatusPosition, ThemeCfg, Widget,
    WidgetSpec, WindowSize,
};
pub use widget::{
    Cell, CellStyle, SessionNameWidget, StatusBar, StatusWidget, TextWidget, TimeWidget,
    WidgetCells, WidgetContext, WidgetError, WidgetFactory, WidgetRegistry, WindowInfo,
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
/// component of `cwd`. Session names double as selector tokens
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
    let basename = cwd
        .file_name()
        .map(|os| os.to_string_lossy().replace(':', "_"))
        .unwrap_or_default();
    template.replace("${cwd-basename}", &basename)
}

#[cfg(test)]
mod session_name_tests {
    use super::render_session_name_template;
    use std::path::Path;

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
