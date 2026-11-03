//! The service plan and the unit renderers: a `launchd` plist on macOS, a
//! systemd user unit on Linux, each a pure function of a [`ServicePlan`].

use std::path::PathBuf;

use super::{
    RESTART_THROTTLE_SECS, SERVICE_MANAGED_ENV, START_LIMIT_BURST, launchd_label_for, path_string,
};

#[cfg(test)]
pub(super) mod tests;

/// Everything the unit renderers need, resolved once at install time so the
/// renderers are pure functions.
#[derive(Debug, Clone)]
pub(crate) struct ServicePlan {
    /// Absolute path to the `phux` binary the unit runs (the init system's `PATH`
    /// may not find `phux`).
    pub(crate) binary: PathBuf,
    /// `HOST:PORT` for the QUIC listener, if the operator asked for one.
    pub(crate) quic: Option<String>,
    /// `HOST:PORT` for the WebSocket listener, if the operator asked for one.
    pub(crate) listen: Option<String>,
    /// Token store the server reads. Always materialized, never left to a
    /// default the init system's environment may not reproduce.
    pub(crate) tokens: PathBuf,
    /// TLS certificate the server presents on a routable bind.
    pub(crate) cert: PathBuf,
    /// TLS private key paired with `cert`.
    pub(crate) key: PathBuf,
    /// UDS path override. Only this, not [`Self::socket_path`], becomes
    /// `PHUX_SOCKET` in the unit, so a default-socket install stays portable.
    pub(crate) socket: Option<PathBuf>,
    /// Run the supervised server as a federation hub, loading and maintaining
    /// every enabled `[[satellites]]` route from `config.toml`.
    pub(crate) hub: bool,
    /// The socket the server will actually bind; the wrapper script polls it.
    pub(crate) socket_path: PathBuf,
    /// The active ADR-0080 profile when it is not the default, else `None`.
    pub(crate) profile: Option<String>,
    /// Where the service's stdout and stderr land.
    pub(crate) log: PathBuf,
    /// Workspace archive path when `--restore` is on; `Some` makes the unit run
    /// the save/restore wrapper script instead of the server directly.
    pub(crate) restore: Option<PathBuf>,
    /// Path of the generated wrapper script. Only read when `restore` is
    /// `Some`.
    pub(crate) wrapper: PathBuf,
}

impl ServicePlan {
    /// The server's environment, ordered so a regenerated unit is byte-identical.
    pub(super) fn environment(&self) -> Vec<(&'static str, String)> {
        let mut env = Vec::with_capacity(7);
        env.push((SERVICE_MANAGED_ENV, "1".to_owned()));
        if let Some(quic) = &self.quic {
            env.push(("PHUX_QUIC_ADDR", quic.clone()));
        }
        if let Some(listen) = &self.listen {
            env.push(("PHUX_WS_ADDR", listen.clone()));
        }
        env.push(("PHUX_WS_TOKENS", path_string(&self.tokens)));
        env.push(("PHUX_WS_TLS_CERT", path_string(&self.cert)));
        env.push(("PHUX_WS_TLS_KEY", path_string(&self.key)));
        if let Some(socket) = &self.socket {
            env.push(("PHUX_SOCKET", path_string(socket)));
        }
        env
    }

    /// The argv the unit executes: the server directly, or `sh` on the
    /// generated wrapper when `--restore` brackets it with save/restore.
    pub(super) fn program_arguments(&self) -> Vec<String> {
        if self.restore.is_some() {
            return vec!["/bin/sh".to_owned(), path_string(&self.wrapper)];
        }
        let mut args = vec![path_string(&self.binary), "server".to_owned()];
        if self.hub {
            args.push("--hub".to_owned());
        }
        args
    }
}

/// The launchd restart-policy keys, shared with [`reconcile_unit`] so the
/// reconciler decides "current" against exactly what the generator emits.
///
/// Restart on abnormal exit only (`SuccessfulExit: false`), so
/// `phux kill --server` stays down, and throttle restarts so a crash-loop is
/// visible rather than a silent respawn storm.
pub(super) fn launchd_policy_lines() -> Vec<String> {
    vec![
        "  <key>KeepAlive</key>".to_owned(),
        "  <dict>".to_owned(),
        "    <key>SuccessfulExit</key>".to_owned(),
        "    <false/>".to_owned(),
        "  </dict>".to_owned(),
        "  <key>ThrottleInterval</key>".to_owned(),
        format!("  <integer>{RESTART_THROTTLE_SECS}</integer>"),
        // Scheduling class (ADR-0096); owned here so `reconcile` migrates old
        // `Background` units.
        "  <key>ProcessType</key>".to_owned(),
        "  <string>Interactive</string>".to_owned(),
    ]
}

/// The `[Service]` half of the systemd spelling of [`launchd_policy_lines`].
pub(super) fn systemd_policy_lines() -> Vec<String> {
    vec![
        "Restart=on-failure".to_owned(),
        format!("RestartSec={RESTART_THROTTLE_SECS}s"),
    ]
}

/// The `[Unit]` half: start-rate limits are unit settings, and systemd ignores
/// them under `[Service]` (`Unknown key StartLimitIntervalSec`). The explicit
/// limit makes systemd give up on a real crash-loop: its default (5 starts in
/// 10s) can never trip at a 30s `RestartSec`.
pub(super) fn systemd_unit_policy_lines() -> Vec<String> {
    vec![
        format!(
            "StartLimitIntervalSec={}s",
            RESTART_THROTTLE_SECS * (START_LIMIT_BURST + 1)
        ),
        format!("StartLimitBurst={START_LIMIT_BURST}"),
    ]
}

