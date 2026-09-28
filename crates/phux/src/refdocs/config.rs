//! The generated config reference: a section index, a scalar-key defaults
//! table, and the annotated `default.toml` verbatim. Tests pin [`SECTIONS`] to
//! the schema's key set and the table (walked from `Config::default()`) to the
//! embedded `default.toml`.

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

/// Every top-level `config.toml` section, in schema order; pinned by
/// `sections_cover_the_whole_schema`.
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
        summary: "The window sidebar: enabled by default; width 0 adapts to \
                  28–40 columns, positive widths stay fixed; position chooses \
                  the docking edge.",
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
                  `phux host add` and resolved by `phux attach <name>` \
                  (ADR-0055, ADR-0122).",
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
    Section {
        key: "policy",
        header: "[policy]",
        summary: "The authorization posture read at server start: `local` \
                  (owner socket only) or `paired` (workload mTLS, scope \
                  ceilings enforced at dispatch).",
    },
    Section {
        key: "voice",
        header: "[voice]",
        summary: "The server-side transcriber behind `TRANSCRIBE`: an argv \
                  that turns an uploaded clip into text for a paste.",
    },
    Section {
        key: "limits",
        header: "[limits]",
        summary: "Server-enforced ceilings that are not a per-pane spawn \
                  default: the largest L3 metadata value the server stores \
                  at one key.",
    },
];

/// Collect every scalar leaf of `value` as `(dotted-key, TOML literal)`;
/// arrays are composition, shown in the TOML block instead.
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

/// Keys whose shipped state is a documented "unset": serde omits a `None`,
/// so these rows restore the key. Guarded by
/// `tristate_keys_are_really_unset_in_the_schema`.
const TRISTATE_ROWS: &[(&str, &str)] = &[
    (
        "experimental.predictive-echo",
        "unset — the dial decides: on when the attach leaves the machine, \
         off otherwise. `true` / `false` force it on every transport",
    ),
    (
        "policy.mode",
        "unset — transitional: every admitted connection holds the owner's \
         full grant, and a remote listener is warned about at startup. \
         `local` admits the owner socket only; `paired` requires an enrolled \
         workload certificate on every TLS connection",
    ),
    (
        "voice.transcriber",
        "unset — `TRANSCRIBE` is refused with a remedy. An argv; `{path}` is \
         replaced by the uploaded clip's path and stdout is the transcript",
    ),
    (
        "voice.timeout-secs",
        "unset — 30. Seconds before the transcriber is killed and the \
         request refused",
    ),
];

/// The scalar knobs of the schema defaults, walked from `Config::default()`
/// (the parsed file would leak binding maps and widget lists as keys).
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

/// The rendered `| Key | Default |` cells. Kept apart from
/// [`default_scalar_rows`], which stays the raw serialized values the
/// agreement test compares.
fn rendered_scalar_rows() -> Vec<(String, String)> {
    // Serialized defaults render as code; tri-state rows are prose with their
    // own code spans.
    let mut rows: Vec<(String, String)> = default_scalar_rows()
        .into_iter()
        .map(|(key, value)| (key, format!("`{value}`")))
        .collect();
    rows.extend(
        TRISTATE_ROWS
            .iter()
            .map(|(key, meaning)| ((*key).to_owned(), (*meaning).to_owned())),
    );
    // `scalar_rows` walks a `toml::Value::Table`, which is already sorted, so
    // the appended rows are the only ones out of place. Sorting the whole list
    // keeps the page's one ordering rule ("alphabetical by key") true.
    rows.sort_by(|(a, _), (b, _)| a.cmp(b));
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
         schema itself, plus the tri-state knobs whose shipped state is \
         *unset* and whose unset meaning is spelled out in place of a \
         value. Keys that are simply absent by default (`defaults.shell`, \
         `defaults.spawn-on-attach`) and composite keys — widget lists, \
         binding tables, hook and registry arrays — do not appear here; \
         the annotated config below documents them in place.\n\n\
         | Key | Default |\n\
         |---|---|\n",
    );
    for (key, value) in rendered_scalar_rows() {
        let _ = writeln!(body, "| `{key}` | {value} |");
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
            ssh: Some("me@mini".to_owned()),
            direct: None,
        }];
        config
            .theme
            .slots
            .insert("fg".to_owned(), "#cdd6f4".to_owned());
        config.experimental.predictive_echo = Some(true);
        config
    }

    /// Every [`TRISTATE_ROWS`] key must really be absent from the serialized
    /// defaults, or the page would carry a duplicate, stale row.
    #[test]
    fn tristate_keys_are_really_unset_in_the_schema() {
        let defaults =
            toml::Value::try_from(Config::default()).expect("Config::default() serializes to TOML");
        let mut serialized = Vec::new();
        scalar_rows(&defaults, "", &mut serialized);
        for (key, _) in super::TRISTATE_ROWS {
            assert!(
                !serialized.iter().any(|(got, _)| got == key),
                "{key} is listed in TRISTATE_ROWS but the schema now \
                 serializes a default for it; drop the hand-written row and \
                 let the generated table carry it"
            );
        }
    }

    /// `SECTIONS` and the schema's serde key set must be identical.
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

    /// The embedded `default.toml` must agree with the schema defaults on every
    /// scalar it covers (else reconcile, or run `just docs-gen`).
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

    /// The page carries the section index, scalar rows, and the verbatim
    /// defaults.
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
            "| `defaults.history-bytes` | `2097152` |",
            "| `defaults.history-limit` | `50000` |",
            "| `keybindings.prefix` | `\"C-a\"` |",
            "| `sidebar.width` | `0` |",
            "| `status.position` | `\"top\"` |",
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
