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

/// The variable the server injects into every pane: that pane's local wire
/// id, written as bare digits (`5`) so shell hooks can prefix `@` themselves.
pub(crate) const TERMINAL_ID_ENV: &str = "PHUX_TERMINAL_ID";

/// The pane this process runs in, from [`TERMINAL_ID_ENV`]; `None` outside a
/// pane. Reads both the canonical `5` and the `@5` spelling older guardians
/// wrote, and never the reserved id 0.
pub(crate) fn own_terminal_id() -> Option<u32> {
    parse_terminal_id(&std::env::var(TERMINAL_ID_ENV).ok()?)
}

/// [`own_terminal_id`], when that pane is on the server at `socket`: a pane
/// id means nothing on another server, so one that `PHUX_SOCKET` says this
/// process does not run on is `None`.
pub(crate) fn own_terminal_id_on(socket: &std::path::Path) -> Option<u32> {
    let same = std::env::var_os("PHUX_SOCKET")
        .filter(|own| !own.is_empty())
        .is_none_or(|own| same_socket(own.as_ref(), socket));
    own_terminal_id().filter(|_| same)
}

/// Whether two socket paths name one socket (`/tmp` is a link on macOS).
fn same_socket(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

fn parse_terminal_id(raw: &str) -> Option<u32> {
    let digits = raw.strip_prefix('@').unwrap_or(raw);
    digits.parse::<u32>().ok().filter(|id| *id != 0)
}

/// The deadline on every server exchange; see [`deadline_from`].
pub(crate) const TIMEOUT_ENV: &str = "PHUX_TIMEOUT";

/// The server-exchange deadline from a [`TIMEOUT_ENV`] value: whole or
/// fractional seconds, `0` for none. Unset or unparseable is the 10-second
/// default, so a typo never uncaps the wait.
pub(crate) fn deadline_from(raw: Option<&str>) -> Option<std::time::Duration> {
    const DEFAULT: std::time::Duration = std::time::Duration::from_secs(10);
    let parsed = raw
        .and_then(|raw| raw.trim().trim_end_matches('s').parse::<f64>().ok())
        .and_then(|secs| std::time::Duration::try_from_secs_f64(secs).ok());
    match parsed {
        Some(deadline) if deadline.is_zero() => None,
        Some(deadline) => Some(deadline),
        None => Some(DEFAULT),
    }
}

/// Every environment variable the binary reads, in the order the topic
/// lists them: the socket first, then the remote listeners, then the
/// helper programs, then logging.
pub(crate) const ENV_VARS: &[EnvVarSpec] = &[
    EnvVarSpec {
        name: "PHUX_SOCKET",
        lines: &[
            "Server socket for the CLI verbs and the server. `--socket`",
            "overrides it. Default: $XDG_RUNTIME_DIR/phux/phux.sock; when",
            "XDG_RUNTIME_DIR is unset, /run/user/<uid>/phux/phux.sock if",
            "that directory exists, else /tmp/phux-$USER/phux.sock.",
        ],
    },
    EnvVarSpec {
        name: TIMEOUT_ENV,
        lines: &[
            "Seconds to wait for the server to answer the handshake and",
            "each request (default 10; `0` waits forever). On expiry the",
            "verb exits 4. Streams (attach, watch, forward) are bounded",
            "only until their handshake.",
        ],
    },
    EnvVarSpec {
        name: phux_client::conversation::CONTEXT_ENV,
        lines: &[
            "Private record selected by `phux chat run` (absolute path).",
            "Scopes mail only, never path claims or terminal authority.",
            "A bad record fails closed; no fallback to another store.",
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
        name: "PHUX_WS_ALLOWED_ORIGINS",
        lines: &[
            "Browser origins a plaintext loopback --listen address admits",
            "beyond loopback pages: comma-separated exact origins, or `*`",
            "behind a proxy that checks origins itself.",
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
        name: "PHUX_NO_AUTO_LISTEN",
        lines: &[
            "Set (to anything) to keep the server from auto-binding a",
            "remote listener on the private network address.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WORKLOAD_CA",
        lines: &[
            "The server's workload CA certificate. Default:",
            "workload-ca.pem in the state directory. With this,",
            "PHUX_WORKLOAD_CA_KEY, or PHUX_WORKLOAD_KEYS set,",
            "`[policy] mode` must be set too.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WORKLOAD_CA_KEY",
        lines: &[
            "Its private key. Default: workload-ca.key in the state",
            "directory.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WORKLOAD_KEYS",
        lines: &[
            "Registry of the client keys the server admits. Default:",
            "workload-keys in the state directory. enrollment-tickets and",
            "workload-org-roots/ move with it.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WORKLOAD_CERT",
        lines: &[
            "Client certificate (PEM) a remote dial presents when the",
            "host entry names none. Set with PHUX_WORKLOAD_KEY; one",
            "alone is an error.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_WORKLOAD_KEY",
        lines: &["Private key of PHUX_WORKLOAD_CERT."],
    },
    EnvVarSpec {
        name: "PHUX_WORKLOAD_REQUIRE_PAIRED",
        lines: &[
            "Set (to anything) to refuse a server that does not request",
            "the PHUX_WORKLOAD_CERT identity.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_AGENT_DETECT",
        lines: &["Set to `0` to turn agent detection off."],
    },
    EnvVarSpec {
        name: "PHUX_AGENT_RULES_DIR",
        lines: &[
            "Directory of *.toml agent manifests that override or extend",
            "the built-in ones; empty means none. Default:",
            "$XDG_CONFIG_HOME/phux/agent-rules, else",
            "~/.config/phux/agent-rules.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_UPLOAD_DIR",
        lines: &[
            "Where the server lands file uploads (PUT_FILE). Default:",
            "$XDG_DATA_HOME/phux/uploads, else",
            "~/.local/share/phux/uploads.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_UPLOAD_MAX_BYTES",
        lines: &[
            "Total bytes of finished and partial uploads the server",
            "keeps before refusing more (default 8 GiB; 0 = no limit).",
        ],
    },
    EnvVarSpec {
        name: "PHUX_UPLOAD_MAX_FILES",
        lines: &[
            "Uploads, finished or partial, the server keeps before",
            "refusing a new one (default 10000; 0 = no limit).",
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
        name: "PHUX_OVERLAY_ADDRS",
        lines: &[
            "Comma-separated concrete unicast IPs for any private network",
            "(e.g. Defguard/WireGuard). Overrides overlay discovery for",
            "pair, doctor, and auto-listen. Empty or invalid disables",
            "discovery without fallback. First IP is the auto-bind IP.",
            "Explicit --listen/--quic still win. Set in the service env",
            "and pairing shell; this does not install or configure a VPN.",
            "Set, it also keeps `phux pair` links off MagicDNS names.",
        ],
    },
    EnvVarSpec {
        name: "PHUX_TAILSCALE",
        lines: &[
            "Tailscale-compatible CLI used to detect the overlay address",
            "(default: `tailscale` on PATH) for `phux pair`, `phux",
            "doctor`, and the server's auto-bound remote listener. When",
            "set and PHUX_OVERLAY_ADDRS is unset, it is the only source:",
            "a command reporting nothing disables overlay discovery.",
            "`phux pair` also reads its `status --json` for the host's",
            "MagicDNS name, which its links name instead of the address.",
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
        name: "PHUX_ALLOW_NESTED",
        lines: &[
            "Set (non-empty) to let `phux`, `phux attach`, and `phux new`",
            "attach from inside one of the same server's panes, which",
            "they otherwise refuse (exit 2).",
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
        name: "PHUX_DEV",
        lines: &[
            "Set to `1`, `true`, `yes`, or `on` to enable dev telemetry:",
            "server heap accounting. Off by default.",
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
        lines: &[
            "Set (non-empty) to keep human output plain, including help",
            "and diagnostics. Overrides CLICOLOR_FORCE.",
        ],
    },
    EnvVarSpec {
        name: "CLICOLOR_FORCE",
        lines: &[
            "Set (non-empty, not `0`) to colour human output even when",
            "piped. NO_COLOR and TERM=dumb still win.",
        ],
    },
    EnvVarSpec {
        name: "CLICOLOR",
        lines: &[
            "Set to `0` to disable colour in human output unless forced.",
            "Otherwise colour follows each destination stream's terminal.",
        ],
    },
    EnvVarSpec {
        name: "TERM",
        lines: &["Set to `dumb` to keep human output plain, even when forced."],
    },
];

/// The width of the name column in [`environment_section`]: a longer
/// name gets its meaning on the following lines.
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
        "\nRun `phux server --listen 127.0.0.1:8787` to expose a port, with\n\
         `[policy] mode = \"paired\"` in config.toml; see `phux server --help`\n\
         for the remote and TLS details.\n",
    );
    section
}

#[cfg(test)]
mod tests {
    use super::{ENV_VARS, NAME_COLUMN, deadline_from, environment_section};

    #[test]
    fn deadline_reads_seconds_and_zero_disables() {
        use std::time::Duration;
        let ten = Some(Duration::from_secs(10));
        assert_eq!(deadline_from(None), ten);
        assert_eq!(deadline_from(Some("nope")), ten);
        assert_eq!(deadline_from(Some("-1")), ten);
        assert_eq!(deadline_from(Some("0")), None);
        assert_eq!(deadline_from(Some("3")), Some(Duration::from_secs(3)));
        assert_eq!(
            deadline_from(Some("1.5s")),
            Some(Duration::from_millis(1500))
        );
    }

    /// Every variable lands on its own line, in table order, and the
    /// rendered block stays inside an 80-column terminal.
    #[test]
    fn section_lists_every_variable_within_eighty_columns() {
        let section = environment_section();
        let mut last = 0;
        for spec in ENV_VARS {
            let at = section
                .find(&format!(
                    "  {}{}",
                    spec.name,
                    if spec.name.len() >= NAME_COLUMN {
                        "\n"
                    } else {
                        " "
                    }
                ))
                .unwrap_or_else(|| panic!("{} is missing from the topic", spec.name));
            assert!(at >= last, "{} is out of table order", spec.name);
            last = at;
        }
        for line in section.lines() {
            assert!(line.len() <= 80, "wider than 80 columns: {line:?}");
        }
    }
}

#[cfg(test)]
mod terminal_id_tests {
    use super::parse_terminal_id;

    #[test]
    fn terminal_id_reads_bare_and_at_prefixed_ids_only() {
        assert_eq!(parse_terminal_id("5"), Some(5));
        assert_eq!(parse_terminal_id("@5"), Some(5));
        for invalid in ["", "@", "0", "@0", "x", "@@5", "5x", "host/@5"] {
            assert_eq!(parse_terminal_id(invalid), None, "{invalid:?}");
        }
    }
}