/// The `[Service]` keys [`reconcile_unit`] owns in a systemd unit. Every
/// assignment of one is replaced by [`systemd_policy_lines`]; nothing else in
/// the file is touched.
pub(super) const SYSTEMD_POLICY_KEYS: [&str; 2] = ["Restart", "RestartSec"];

/// The `[Unit]` keys [`reconcile_unit`] owns, replaced by
/// [`systemd_unit_policy_lines`]. Older installs wrote them under
/// `[Service]`; the reconciler moves them.
pub(super) const SYSTEMD_UNIT_POLICY_KEYS: [&str; 2] = ["StartLimitIntervalSec", "StartLimitBurst"];

/// Render the launchd `LaunchAgent` plist. `ProcessType` is `Interactive`
/// because the server is on the keystroke path (ADR-0096).
pub(crate) fn render_launchd_plist(plan: &ServicePlan) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(1024);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    out.push_str("<plist version=\"1.0\">\n<dict>\n");
    out.push_str("  <!-- Generated by `phux service install` (ADR-0055). -->\n");
    out.push_str("  <!-- Edits are overwritten on the next install. -->\n");

    out.push_str("  <key>Label</key>\n");
    let _ = writeln!(
        out,
        "  <string>{}</string>",
        launchd_label_for(plan.profile.as_deref())
    );

    out.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    for arg in plan.program_arguments() {
        let _ = writeln!(out, "    <string>{}</string>", xml_escape(&arg));
    }
    out.push_str("  </array>\n");

    out.push_str("  <key>RunAtLoad</key>\n  <true/>\n");

    for line in launchd_policy_lines() {
        let _ = writeln!(out, "{line}");
    }

    let env = plan.environment();
    if !env.is_empty() {
        out.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (key, value) in env {
            let _ = writeln!(out, "    <key>{key}</key>");
            let _ = writeln!(out, "    <string>{}</string>", xml_escape(&value));
        }
        out.push_str("  </dict>\n");
    }

    let log = xml_escape(&path_string(&plan.log));
    out.push_str("  <key>StandardOutPath</key>\n");
    let _ = writeln!(out, "  <string>{log}</string>");
    out.push_str("  <key>StandardErrorPath</key>\n");
    let _ = writeln!(out, "  <string>{log}</string>");

    out.push_str("</dict>\n</plist>\n");
    out
}

/// Render the systemd user unit; the systemd spellings of the launchd keys.
pub(crate) fn render_systemd_unit(plan: &ServicePlan) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(768);
    out.push_str("# Generated by `phux service install` (ADR-0055).\n");
    out.push_str("# Edits are overwritten on the next install.\n\n");

    out.push_str("[Unit]\n");
    out.push_str("Description=phux terminal control plane server\n");
    out.push_str("Documentation=https://github.com/no-phux/phux\n");
    out.push_str("After=network-online.target\n");
    for line in systemd_unit_policy_lines() {
        let _ = writeln!(out, "{line}");
    }
    out.push('\n');

    out.push_str("[Service]\n");
    out.push_str("Type=simple\n");
    let _ = writeln!(
        out,
        "ExecStart={}",
        plan.program_arguments()
            .iter()
            .map(|arg| systemd_escape(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    for line in systemd_policy_lines() {
        let _ = writeln!(out, "{line}");
    }
    for (key, value) in plan.environment() {
        let _ = writeln!(out, "Environment=\"{key}={}\"", systemd_quote(&value));
    }
    let log = path_string(&plan.log);
    let _ = writeln!(out, "StandardOutput=append:{log}");
    let _ = writeln!(out, "StandardError=append:{log}\n");

    out.push_str("[Install]\n");
    out.push_str("WantedBy=default.target\n");
    out
}

/// Escape the five XML metacharacters; an unescaped `&` in a path makes a plist
/// launchd silently refuses.
pub(super) fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Undo [`xml_escape`] in one pass (so `&amp;amp;` decodes once).
pub(super) fn xml_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let decoded = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .into_iter()
        .find_map(|(entity, ch)| tail.strip_prefix(entity).map(|rest| (ch, rest)));
        if let Some((ch, remainder)) = decoded {
            out.push(ch);
            rest = remainder;
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Undo [`systemd_quote`] in one pass, like [`xml_unescape`].
pub(super) fn systemd_unquote(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        let escaped = match (ch, chars.peek()) {
            ('\\', Some('\\' | '"')) | ('%', Some('%')) | ('$', Some('$')) => chars.next(),
            _ => None,
        };
        out.push(escaped.unwrap_or(ch));
    }
    out
}

/// Escape an `ExecStart` argument for systemd's own unquoting pass.
///
/// systemd splits `ExecStart` on whitespace, so a path containing a space
/// must be quoted; backslashes and quotes inside it must then be escaped.
pub(super) fn systemd_escape(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ':' | '='))
    {
        return arg.to_owned();
    }
    format!("\"{}\"", systemd_quote(arg))
}

/// Escape what systemd treats specially inside a double-quoted
/// `ExecStart=`/`Environment=` value: `"` and `\`, plus `%` (specifier) and `$`
/// (variable) expansion, which apply regardless of quoting.
pub(super) fn systemd_quote(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$")
}
