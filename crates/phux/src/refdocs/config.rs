//! The generated config reference: a section index over the full schema,
//! a scalar-key defaults table, and the annotated `default.toml` verbatim.
//!
//! Three sources, all compiled into this binary, keep the page honest:
//!
//! 1. [`SECTIONS`] names every top-level `Config` field. A unit test
//!    serializes a fully-populated sample [`phux_config::Config`] and
//!    asserts key-set equality, so a schema field added without a
//!    `SECTIONS` row fails the test suite until it is documented here.
//! 2. The scalar-key table is walked out of the serialized schema
//!    defaults ([`phux_config::Config::default`]); a second test pins
//!    that the embedded `default.toml` agrees with those values, so the
//!    table can honestly be labelled "the shipped defaults".
//! 3. The fenced TOML block is [`phux_config::DEFAULT_CONFIG_TOML`]
//!    verbatim — the annotated base layer users actually inherit,
//!    including the commented `[sidebar]` / `[[remote]]` /
//!    `[[satellites]]` / `[[connector]]` blocks.

use phux_config::DEFAULT_CONFIG_TOML;

use super::Page;

/// One top-level section of the config schema, as the reference lists it.
struct Section {
    /// serde key of the top-level [`phux_config::Config`] field — what
    /// the key-set test compares against.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read only by the key-set coverage test")
    )]
    key: &'static str,
    /// The TOML header as a user writes it (`[defaults]`, `[[remote]]`).
    header: &'static str,
    /// One-line summary for the section index.
    summary: &'static str,
}

/// Every top-level `config.toml` section, in schema declaration order.
///
/// Pinned against the serde key set of a fully-populated
/// [`phux_config::Config`] by `sections_cover_the_whole_schema`: adding a
/// schema field without a row here fails CI until the reference names it.
const SECTIONS: &[Section] = &[
    Section {
        key: "defaults",
        header: "[defaults]",
        summary: "Server-wide defaults: shell, `TERM`, scrollback depth, \
                  mouse tracking, spawn-time cwd policy, session naming, \
                  multi-view window sizing.",
    },
    Section {
        key: "keybindings",
        header: "[keybindings]",
        summary: "Prefix chord, the prefix-table and global binding maps, \
                  and the which-key popup knobs.",
    },
    Section {
        key: "status",
        header: "[status]",
        summary: "Status-bar composition: widget lists for the left, \
                  center, and right slots, plus which outer-terminal row \
                  the bar reserves.",
    },
    Section {
        key: "sidebar",
        header: "[sidebar]",
        summary: "The window sidebar: off by default; width in columns and \
                  the edge it docks to when enabled.",
    },
    Section {
        key: "chrome",
        header: "[chrome]",
        summary: "Responsive-chrome breakpoints: the column and row counts \
                  at which overlays go full-bleed and the sidebar yields \
                  its columns back to the panes.",
    },
    Section {
        key: "hooks",
        header: "[[hooks.<event>]]",
        summary: "Event hooks: per event name, an array of `when` \
                  predicates each paired with an action to run on match.",
    },
    Section {
        key: "plugins",
        header: "[[plugins]]",
        summary: "Declarative plugin manifests composed into this config; \
                  each entry names a `phux-plugin.toml` path and an \
                  enabled flag.",
    },
    Section {
        key: "satellites",
        header: "[[satellites]]",
        summary: "Federation satellites a hub routes to: name, endpoint, \
                  token-file path, and certificate pin (ADR-0038).",
    },
    Section {
        key: "connector",
        header: "[[connector]]",
        summary: "Outbound relay links this server supervises: relay \
                  endpoint, token-file path, and certificate pin \
                  (ADR-0052).",
    },
    Section {
        key: "remote",
        header: "[[remote]]",
        summary: "Remote phux servers this machine attaches to, written by \
                  `phux host enroll` / `phux host add` and resolved by \
                  `phux attach <name>` (ADR-0055).",
    },
    Section {
        key: "theme",
        header: "[theme]",
        summary: "Free-form color slots (`slot = \"color\"`) consumed by \
                  the renderer.",
    },
    Section {
        key: "experimental",
        header: "[experimental]",
        summary: "Opt-in unstable knobs; anything here may change or \
                  disappear without notice.",
    },
];

