//! The canonical table of environment variables the `phux` binary reads.
//!
//! Two surfaces render from it and so cannot disagree: the `phux help
//! environment` topic ([`environment_section`]) and the generated
//! `docs/reference/environment.md` page (`refdocs::environment`). It is a
//! table rather than prose for the same reason `exit_codes` is: a row is
//! one variable, and a renderer that wants a different layout reads the
//! rows rather than re-flowing a paragraph.

/// One documented environment variable: its name and its meaning, pre-broken
/// into help-width lines the way `exit_codes::ExitCodeSpec` carries its own.
pub(crate) struct EnvVarSpec {
    /// The variable, as the process reads it.
    pub(crate) name: &'static str,
    /// The meaning, broken for the fixed-width help layout; the markdown
    /// renderer joins the lines back into one cell.
    pub(crate) lines: &'static [&'static str],
}

/// Every environment variable the binary reads, in the order the topic
/// lists them: the socket first, then the remote listeners, then the
/// helper programs, then logging.
pub(crate) const ENV_VARS: &[EnvVarSpec] = &[
    EnvVarSpec {
        name: "PHUX_SOCKET",
        lines: &[
            "Server socket for the CLI verbs and the server. `--socket`",
            "overrides it. Default: $XDG_RUNTIME_DIR/phux/phux.sock, or",
            "/tmp/phux-$USER/phux.sock when XDG_RUNTIME_DIR is unset.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WS_ADDR",
        lines: &[
            "Also accept WebSocket clients on HOST:PORT. Equivalent to",
            "`phux server --listen`, which overrides it.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WS_SECURE",
        lines: &[
            "Force TLS and token auth on a loopback --listen address, to",
            "exercise the remote path locally.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WS_TLS_CERT",
        lines: &["Operator-supplied server certificate (PEM), instead of the"],
    },
    EnvVarSpec {
        name: "PHUX_WS_TLS_KEY",
        lines: &["auto-provisioned self-signed pair used off-loopback."],
    },
    EnvVarSpec {
        name: "PHUX_WS_TOKENS",
        lines: &["Pairing-token store the server reads and `phux pair` writes."],
    },
    EnvVarSpec {
        name: "PHUX_QUIC_ADDR",
        lines: &[
            "Also accept QUIC clients on HOST:PORT. Equivalent to",
            "`phux server --quic`, which overrides it.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WT_ADDR",
        lines: &[
            "Also accept WebTransport (HTTP/3 over QUIC) clients on",
            "HOST:PORT. Equivalent to `phux server --webtransport`.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_SSH",
        lines: &[
            "OpenSSH-compatible program used to reach ssh:// hosts and",
            "satellites (default: `ssh` on PATH).",
        ],
    },
    EnvVarSpec {
        name: "PHUX_TAILSCALE",
        lines: &[
            "Tailscale-compatible CLI used to detect the overlay address",
            "(default: `tailscale` on PATH) for `phux pair`, `phux",
            "doctor`, and the server's auto-bound remote listener. When",
            "set it is the only source consulted: naming a command that",
            "reports nothing turns overlay detection off everywhere.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE",
        lines: &[
            "Idle limit in seconds (1..=86400) for an auto-spawned",
            "server, as if started with `phux server --exit-after-idle`.",
            "Unset means no limit. For test harnesses and CI jobs that",
            "cannot guarantee their own cleanup runs.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_LOG",
        lines: &[
            "Write logs to this file (the server tees to it; the client",
            "writes only here).",
        ],
    },
    EnvVarSpec {
        name: "PHUX_LOG_FORMAT",
        lines: &["`text` (default) or `json`: the log line format."],
    },
    EnvVarSpec {
        name: "RUST_LOG",
        lines: &["tracing level filter, e.g. `phux=debug`."],
    },
    EnvVarSpec {
        name: "NO_COLOR",
        lines: &["Set (non-empty) to keep colour out of help output."],
    },
    EnvVarSpec {
        name: "CLICOLOR_FORCE",
        lines: &["Set (not `0`) to colour help output even when piped."],
    },
];

/// The width of the name column in [`environment_section`]: long enough
/// for every name but one, which gets its meaning on the following lines.
const NAME_COLUMN: usize = 18;

/// Render the `phux help environment` topic from [`ENV_VARS`].
pub(crate) fn environment_section() -> String {
    use std::fmt::Write as _;

    let mut section = String::from("ENVIRONMENT\n");
    for spec in ENV_VARS {
        let mut lines = spec.lines.iter();
        let first = lines.next().copied().unwrap_or_default();
        if spec.name.len() >= NAME_COLUMN {
            let _ = writeln!(section, "  {}", spec.name);
            let _ = writeln!(section, "  {:<NAME_COLUMN$}{first}", "");
        } else {
            let _ = writeln!(section, "  {:<NAME_COLUMN$}{first}", spec.name);
        }
        for line in lines {
            let _ = writeln!(section, "  {:<NAME_COLUMN$}{line}", "");
        }
    }
    section.push_str(
        "\nRun `phux server --listen 127.0.0.1:8787` to expose a port; see\n\
         `phux server --help` for the remote and TLS details.\n",
    );
    section
}

#[cfg(test)]
mod tests {
    use super::{ENV_VARS, environment_section};

    /// Every variable lands on its own line, in table order, and the
    /// rendered block stays inside an 80-column terminal.
    #[test]
    fn section_lists_every_variable_within_eighty_columns() {
        let section = environment_section();
        let mut last = 0;
        for spec in ENV_VARS {
            let at = section
                .find(&format!("  {}", spec.name))
                .unwrap_or_else(|| panic!("{} is missing from the topic", spec.name));
            assert!(at >= last, "{} is out of table order", spec.name);
            last = at;
        }
        for line in section.lines() {
            assert!(line.len() <= 80, "wider than 80 columns: {line:?}");
        }
    }

    /// No two rows describe one variable, and every row says something.
    #[test]
    fn table_rows_are_unique_and_non_empty() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in ENV_VARS {
            assert!(seen.insert(spec.name), "{} listed twice", spec.name);
            assert!(!spec.lines.is_empty(), "{} has no meaning", spec.name);
        }
    }
}
