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
        summary: "The window sidebar: enabled by default; width defaults to \
                  20 columns, 0 adapts to 28–40; position chooses \
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
        key: "notifications",
        header: "[notifications]",
        summary: "Agent notifications the TUI posts when an agent blocks \
                  or finishes: delivery mode (terminal OSC, system, bell, \
                  off), the OSC dialect, per-event toggles, focus \
                  suppression, and sound.",
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
                  token-file path, certificate pin, the client \
                  certificate a paired satellite requires, and an optional \
                  `color` like `[[remote]]`'s.",
    },
    Section {
        key: "connector",
        header: "[[connector]]",
        summary: "Outbound relay links this server supervises: relay \
                  endpoint, token-file path, and certificate pin.",
    },
    Section {
        key: "uplink",
        header: "[[uplink]]",
        summary: "Hubs this server dials out to as a satellite, written by \
                  `phux host join`: satellite name, hub endpoint, the hub's \
                  CA pin, and the enrolled identity's paths.",
    },
    Section {
        key: "mirrors",
        header: "[[mirrors]]",
        summary: "Displays and windows the server keeps mirrored, each in \
                  its own `mirror-*` session, restarted with backoff and \
                  live-reloaded (Linux; macOS skips them).",
    },
    Section {
        key: "producers",
        header: "[[producers]]",
        summary: "Commands kept running as document producers, each in its \
                  own `producer-*` session, like `[[mirrors]]`.",
    },
    Section {
        key: "remote",
        header: "[[remote]]",
        summary: "Remote phux servers this machine attaches to, written by \
                  `phux host add` and resolved by `phux attach <name>`; \
                  `color` overrides a host's automatic colour.",
    },
    Section {
        key: "host",
        header: "[host]",
        summary: "This machine as clients draw it: `color` overrides its \
                  automatic colour (`phux host color --local`).",
    },
    Section {
        key: "projects",
        header: "[[projects]]",
        summary: "Named projects `phux project open <name>` resolves: a \
                  checkout path and an optional out-of-repo recipe.",
    },
    Section {
        key: "sessions",
        header: "[[sessions]]",
        summary: "A colour for a session by name, drawn beside it in the \
                  sidebar: `color` is one of the host palette's ids.",
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
        key: "organize",
        header: "[organize]",
        summary: "Who judges where panes belong for `phux organize`: \
                  `TypeSafe`'s Jev with a key, else repository matching.",
    },
    Section {
        key: "restore",
        header: "[restore]",
        summary: "Whether a starting server relaunches the panes the last \
                  server stop ended, as `phux restore` does by hand.",
    },
    Section {
        key: "rune",
        header: "[rune]",
        summary: "Settings the Rune desktop app reads at launch; a key set \
                  here wins over the choice saved in the app.",
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
        "unset — `local`, and a configured remote listener, relay connector, \
         or workload authority location refuses to start the server. \
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
    (
        "organize.jev-key-command",
        "unset — `TYPESAFE_API_KEY` only. An argv whose trimmed stdout is \
         the key, run when the variable is unset",
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
         (the files under `crates/phux-config/src/default/`, joined in \
         order), verbatim. `phux config init` writes a fully-commented projection of it, and \
         `phux config show --default` prints it.\n\n\
         ```toml\n{DEFAULT_CONFIG_TOML}```\n"
    );

    Page {
        file: "config.md",
        title: "Config settings",
        summary: "Every `config.toml` section, the scalar defaults, and \
                  the annotated default config.",
        lede: "Every `config.toml` section and setting, with the shipped defaults.",
        body,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