/// Collect every scalar leaf of `value` as a `(dotted-key, TOML literal)`
/// row. Arrays (widget lists, hook entries, registry tables) are skipped:
/// they are composition, not knobs, and the annotated TOML block shows
/// their shape.
fn scalar_rows(value: &toml::Value, path: &str, rows: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                scalar_rows(child, &child_path, rows);
            }
        }
        toml::Value::Array(_) => {}
        scalar => rows.push((path.to_owned(), scalar.to_string())),
    }
}

/// The scalar knobs of the schema defaults, as rendered in the table.
///
/// Walked from `Config::default()` rather than the parsed `default.toml`
/// so the rows are exactly the schema's scalar fields — the shipped
/// file's binding maps and widget lists would otherwise leak their
/// entries in as pseudo-keys. A unit test pins the two sources to agree
/// on every one of these rows.
#[allow(
    clippy::expect_used,
    reason = "serializing the schema's own Default cannot fail, and a panic \
              here fails every refdocs test loudly rather than publishing a \
              page with an empty table"
)]
fn default_scalar_rows() -> Vec<(String, String)> {
    let defaults = toml::Value::try_from(phux_config::Config::default())
        .expect("Config::default() serializes to TOML");
    let mut rows = Vec::new();
    scalar_rows(&defaults, "", &mut rows);
    rows
}

/// Render `docs/reference/config.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "The configuration surface of `~/.config/phux/config.toml`. The \
         loader layers your file on top of the annotated defaults shown \
         at the bottom of this page: every key you set wins, everything \
         you omit keeps tracking the shipped default. Scaffold a starter \
         file with `phux config init`, validate yours with \
         `phux config check`, and inspect the effective merged result \
         with `phux config show`.\n\n\
         ## Sections\n\n\
         | Section | Contents |\n\
         |---|---|\n",
    );
    for section in SECTIONS {
        let Section {
            header, summary, ..
        } = section;
        let _ = writeln!(body, "| `{header}` | {summary} |");
    }

    body.push_str(
        "\n## Scalar keys\n\n\
         Every scalar knob with its shipped default, serialized from the \
         schema itself. Keys that are unset by default (`defaults.shell`, \
         `defaults.spawn-on-attach`) and composite keys — widget lists, \
         binding tables, hook and registry arrays — do not appear here; \
         the annotated config below documents them in place.\n\n\
         | Key | Default |\n\
         |---|---|\n",
    );
    for (key, value) in default_scalar_rows() {
        let _ = writeln!(body, "| `{key}` | `{value}` |");
    }

    debug_assert!(
        !DEFAULT_CONFIG_TOML.contains("```"),
        "default.toml must not break out of the fenced block"
    );
    let _ = write!(
        body,
        "\n## The annotated default config\n\n\
         The base layer embedded in the binary \
         (`crates/phux-config/src/default.toml`), verbatim. `phux config \
         init` writes a fully-commented projection of this file, and \
         `phux config show --default` prints it.\n\n\
         ```toml\n{DEFAULT_CONFIG_TOML}```\n"
    );

    Page {
        file: "config.md",
        title: "phux config reference",
        summary: "Every `config.toml` section, the scalar defaults, and \
                  the annotated default config.",
        tldr: "The complete `config.toml` surface: a section index pinned \
               against the config schema, every scalar knob with its \
               shipped default, and the annotated default configuration \
               embedded in the binary that generated this page.",
        body,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use phux_config::{
        Action, Config, ConnectorConfigEntry, HookEntry, RemoteConfigEntry, SatelliteConfigEntry,
        Widget,
    };

    use super::{SECTIONS, default_scalar_rows, page, scalar_rows};

    /// A `Config` with every collection field non-empty and every option
    /// set, so serialization cannot drop a top-level key (empty maps and
    /// `None` options are the shapes serializers elide).
    fn fully_populated_sample() -> Config {
        let mut config = Config::default();
        config.defaults.shell = Some("/bin/zsh".to_owned());
        config.defaults.spawn_on_attach = Some("htop".to_owned());
        config
            .keybindings
            .prefix_table
            .insert("x".to_owned(), Action::Bare("kill-pane".to_owned()));
        config
            .keybindings
            .global
            .insert("M-Enter".to_owned(), Action::Bare("detach".to_owned()));
        config.status.left = vec![Widget::Bare("windows".to_owned())];
        config.sidebar.enabled = true;
        config.hooks.insert(
            "pane-exit".to_owned(),
            vec![HookEntry {
                when: std::collections::BTreeMap::new(),
                action: Action::Bare("kill-pane".to_owned()),
            }],
        );
        config.plugins = vec![phux_config::plugin::PluginConfigEntry {
            manifest: PathBuf::from("/plugins/example/phux-plugin.toml"),
            enabled: true,
        }];
        config.satellites = vec![SatelliteConfigEntry {
            name: "devbox".to_owned(),
            endpoint: "quic://devbox.example:8788".to_owned(),
            enabled: true,
            token_file: Some(PathBuf::from("/tokens/devbox.token")),
            cert_fingerprint: Some("AB:CD".to_owned()),
        }];
        config.connector = vec![ConnectorConfigEntry {
            relay: "relay.example:4433".to_owned(),
            token_file: Some(PathBuf::from("/tokens/relay.token")),
            cert_fingerprint: Some("AB:CD".to_owned()),
        }];
        config.remote = vec![RemoteConfigEntry {
            name: "mini".to_owned(),
            endpoint: "quic://mini.example:8788".to_owned(),
            token_file: Some(PathBuf::from("/tokens/mini.token")),
            cert_fingerprint: Some("AB:CD".to_owned()),
            session: Some("main".to_owned()),
        }];
        config
            .theme
            .slots
            .insert("fg".to_owned(), "#cdd6f4".to_owned());
        config.experimental.predictive_echo = true;
        config
    }

    /// THE coverage gate: `SECTIONS` and the schema's serde key set must
    /// be identical. A new top-level `Config` field fails here until the
    /// reference documents it; a `SECTIONS` row for a removed field fails
    /// here until it is deleted.
    #[test]
    fn sections_cover_the_whole_schema() {
        let serialized = toml::Value::try_from(fully_populated_sample())
            .expect("a fully-populated Config serializes to TOML");
        let schema_keys: BTreeSet<&str> = serialized
            .as_table()
            .expect("Config serializes to a table")
            .keys()
            .map(String::as_str)
            .collect();
        let section_keys: BTreeSet<&str> = SECTIONS.iter().map(|section| section.key).collect();
        assert_eq!(
            section_keys, schema_keys,
            "refdocs::config::SECTIONS drifted from the Config schema; \
             update SECTIONS in crates/phux/src/refdocs/config.rs, then \
             run `just docs-gen` and commit the result"
        );
    }

    /// The scalar table is walked from `Config::default()` but billed as
    /// the shipped defaults, so the embedded `default.toml` must agree on
    /// every scalar it also covers. If this fails, either the file set a
    /// scalar the schema defaults differently (reconcile them) or a serde
    /// default changed without regenerating (run `just docs-gen`).
    #[test]
    fn the_embedded_defaults_agree_with_the_schema_defaults() {
        let parsed =
            phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
                .expect("embedded defaults parse");
        let serialized = toml::Value::try_from(parsed).expect("parsed defaults serialize");
        let mut shipped = Vec::new();
        scalar_rows(&serialized, "", &mut shipped);

        for (key, default_value) in default_scalar_rows() {
            let shipped_value = shipped
                .iter()
                .find(|(shipped_key, _)| *shipped_key == key)
                .map_or_else(
                    || panic!("default.toml round-trip lost the `{key}` scalar"),
                    |(_, value)| value.as_str(),
                );
            assert_eq!(
                shipped_value, default_value,
                "`{key}`: default.toml ships {shipped_value} but the \
                 schema default is {default_value}; reconcile them, then \
                 run `just docs-gen`"
            );
        }
    }

    /// The page carries all three parts: every section header in the
    /// index (the `[sidebar]` / `[[remote]]` gaps this page closes
    /// included), representative scalar rows, and the annotated defaults
    /// verbatim inside the fence.
    #[test]
    fn config_page_renders_index_scalars_and_annotated_defaults() {
        let page = page();
        for section in SECTIONS {
            assert!(
                page.body.contains(&format!("| `{}` |", section.header)),
                "section index lost the {} row",
                section.header
            );
        }
        for row in [
            "| `defaults.history-limit` | `50000` |",
            "| `keybindings.prefix` | `\"C-a\"` |",
            "| `sidebar.width` | `28` |",
            "| `status.position` | `\"bottom\"` |",
        ] {
            assert!(page.body.contains(row), "scalar table lost the row {row:?}");
        }
        assert!(
            page.body
                .contains(&format!("```toml\n{}```", phux_config::DEFAULT_CONFIG_TOML)),
            "the annotated default config must appear verbatim in a fenced block"
        );
    }
}
