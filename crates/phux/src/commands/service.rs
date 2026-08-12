//! `phux service` — generate and manage the per-user service unit that keeps
//! a phux server running across logout and reboot (ADR-0055).
//!
//! The naked `phux` path auto-spawns a server when the socket is missing,
//! which covers a cold client but not a cold *host*: a rebooted machine has
//! no server until someone logs in and runs one. This module closes that gap
//! by generating the host's native unit — a `launchd` `LaunchAgent` on macOS, a
//! systemd **user** unit on Linux — with the server's environment
//! materialized into it.
//!
//! The environment wiring is the whole reason this is code rather than a
//! documented snippet. A hand-written unit that omits `PHUX_WS_TOKENS`
//! silently starts a server that rejects every paired device, and the
//! operator discovers it days later from a laptop that will not attach.
//!
//! Scope is per-user by construction (ADR-0003: one server per user). A
//! system-wide `LaunchDaemon` or system systemd unit would imply a multi-user
//! server, which phux does not have.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::socket::{self, SocketState};

/// launchd's reverse-DNS job label for the default profile, and the basename
/// of the plist it loads.
const LAUNCHD_LABEL: &str = "com.phux.server";

/// systemd's unit name for the default profile. `--user` scope, so it lives
/// under `$XDG_CONFIG_HOME/systemd/user/`.
const SYSTEMD_UNIT: &str = "phux.service";

/// launchd's job label for the *active* profile.
///
/// The default profile keeps the bare `com.phux.server`, for the same reason
/// [`phux_config::instance::DEFAULT_PROFILE`] is stored unsuffixed on disk: a
/// user upgrading into a profile-aware build must not end up with their
/// already-loaded job orphaned under a name nothing addresses.
///
/// Every other profile is suffixed. Without this, ADR-0080's isolation was
/// half-applied: `resolve_plan` already scopes the socket, state and log paths
/// by profile, so a dev-profile `service install` wrote a unit pointing at
/// `phux-dev` locations — but filed under the *production* label, silently
/// replacing the job that supervises the user's real server (phux-gyza).
fn launchd_label() -> String {
    launchd_label_for(profile_suffix().as_deref())
}

/// systemd's unit name for the active profile. See [`launchd_label`] for why
/// the default profile keeps the bare name.
fn systemd_unit() -> String {
    systemd_unit_for(profile_suffix().as_deref())
}

/// [`launchd_label`] with the profile injected, so tests can drive both the
/// default and a named profile without mutating the process environment
/// (`env::set_var` is unsafe under edition 2024 and this crate forbids
/// unsafe). Same `*_from` idiom as [`home_dir_from`].
fn launchd_label_for(profile: Option<&str>) -> String {
    profile.map_or_else(
        || LAUNCHD_LABEL.to_owned(),
        |profile| format!("{LAUNCHD_LABEL}.{profile}"),
    )
}

/// [`systemd_unit`] with the profile injected. See [`launchd_label_for`].
fn systemd_unit_for(profile: Option<&str>) -> String {
    profile.map_or_else(
        || SYSTEMD_UNIT.to_owned(),
        |profile| format!("phux-{profile}.service"),
    )
}

/// The active profile when it is not the default, else `None`.
///
/// One place resolves it so the label, the unit name and the refusal message
/// cannot disagree about which profile they are talking about.
fn profile_suffix() -> Option<String> {
    (!phux_config::instance::is_default_profile()).then(phux_config::instance::profile)
}

/// Minimum seconds between supervised restarts (phux-zomb.4).
///
/// launchd `ThrottleInterval` / systemd `RestartSec`. Chosen to make a
/// crash-loop *legible* rather than to minimise downtime: at one start per
/// 30s a human notices, `phux doctor` can count the restarts, and the log
/// stays readable. The previous 500 ms floor (and launchd's unthrottled
/// default) produced thousands of generations that buried the first failure —
/// the one that explains all the others.
const RESTART_THROTTLE_SECS: u32 = 30;

/// How many consecutive failed starts systemd tolerates before it stops
/// retrying and leaves the unit failed.
///
/// Matches the threshold `phux doctor`'s `server-health` check already calls a
/// crash-loop, so the two agree on what "this is not coming back" means: by
/// the time systemd gives up, doctor is already reporting it. launchd has no
/// equivalent knob -- it retries forever regardless -- which is why
/// `run_install` refuses up front rather than relying on the supervisor to
/// notice (phux-67wg).
const START_LIMIT_BURST: u32 = 5;

/// Marker `phux service install` stamps into the unit's OWN
/// `EnvironmentVariables` (launchd) / `Environment=` (systemd) block, and
/// [`crate::commands::server::run_server`] reads back at startup to decide
/// whether server-spawned panes need login-shell treatment (phux-87rr).
///
/// This is the reliable half of "reliable, not a heuristic": launchd and
/// systemd both start their unit with a minimal environment that never ran
/// a login shell, so profile-provided `PATH` entries (Homebrew, Nix) are
/// invisible to every pane — but environment markers like `NIX_PROFILES`
/// can still be inherited from whatever *built* the unit, which is exactly
/// what makes sniffing "is my PATH short" or "is my parent launchd"
/// unreliable: both can be true, or false, independent of how this
/// specific server process was actually started. A value this code itself
/// wrote into the unit at install time, and only there, has no such
/// ambiguity — a server without it was not started from a unit this
/// `phux` ever wrote, full stop.
pub(crate) const SERVICE_MANAGED_ENV: &str = "PHUX_SERVICE_MANAGED";

/// Which init system this host's unit targets.
///
/// Resolved from the compile target rather than probed at runtime: a macOS
/// build has launchd and a Linux build has systemd-or-nothing, and guessing
/// from `/proc` would only add a failure mode. Both renderers are compiled
/// on every platform so their tests run everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Manager {
    Launchd,
    Systemd,
}

impl Manager {
    /// The manager for the host we were built for, or `None` on a platform
    /// with no generator — those get a printed unit and a manual
    /// instruction, never a hard error.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "None is reachable on targets that are neither macOS nor Linux; clippy only sees the active cfg"
    )]
    const fn host() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Some(Self::Launchd)
        }
        #[cfg(target_os = "linux")]
        {
            Some(Self::Systemd)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// Where the unit file for this manager belongs.
    ///
    /// Fallible: with `HOME` (and, for systemd, `XDG_CONFIG_HOME`) unset,
    /// the naive join used to produce a *relative* path — `Library/...` or
    /// `.config/...` — and every caller would then create directories and
    /// write the unit under whatever the current working directory
    /// happened to be, silently. Fail here instead, at the one place that
    /// knows why.
    /// `profile` is the ADR-0080 profile suffix (`None` for the default), so
    /// the basename is scoped exactly the way the label inside the unit is.
    fn unit_path(self, profile: Option<&str>) -> Result<PathBuf, String> {
        match self {
            Self::Launchd => Ok(home_dir()?
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{}.plist", launchd_label_for(profile)))),
            Self::Systemd => Ok(config_home()?
                .join("systemd")
                .join("user")
                .join(systemd_unit_for(profile))),
        }
    }
}

/// Everything the unit renderers need, resolved once at install time.
///
/// Held as plain data — no environment reads, no filesystem access — so
/// [`render_launchd_plist`], [`render_systemd_unit`], and
/// [`render_wrapper_script`] are pure functions that tests can drive on any
/// platform with any combination of options.
#[derive(Debug, Clone)]
pub(crate) struct ServicePlan {
    /// Absolute path to the `phux` binary the unit runs. Resolved from
    /// `current_exe` at install time and baked in: a unit that says `phux`
    /// depends on a `PATH` the init system does not necessarily share.
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
    /// UDS path override, when the operator runs a non-default socket. Only
    /// this — not [`Self::socket_path`] — becomes `PHUX_SOCKET` in the unit,
    /// so a default-socket install stays portable across a host whose
    /// `XDG_RUNTIME_DIR` changes.
    pub(crate) socket: Option<PathBuf>,
    /// Run the supervised server as a federation hub, loading and maintaining
    /// every enabled `[[satellites]]` route from `config.toml`.
    pub(crate) hub: bool,
    /// The socket the server will actually bind, resolved at install time.
    ///
    /// The wrapper script tests this path to learn when the server is
    /// listening. Resolving it here rather than in `sh` keeps one
    /// implementation of the precedence rules
    /// (`$PHUX_SOCKET` > `$XDG_RUNTIME_DIR` > `/tmp/phux-$USER`).
    pub(crate) socket_path: PathBuf,
    /// The active ADR-0080 profile when it is not the default, else `None`.
    ///
    /// Resolved once here, with every other path, so the renderers stay pure:
    /// the label a plist carries and the basename the unit is written under
    /// both come from this field rather than from the ambient environment,
    /// which is what lets a test drive either profile (phux-gyza).
    pub(crate) profile: Option<String>,
    /// Where the service's stdout and stderr land.
    pub(crate) log: PathBuf,
    /// Workspace archive path when `--restore` is on. `Some` switches the
    /// unit from running the server directly to running the wrapper script
    /// that brackets it with save/restore.
    pub(crate) restore: Option<PathBuf>,
    /// Path of the generated wrapper script. Only read when `restore` is
    /// `Some`.
    pub(crate) wrapper: PathBuf,
}

impl ServicePlan {
    /// The server's environment, as ordered key/value pairs.
    ///
    /// Ordered (not a map) so a regenerated unit is byte-identical to the
    /// last one for the same inputs — an install that reshuffles keys looks
    /// like a real change in `diff` and in version control.
    fn environment(&self) -> Vec<(&'static str, String)> {
        let mut env = Vec::with_capacity(7);
        // Unconditional (phux-87rr): the marker the server reads to know it
        // was started from a unit this `phux` wrote, and therefore needs
        // login-shell treatment for its spawned panes. See
        // `SERVICE_MANAGED_ENV`'s doc for why this is the reliable signal
        // rather than a heuristic sniffed from the process environment.
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
    fn program_arguments(&self) -> Vec<String> {
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

/// The launchd keys that carry the ADR-0080 restart policy, exactly as the
/// generator emits them.
///
/// Shared with [`reconcile_unit`] rather than written twice: the reconciler
/// decides a unit is current by patching it and finding nothing changed, so a
/// generator and a reconciler that disagree by one byte would make `phux
/// service reconcile` rewrite the file `phux service install` had just
/// written, on every run, forever. One definition removes the failure mode
/// instead of testing for it.
///
/// phux-zomb.4: restart on ABNORMAL exit only, and rate-limit it.
///
/// `KeepAlive: true` — what this generator used to emit — restarts on
/// *every* exit at full speed. Two consequences, both observed in the
/// field: `phux kill --server` (a clean exit) came straight back, so a
/// server could not be stopped; and a server crashing at startup produced
/// a silent respawn storm (1487 generations against one log on one
/// machine) that made a dead server look like a running one.
///
/// `SuccessfulExit: false` restarts only when the server exits non-zero or
/// on a signal, so a deliberate shutdown stays down. `ThrottleInterval`
/// holds launchd to one start per 30s, which turns a crash-loop into
/// something a human — and `phux doctor` — can see rather than a firehose.
fn launchd_policy_lines() -> Vec<String> {
    vec![
        "  <key>KeepAlive</key>".to_owned(),
        "  <dict>".to_owned(),
        "    <key>SuccessfulExit</key>".to_owned(),
        "    <false/>".to_owned(),
        "  </dict>".to_owned(),
        "  <key>ThrottleInterval</key>".to_owned(),
        format!("  <integer>{RESTART_THROTTLE_SECS}</integer>"),
    ]
}

/// The systemd spelling of [`launchd_policy_lines`], and shared with
/// [`reconcile_unit`] for the same reason.
///
/// `StartLimitIntervalSec`/`StartLimitBurst` make systemd give up eventually.
/// Its default rate limit is 5 starts in 10s, which at a `RestartSec` of 30s
/// can never trip — so without these a server that fails every start retries
/// forever (phux-67wg). The window is sized to admit the throttle:
/// `START_LIMIT_BURST` starts spaced `RESTART_THROTTLE_SECS` apart fit inside
/// it, so a genuine crash-loop is caught while an occasional restart is not.
fn systemd_policy_lines() -> Vec<String> {
    vec![
        "Restart=on-failure".to_owned(),
        format!("RestartSec={RESTART_THROTTLE_SECS}s"),
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
const SYSTEMD_POLICY_KEYS: [&str; 4] = [
    "Restart",
    "RestartSec",
    "StartLimitIntervalSec",
    "StartLimitBurst",
];

/// Render the launchd `LaunchAgent` plist.
///
/// `RunAtLoad` starts the server when the agent is bootstrapped (login, and
/// boot when the host auto-logs-in); `KeepAlive` restarts it on any exit.
/// `ProcessType` is `Background` so the scheduler does not treat a server
/// with no attached client as an idle GUI app.
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

    // The restart policy (phux-zomb.4) lives in `launchd_policy_lines` so the
    // reconciler and the generator cannot drift; its doc explains the policy.
    for line in launchd_policy_lines() {
        let _ = writeln!(out, "{line}");
    }
    out.push_str("  <key>ProcessType</key>\n  <string>Background</string>\n");

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

/// Render the systemd user unit.
///
/// `Restart=on-failure` is launchd's `KeepAlive{SuccessfulExit:false}`;
/// `WantedBy=default.target` is its `RunAtLoad`; `RestartSec` is its
/// `ThrottleInterval`. See the launchd renderer for why a clean exit must
/// stay down and why the restart is rate-limited (phux-zomb.4).
pub(crate) fn render_systemd_unit(plan: &ServicePlan) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(768);
    out.push_str("# Generated by `phux service install` (ADR-0055).\n");
    out.push_str("# Edits are overwritten on the next install.\n\n");

    out.push_str("[Unit]\n");
    out.push_str("Description=phux terminal control plane server\n");
    out.push_str("Documentation=https://github.com/phall1/phux\n");
    out.push_str("After=network-online.target\n\n");

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

/// Render the `--restore` wrapper script.
///
/// launchd has no `ExecStartPost`/`ExecStopPre` equivalent, so bracketing the
/// server with save/restore needs a wrapper on macOS. Using the same wrapper
/// under systemd — which does have those directives — keeps one code path and
/// one thing to test, and keeps the two units' observable behavior identical.
///
/// The restore half polls for the socket rather than sleeping a fixed
/// interval: the server is ready when its socket exists, and a fixed sleep is
/// either a slow boot or a lost restore. The save half runs on `TERM`, which
/// is what both managers send first on stop.
///
/// Mirrors the continuum example plugin's write-to-temp-then-`mv` idiom so a
/// crash mid-save cannot truncate the last good archive.
pub(crate) fn render_wrapper_script(plan: &ServicePlan) -> String {
    let Some(archive) = &plan.restore else {
        return String::new();
    };
    let binary = sh_quote(&path_string(&plan.binary));
    let archive = sh_quote(&path_string(archive));
    let socket = sh_quote(&path_string(&plan.socket_path));
    let socket_arg = plan.socket.as_ref().map_or_else(String::new, |socket| {
        format!(" --socket {}", sh_quote(&path_string(socket)))
    });
    let hub_arg = if plan.hub { " --hub" } else { "" };

    format!(
        "#!/bin/sh\n\
         # Generated by `phux service install --restore` (ADR-0055).\n\
         # Edits are overwritten on the next install.\n\
         #\n\
         # Restores session names, layout, and cwd on start; saves them on\n\
         # stop. It does NOT restore running processes — they died with the\n\
         # host. Restored panes are fresh shells in the right directories.\n\
         set -u\n\
         \n\
         phux={binary}\n\
         archive={archive}\n\
         socket={socket}\n\
         \n\
         # Save is best-effort on every path: a stop that cannot reach the\n\
         # server must still stop, and a half-written archive must never\n\
         # replace the last good one (write-temp-then-mv, as the continuum\n\
         # example plugin does).\n\
         save() {{\n\
         \x20   [ -S \"$socket\" ] || return 0\n\
         \x20   \"$phux\" workspace save{socket_arg} --output \"$archive.tmp\" \\\n\
         \x20       && mv -f \"$archive.tmp\" \"$archive\"\n\
         }}\n\
         \n\
         # Trap before the server exists so a stop during startup is still\n\
         # handled; `kill` on an unset server is a no-op we tolerate.\n\
         server=''\n\
         trap 'save; [ -n \"$server\" ] && kill -TERM \"$server\" 2>/dev/null; \
         wait \"$server\" 2>/dev/null; exit 0' TERM INT\n\
         \n\
         \"$phux\" server{hub_arg}{socket_arg} &\n\
         server=$!\n\
         \n\
         # Restore once the server is listening. Bounded at ~10s so a server\n\
         # that never comes up fails the restore instead of hanging the unit.\n\
         if [ -f \"$archive\" ]; then\n\
         \x20   waited=0\n\
         \x20   while [ ! -S \"$socket\" ] && [ \"$waited\" -lt 100 ]; do\n\
         \x20       sleep 0.1\n\
         \x20       waited=$((waited + 1))\n\
         \x20   done\n\
         \x20   if [ -S \"$socket\" ]; then\n\
         \x20       \"$phux\" workspace restore \"$archive\"{socket_arg} || true\n\
         \x20   fi\n\
         fi\n\
         \n\
         wait \"$server\"\n"
    )
}

// ---------------------------------------------------------------------------
// In-place reconcile (phux-l1yx / phux-bd30)
// ---------------------------------------------------------------------------
//
// A unit written before phux-zomb.4 keeps its unthrottled restart-on-any-exit
// policy until something rewrites it. Until now the only "something" was
// `phux service install`, and that is a bad trade for two independent reasons:
//
//   1. It re-renders the unit from a *fresh* `ServicePlan`. `--quic`,
//      `--listen`, `--restore`, `--hub` and `--socket` survive only inside the
//      rendered unit — nothing parses one back — so a blind re-run silently
//      drops the operator's listeners and hub mode.
//   2. It reloads. `launchctl bootout` and `systemctl enable --now` stop the
//      supervised server, and every pane and its in-flight shells, agents and
//      subagents die with it (phux-nvi2).
//
// So the reconcile does neither. It reads the installed file, replaces only
// the keys that carry the restart policy, and leaves every other byte exactly
// where it was — which makes obstacle 1 structurally impossible rather than
// carefully avoided, since the flags are never re-derived at all.

/// What reconciling an installed unit's restart policy would do to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reconcile {
    /// The file already carries the current policy, byte for byte. Nothing to
    /// write. This is not detected by looking for markers — it is what falls
    /// out when the patch produces the input unchanged, so "current" can never
    /// mean anything other than "what this binary would write".
    Current,
    /// The patched file. Every byte outside the policy keys is the operator's.
    Patched(String),
    /// The file is not a shape this can rewrite without guessing.
    ///
    /// Refusing is the whole point: a mis-scoped edit produces a unit the init
    /// system silently declines to load, which is strictly worse than the
    /// legacy policy this was trying to fix.
    Unrecognized(&'static str),
}

/// Rewrite `body`'s restart-policy keys to the current policy.
///
/// Pure: no environment, no filesystem, no `ServicePlan`. That is what lets it
/// run over a unit generated by a *different* build, with flags this process
/// knows nothing about, and still be safe.
pub(crate) fn reconcile_unit(manager: Manager, body: &str) -> Reconcile {
    match manager {
        Manager::Launchd => reconcile_launchd(body),
        Manager::Systemd => reconcile_systemd(body),
    }
}

/// `Current` when the patch was a no-op, `Patched` otherwise.
fn settled(original: &str, patched: &[String]) -> Reconcile {
    let patched = patched.join("\n");
    if patched == original {
        Reconcile::Current
    } else {
        Reconcile::Patched(patched)
    }
}

/// Replace the `KeepAlive` and `ThrottleInterval` entries of a plist's
/// top-level dict, preserving every other entry and its formatting.
fn reconcile_launchd(body: &str) -> Reconcile {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut kept: Vec<String> = Vec::with_capacity(lines.len() + 8);
    // Where the first policy key stood, so the replacement lands in the same
    // place and a unit this binary generated reconciles to itself byte for
    // byte (`the_generated_units_reconcile_to_themselves`).
    let mut anchor: Option<usize> = None;

    // Nesting depth, so only the *top-level* dict's entries are candidates. A
    // `<key>KeepAlive</key>` inside `EnvironmentVariables` would be an
    // environment variable of that name, not the restart policy, and rewriting
    // it would corrupt the unit while leaving the real policy untouched.
    let mut depth = 0_usize;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if depth == 1
            && (trimmed == "<key>KeepAlive</key>" || trimmed == "<key>ThrottleInterval</key>")
        {
            // The value element is balanced, so skipping it leaves `depth`
            // exactly where it was.
            let Some(end) = plist_value_end(&lines, index + 1) else {
                return Reconcile::Unrecognized(
                    "its KeepAlive/ThrottleInterval value is a shape this cannot rewrite safely",
                );
            };
            let _ = anchor.get_or_insert(kept.len());
            index = end;
            continue;
        }
        if trimmed == "<dict>" || trimmed == "<array>" {
            depth += 1;
        } else if trimmed == "</dict>" || trimmed == "</array>" {
            depth = depth.saturating_sub(1);
        }
        kept.push(line.to_owned());
        index += 1;
    }

    // No policy keys at all. A plist dict is unordered, so appending at the end
    // of the top-level dict is as valid as anywhere else and needs no guess
    // about where the operator would have wanted it.
    let anchor = if let Some(at) = anchor {
        at
    } else {
        let Some(plist_end) = kept.iter().rposition(|line| line.trim() == "</plist>") else {
            return Reconcile::Unrecognized("it does not close a <plist> element");
        };
        let Some(dict_end) = kept
            .iter()
            .take(plist_end)
            .rposition(|line| line.trim() == "</dict>")
        else {
            return Reconcile::Unrecognized("it has no top-level <dict> to carry the policy");
        };
        dict_end
    };

    for (offset, line) in launchd_policy_lines().into_iter().enumerate() {
        kept.insert(anchor + offset, line);
    }
    settled(body, &kept)
}

/// Index just past the plist value element starting at or after `start`, or
/// `None` when it is a shape [`reconcile_launchd`] must not touch.
///
/// Covers what launchd units actually contain: a self-closing scalar
/// (`<true/>`), a one-line element (`<integer>30</integer>`), and a nested
/// `<dict>`/`<array>` block. Everything else — a multi-line `<data>` blob, an
/// XML comment between key and value, a hand-wrapped string — falls through to
/// `None` deliberately.
fn plist_value_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut index = start;
    while lines.get(index).is_some_and(|line| line.trim().is_empty()) {
        index += 1;
    }
    let first = lines.get(index)?.trim();

    if first == "<dict>" || first == "<array>" {
        let mut depth = 0_usize;
        while let Some(line) = lines.get(index) {
            let trimmed = line.trim();
            if trimmed == "<dict>" || trimmed == "<array>" {
                depth += 1;
            } else if trimmed == "</dict>" || trimmed == "</array>" {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            index += 1;
        }
        return None;
    }

    // `<true/>`, `<dict/>`: one tag, self-closing.
    if first.starts_with('<') && first.ends_with("/>") && first.matches('<').count() == 1 {
        return Some(index + 1);
    }
    // `<integer>30</integer>`: opens and closes on the same line.
    if first.starts_with('<') && first.ends_with('>') && first.matches('<').count() == 2 {
        return Some(index + 1);
    }
    None
}

/// Replace the restart-policy assignments in a systemd unit's `[Service]`
/// section, preserving every other directive, comment and blank line.
fn reconcile_systemd(body: &str) -> Reconcile {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut kept: Vec<String> = Vec::with_capacity(lines.len() + 4);
    let mut in_service = false;
    let mut anchor: Option<usize> = None;
    // Fallbacks for a unit that carries no policy keys yet: the end of the
    // `[Service]` block's last directive, else just after its header.
    let mut service_tail: Option<usize> = None;
    let mut service_head: Option<usize> = None;

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_service = trimmed == "[Service]";
            if in_service {
                service_head = Some(kept.len() + 1);
            }
        } else if in_service {
            let key = trimmed.split('=').next().unwrap_or_default().trim();
            if trimmed.contains('=') && SYSTEMD_POLICY_KEYS.contains(&key) {
                let _ = anchor.get_or_insert(kept.len());
                continue;
            }
            if !trimmed.is_empty() {
                service_tail = Some(kept.len() + 1);
            }
        }
        kept.push((*line).to_owned());
    }

    let Some(anchor) = anchor.or(service_tail).or(service_head) else {
        return Reconcile::Unrecognized("it has no [Service] section to carry the policy");
    };

    for (offset, line) in systemd_policy_lines().into_iter().enumerate() {
        kept.insert(anchor + offset, line);
    }
    settled(body, &kept)
}

/// The `PHUX_SOCKET` a unit pins, when it pins one.
///
/// Read from the unit rather than from `--socket` or the ambient environment,
/// because the question a reconcile has to answer is "is the server *this
/// unit* supervises alive", and only the unit knows. A unit with no override
/// leaves the caller on [`phux_server::runtime::default_socket_path`], which
/// is exactly what the supervised server would resolve.
fn unit_socket_override(manager: Manager, body: &str) -> Option<PathBuf> {
    match manager {
        Manager::Launchd => {
            let key = body
                .lines()
                .position(|line| line.trim() == "<key>PHUX_SOCKET</key>")?;
            let value = body.lines().nth(key + 1)?.trim();
            let inner = value.strip_prefix("<string>")?.strip_suffix("</string>")?;
            Some(PathBuf::from(xml_unescape(inner)))
        }
        Manager::Systemd => {
            let value = body.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("Environment=\"PHUX_SOCKET=")?
                    .strip_suffix('"')
            })?;
            Some(PathBuf::from(systemd_unquote(value)))
        }
    }
}

/// `phux service reconcile` — bring an installed unit's restart policy up to
/// date without stopping the server it supervises.
///
/// The honest contract, which differs per platform and says so:
///
/// - **systemd** can re-read a unit file without touching the running service.
///   `daemon-reload` does exactly that, so the corrected policy governs the
///   running server's very next exit and no pane is disturbed.
/// - **launchd** cannot. A loaded job keeps the policy it was bootstrapped
///   with, and the only way to replace it is `bootout` + `bootstrap`, which
///   SIGTERMs the job. So this writes the file, reports that the *loaded* job
///   is still on the old policy, and says both when it fixes itself (next
///   login or reboot, no action needed) and what doing it now would cost.
///
/// Printing "reconciled" on macOS and stopping there would be the failure this
/// verb exists to avoid: a command that claims a fix it did not make.
pub(crate) fn run_reconcile(print: bool) -> ExitCode {
    let Some(manager) = Manager::host() else {
        eprintln!(
            "phux service: no unit generator for this platform, so `phux service install`\n\
             never wrote a unit here. There is nothing to reconcile."
        );
        return ExitCode::FAILURE;
    };

    let unit_path = match manager.unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("phux service: {err}");
            return ExitCode::FAILURE;
        }
    };

    let body = match std::fs::read_to_string(&unit_path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            outln!("not installed (no unit at {})", unit_path.display());
            outln!("Install one with `phux service install`.");
            return ExitCode::FAILURE;
        }
        Err(err) => {
            eprintln!(
                "phux service: could not read {}: {err}",
                unit_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    match reconcile_unit(manager, &body) {
        Reconcile::Unrecognized(why) => {
            eprintln!(
                "phux service: {} was left alone — {why}.\n\
                 \n\
                 Rewriting it would risk a unit the init system silently refuses to load, which\n\
                 is worse than the policy it carries now. Compare it against `phux service\n\
                 install --print` and correct the restart policy by hand.",
                unit_path.display()
            );
            ExitCode::FAILURE
        }
        Reconcile::Current => {
            if print {
                out!("{body}");
                return ExitCode::SUCCESS;
            }
            outln!(
                "Already current: {} carries the throttled, failure-only restart policy.",
                unit_path.display()
            );
            ExitCode::SUCCESS
        }
        Reconcile::Patched(patched) => {
            if print {
                out!("{patched}");
                return ExitCode::SUCCESS;
            }
            if let Err(err) = std::fs::write(&unit_path, &patched) {
                eprintln!(
                    "phux service: could not write {}: {err}",
                    unit_path.display()
                );
                return ExitCode::FAILURE;
            }
            outln!("Rewrote the restart policy in {}.", unit_path.display());
            outln!("  policy  restart on failure only, one start per {RESTART_THROTTLE_SECS}s");
            outln!("  panes   untouched — nothing was stopped");
            outln!();
            let live = socket::probe(
                &unit_socket_override(manager, &body)
                    .unwrap_or_else(phux_server::runtime::default_socket_path),
            ) == SocketState::Live;
            report_policy_reach(manager, &unit_path, live);
            ExitCode::SUCCESS
        }
    }
}

/// Say, per platform, whether the policy just written is *in effect* — and
/// when it is not, what it would cost to make it so.
///
/// Split out because [`run_reconcile`] and the post-update reconcile print the
/// same thing, and the one paragraph a user acts on must not have two
/// wordings that can drift apart.
fn report_policy_reach(manager: Manager, unit_path: &Path, live: bool) {
    match manager {
        Manager::Systemd => match run_tool(
            "systemctl",
            &["--user".to_owned(), "daemon-reload".to_owned()],
        ) {
            Ok(()) => outln!(
                "systemd re-read the unit. The running server kept running, and the corrected\n\
                 policy governs its next exit."
            ),
            Err(err) => {
                eprintln!("phux service: note: {err}");
                outln!(
                    "The file is correct, but systemd is still holding the definition it loaded\n\
                     earlier. Run `systemctl --user daemon-reload` to pick this up; it stops\n\
                     nothing."
                );
            }
        },
        Manager::Launchd => {
            outln!(
                "The corrected policy is NOT active yet. launchd has no way to re-read a plist\n\
                 for a job that is already loaded — `bootout` is the only path, and it stops the\n\
                 job. So the loaded job keeps the old policy for now."
            );
            outln!();
            outln!(
                "It fixes itself at your next login or reboot, when launchd bootstraps the job\n\
                 from the file above. No action needed."
            );
            outln!();
            if live {
                outln!(
                    "To make it active right now, at the cost of every running pane and its\n\
                     in-flight shells and agents (`phux ls` shows what would be lost):"
                );
            } else {
                outln!(
                    "Nothing is listening on this unit's socket, so there are no panes to lose.\n\
                     To make it active right now:"
                );
            }
            outln!();
            outln!("    launchctl bootout {}", launchd_target());
            outln!(
                "    launchctl bootstrap gui/{} {}",
                uid(),
                unit_path.display()
            );
        }
    }
}

/// Reconcile an installed unit after `phux update` replaced the binary
/// (phux-bd30).
///
/// Automatic *only* because the reconcile is non-destructive by construction:
/// it rewrites a file and, on systemd, asks for a reload that stops nothing.
/// An automatic reconcile of the older, reinstall-shaped kind would have ended
/// every pane in the middle of an update with no prompt at all — which is why
/// phux-bd30's "have `phux update` do it" waited on phux-l1yx rather than
/// shipping first.
///
/// Silent unless it changed something, and never fatal: an update that
/// succeeded must not report failure because a unit could not be tidied.
pub(crate) fn reconcile_after_update() {
    let Some(manager) = Manager::host() else {
        return;
    };
    let Ok(unit_path) = manager.unit_path(profile_suffix().as_deref()) else {
        return;
    };
    let Ok(body) = std::fs::read_to_string(&unit_path) else {
        return;
    };
    let Reconcile::Patched(patched) = reconcile_unit(manager, &body) else {
        return;
    };
    if std::fs::write(&unit_path, &patched).is_err() {
        return;
    }

    outln!();
    outln!(
        "Your service unit predated the corrected restart policy; phux rewrote it in\n\
         place. Nothing was stopped."
    );
    outln!("  unit    {}", unit_path.display());
    outln!();
    let live = socket::probe(
        &unit_socket_override(manager, &body)
            .unwrap_or_else(phux_server::runtime::default_socket_path),
    ) == SocketState::Live;
    report_policy_reach(manager, &unit_path, live);
}

/// Build the plan an install will write, resolving every path and default
/// once so the renderers stay pure.
fn resolve_plan(
    quic: Option<String>,
    listen: Option<String>,
    restore: bool,
    socket: Option<PathBuf>,
    hub: bool,
) -> Result<ServicePlan, String> {
    let binary = std::env::current_exe()
        .map_err(|err| format!("could not resolve the running phux binary: {err}"))?;
    let state = phux_server::telemetry::state_dir();
    let socket_path = socket
        .clone()
        .unwrap_or_else(phux_server::runtime::default_socket_path);

    Ok(ServicePlan {
        binary,
        quic,
        listen,
        tokens: std::env::var_os("PHUX_WS_TOKENS")
            .map_or_else(phux_server::auth::default_token_store_path, PathBuf::from),
        cert: std::env::var_os("PHUX_WS_TLS_CERT").map_or_else(
            phux_server::transport::tls::default_cert_path,
            PathBuf::from,
        ),
        key: std::env::var_os("PHUX_WS_TLS_KEY")
            .map_or_else(phux_server::transport::tls::default_key_path, PathBuf::from),
        socket,
        hub,
        socket_path,
        profile: profile_suffix(),
        // The ONE canonical server log — resolved through the shared helper
        // so the unit's writer and `phux service logs`'s reader can never
        // disagree (phux-i0e8.5.1).
        log: phux_server::telemetry::server_log_path(),
        restore: restore.then(|| state.join("workspace.json")),
        wrapper: state.join("service-wrapper.sh"),
    })
}

/// Render the unit for a manager, so callers do not match on it twice.
fn render_unit(manager: Manager, plan: &ServicePlan) -> String {
    match manager {
        Manager::Launchd => render_launchd_plist(plan),
        Manager::Systemd => render_systemd_unit(plan),
    }
}

/// Write the unit (and the restore wrapper, when `--restore` asked for one) to
/// stdout without touching the filesystem.
///
/// `manager` is `None` on a platform with no generator. That case still gets
/// text, per ADR-0055, and the text is the **systemd** unit: the ADR groups
/// third platforms with non-systemd Linux, where the systemd unit is the
/// directly relevant reference, and it is the more transferable of the two
/// renderings for anyone hand-translating it. Emitting both instead would put
/// two documents on one stdout, and a plist cannot legally carry a leading
/// comment saying which is which.
fn dry_run_text(manager: Option<Manager>, plan: &ServicePlan) -> String {
    let mut text = render_unit(manager.unwrap_or(Manager::Systemd), plan);
    if plan.restore.is_some() {
        text.push('\n');
        text.push_str(&render_wrapper_script(plan));
    }
    text
}

/// `phux service install` — write the unit and hand it to the init system.
///
/// Idempotent: an existing unit is reconciled (unloaded, rewritten, reloaded)
/// rather than refused, so rerunning after changing a listener address is the
/// documented way to change it.
pub(crate) fn run_install(
    quic: Option<std::net::SocketAddr>,
    listen: Option<String>,
    restore: bool,
    socket: Option<PathBuf>,
    hub: bool,
    print: bool,
) -> ExitCode {
    // `--quic` arrives pre-validated as a `SocketAddr` (the same type
    // `server --quic` takes); the plan keeps the rendered string so the
    // unit output is byte-identical to what it always was.
    let plan = match resolve_plan(
        quic.map(|addr| addr.to_string()),
        listen,
        restore,
        socket,
        hub,
    ) {
        Ok(plan) => plan,
        Err(err) => {
            eprintln!("phux service: {err}");
            return ExitCode::FAILURE;
        }
    };

    // `--print` is a dry run: render everything to stdout, touch nothing.
    // The unit is the reviewable artifact, so being able to read it before
    // it lands is worth a flag.
    //
    // Deliberately ahead of the platform check. A dry run needs no launchd and
    // no systemd -- it renders text -- and gating it behind `Manager::host()`
    // made `service install --print` fail on exactly the platforms ADR-0055
    // promises a printed unit to (phux-l83y).
    if print {
        out!("{}", dry_run_text(Manager::host(), &plan));
        return ExitCode::SUCCESS;
    }

    let Some(manager) = Manager::host() else {
        // Not a bare error: ADR-0055 commits that a platform with no generator
        // gets the unit and an instruction. It is still a non-zero exit,
        // because nothing was installed and `phux service install && ...` must
        // not run its right-hand side.
        out!("{}", dry_run_text(None, &plan));
        eprintln!(
            "\nphux service: no unit generator for this platform. Nothing was installed.\n\
             The unit above is a starting point -- adapt it for your init system, or run\n\
             `phux server` under your own supervisor."
        );
        return ExitCode::FAILURE;
    };

    // Refuse rather than install a unit that provably cannot work.
    //
    // The supervised server binds the same socket. If a live server already
    // holds it, `handle_existing_socket` refuses with `SocketBusy` before
    // `bind(2)` is ever reached, so the supervised process exits non-zero --
    // every time, deterministically. Under the ADR-0080 policy that is not a
    // one-off failure but a permanent loop: launchd's `ThrottleInterval` is a
    // minimum spacing, not a give-up count, and the systemd unit sets no
    // `StartLimitBurst`, so neither platform ever stops retrying. The user
    // gets a failed start every 30s forever, and `phux doctor` eventually
    // reports it as a crash-loop -- accurate, but the wrong story, since
    // nothing is killing the server; it is refusing to start (phux-67wg).
    //
    // Stopping the incumbent here would be worse: it owns live panes and
    // their in-flight shells and agents. Moving a running server under
    // supervision without losing them is phux-m3ot, and needs mechanism phux
    // does not have yet. So this refuses and says what to do.
    if socket::probe(&plan.socket_path) == SocketState::Live {
        eprintln!(
            "phux service: a server is already running on {}\n\
             \n\
             Installing now would supervise a server that cannot bind that socket, and the\n\
             unit would retry a failing start every {RESTART_THROTTLE_SECS}s indefinitely.\n\
             \n\
             Stop the running server first, then re-run this command. Note that stopping it\n\
             ends its panes and their processes:\n\
             \n\
             \x20   phux ls --socket {}    # see what would be lost\n",
            plan.socket_path.display(),
            plan.socket_path.display(),
        );
        // The single most common reason to be standing here: `phux doctor`
        // said the unit is legacy, and re-running install was the only remedy
        // it could name (phux-nvi2). It is not the only remedy any more, and
        // the non-destructive one costs nothing to mention (phux-l1yx).
        if let Ok(unit_path) = manager.unit_path(profile_suffix().as_deref())
            && let Ok(body) = std::fs::read_to_string(&unit_path)
            && matches!(reconcile_unit(manager, &body), Reconcile::Patched(_))
        {
            eprintln!(
                "The unit at {} predates the corrected restart policy. If bringing\n\
                 that policy up to date is what you were after, `phux service reconcile` does\n\
                 it in place — no stop, no lost panes, and none of the flags baked into that\n\
                 unit are re-derived or dropped.\n",
                unit_path.display()
            );
        }
        return ExitCode::FAILURE;
    }

    let unit_path = match manager.unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("phux service: {err}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = write_unit_files(manager, &plan, &unit_path) {
        eprintln!("phux service: {err}");
        return ExitCode::FAILURE;
    }

    match reload(manager, &plan, &unit_path) {
        Ok(()) => {}
        Err(err) => {
            eprintln!("phux service: unit written, but the init system rejected it: {err}");
            return ExitCode::FAILURE;
        }
    }

    report_install(manager, &plan, &unit_path);
    ExitCode::SUCCESS
}

/// Write the unit file (and the wrapper, when `--restore` is on), creating
/// the directories the init system expects.
fn write_unit_files(manager: Manager, plan: &ServicePlan, unit_path: &Path) -> Result<(), String> {
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    if let Some(parent) = plan.log.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }

    if plan.restore.is_some() {
        let script = render_wrapper_script(plan);
        std::fs::write(&plan.wrapper, &script)
            .map_err(|err| format!("could not write {}: {err}", plan.wrapper.display()))?;
        set_mode(&plan.wrapper, 0o755)?;
    }

    std::fs::write(unit_path, render_unit(manager, plan))
        .map_err(|err| format!("could not write {}: {err}", unit_path.display()))
}

/// Set a file's mode. The wrapper is executed by the init system, so it needs
/// the execute bit; nothing else here is mode-sensitive.
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|err| format!("could not chmod {}: {err}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// The launchd service target for this user's GUI domain.
///
/// `gui/$UID` (not `system/`) is the per-user domain ADR-0055 commits to. The
/// consequence, reported at install time, is that the agent runs when the
/// user has a session — on a headless host that means enabling automatic
/// login.
fn launchd_target() -> String {
    format!("gui/{}/{}", uid(), launchd_label())
}

/// This process's real user id, for launchd's domain syntax.
fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Hand the written unit to the init system, replacing any loaded copy.
fn reload(manager: Manager, plan: &ServicePlan, unit_path: &Path) -> Result<(), String> {
    match manager {
        Manager::Launchd => {
            // Bootout first so a reinstall picks up the new plist; a job
            // that was not loaded makes this fail, which is not an error.
            let _ = run_tool("launchctl", &["bootout".to_owned(), launchd_target()]);
            run_tool(
                "launchctl",
                &[
                    "bootstrap".to_owned(),
                    format!("gui/{}", uid()),
                    path_string(unit_path),
                ],
            )?;
            let _ = plan;
            Ok(())
        }
        Manager::Systemd => {
            run_tool(
                "systemctl",
                &["--user".to_owned(), "daemon-reload".to_owned()],
            )?;
            run_tool(
                "systemctl",
                &[
                    "--user".to_owned(),
                    "enable".to_owned(),
                    "--now".to_owned(),
                    systemd_unit(),
                ],
            )
        }
    }
}

/// Run an init-system tool, turning a nonzero exit into a message that names
/// the command — a bare exit code from `launchctl` is not a diagnosis.
fn run_tool(program: &str, args: &[String]) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("could not run `{program}`: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        format!("exit {}", output.status)
    } else {
        detail.to_owned()
    };
    Err(format!("`{program} {}` failed: {detail}", args.join(" ")))
}

/// Report what an install did, including the caveats an operator only
/// discovers later otherwise.
fn report_install(manager: Manager, plan: &ServicePlan, unit_path: &Path) {
    outln!("phux service installed.");
    outln!("  unit    {}", unit_path.display());
    outln!("  binary  {}", plan.binary.display());
    if let Some(profile) = profile_suffix() {
        outln!("  profile {profile}");
    }
    if let Some(quic) = &plan.quic {
        outln!("  quic    {quic}");
    }
    if let Some(listen) = &plan.listen {
        outln!("  ws      {listen}");
    }
    if plan.quic.is_none() && plan.listen.is_none() {
        outln!("  listen  local socket only (pass --quic or --listen for remote attach)");
    }
    outln!("  logs    {}", plan.log.display());
    if let Some(archive) = &plan.restore {
        outln!("  restore {}", archive.display());
        outln!();
        outln!(
            "Restore brings back session names, layout, and cwd — not running\n\
             processes. Restored panes are fresh shells in the right directories."
        );
    }

    if manager == Manager::Launchd {
        outln!();
        outln!(
            "A LaunchAgent runs while this user has a session. On a headless\n\
             host, enable automatic login (System Settings > Users & Groups >\n\
             Automatic login) so the server comes back after a reboot without\n\
             someone signing in at the console."
        );
    }

    // A non-default profile is usually a development build (ADR-0080 resolves
    // one automatically), and "I installed the service and my sessions are
    // still gone" is the failure it produces if this goes unsaid: the unit is
    // real, it is loaded, and it supervises a server on a different socket
    // than the one a released `phux` attaches to.
    if let Some(profile) = profile_suffix() {
        outln!();
        outln!(
            "This unit is scoped to the `{profile}` profile — its own label, socket and\n\
             state. It does not supervise, replace, or interfere with a default-profile\n\
             server. Set PHUX_PROFILE={profile} to reach the server it starts."
        );
    }

    // Stale per-pid client logs accumulate one file per client that ever
    // ran; report but never delete without being asked.
    if let Ok(count) = count_client_logs()
        && count > 50
    {
        outln!();
        outln!(
            "{count} stale client logs in {}.",
            plan.log.parent().unwrap_or(&plan.log).display()
        );
        outln!("Clear them with `phux service prune-logs`.");
    }
}

/// `phux service uninstall` — unload the unit and remove what install wrote.
pub(crate) fn run_uninstall() -> ExitCode {
    // Unlike `install --print`, there is nothing useful to render here: this
    // build has no generator for this platform, so it never wrote a unit to
    // remove. Say that, rather than the bare platform line, so the operator
    // does not go looking for one.
    let Some(manager) = Manager::host() else {
        eprintln!(
            "phux service: no unit generator for this platform, so `phux service install`\n\
             never wrote a unit here. Remove whatever supervises `phux server` by hand."
        );
        return ExitCode::FAILURE;
    };

    // Unload before deleting: removing the file out from under a loaded job
    // leaves the init system supervising a unit nothing can address.
    let unloaded = match manager {
        Manager::Launchd => run_tool("launchctl", &["bootout".to_owned(), launchd_target()]),
        Manager::Systemd => run_tool(
            "systemctl",
            &[
                "--user".to_owned(),
                "disable".to_owned(),
                "--now".to_owned(),
                systemd_unit(),
            ],
        ),
    };
    if let Err(err) = unloaded {
        // A unit that was not loaded is the expected case when uninstalling
        // twice; say so and keep going to the file removal.
        eprintln!("phux service: note: {err}");
    }

    let unit_path = match manager.unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("phux service: {err}");
            return ExitCode::FAILURE;
        }
    };
    match std::fs::remove_file(&unit_path) {
        Ok(()) => outln!("Removed {}", unit_path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            outln!("No unit at {}", unit_path.display());
        }
        Err(err) => {
            eprintln!(
                "phux service: could not remove {}: {err}",
                unit_path.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let wrapper = phux_server::telemetry::state_dir().join("service-wrapper.sh");
    if wrapper.exists() && std::fs::remove_file(&wrapper).is_ok() {
        outln!("Removed {}", wrapper.display());
    }

    if manager == Manager::Systemd {
        let _ = run_tool(
            "systemctl",
            &["--user".to_owned(), "daemon-reload".to_owned()],
        );
    }

    outln!();
    outln!("Sessions on the running server ended with it. The workspace archive,");
    outln!("token store, and certificate were left in place.");
    ExitCode::SUCCESS
}

/// `phux service status` — is a unit installed, and is the init system
/// running it?
pub(crate) fn run_status() -> ExitCode {
    // Same reasoning as `run_uninstall`: nothing to report on, because nothing
    // this build could have installed exists here.
    let Some(manager) = Manager::host() else {
        eprintln!(
            "phux service: no unit generator for this platform, so there is no phux unit\n\
             to report on. `phux doctor` still checks the server itself."
        );
        return ExitCode::FAILURE;
    };

    let unit_path = match manager.unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("phux service: {err}");
            return ExitCode::FAILURE;
        }
    };
    if !unit_path.exists() {
        outln!("not installed (no unit at {})", unit_path.display());
        outln!("Install one with `phux service install`.");
        return ExitCode::FAILURE;
    }
    outln!("unit  {}", unit_path.display());

    // Delegate liveness to the init system rather than guessing from a pid
    // file phux does not write.
    let status = match manager {
        Manager::Launchd => std::process::Command::new("launchctl")
            .args(["print", &launchd_target()])
            .status(),
        Manager::Systemd => std::process::Command::new("systemctl")
            .args(["--user", "status", &systemd_unit()])
            .status(),
    };

    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => {
            outln!("installed, but the init system is not running it.");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("phux service: could not query the init system: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `phux service logs` — show the server's log.
///
/// launchd writes to the file the plist names, so this is `tail`. systemd
/// also captures to the journal, but the unit appends to the same file, so
/// one implementation covers both. The auto-spawn path redirects its
/// daemon's stderr to the same canonical file (phux-i0e8.5.1), so this
/// verb works even when no service unit was ever installed. Delegates to
/// the shared tail in `logs` — the same code path as `phux logs --server`,
/// so the two verbs can never show a log differently.
pub(crate) fn run_logs(follow: bool, lines: u32) -> ExitCode {
    let log = phux_server::telemetry::server_log_path();
    let missing = format!(
        "phux service: no log at {} yet.\n\
         A server writes it when it next starts; `phux logs` lists every log path.",
        log.display()
    );
    super::logs::tail_file(&log, follow, lines, &missing)
}

/// `phux service prune-logs` — delete the per-pid client logs.
///
/// Every client that ever ran leaves a `client-<pid>.log`, so a
/// long-lived host accumulates hundreds. Deleting them is explicit
/// (its own verb, never a side effect of install) because a log an
/// operator is mid-investigation on is not phux's to remove.
pub(crate) fn run_prune_logs(dry_run: bool) -> ExitCode {
    let dir = phux_server::telemetry::state_dir();
    let entries = match super::logs::client_log_paths(&dir) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("phux service: could not read {}: {err}", dir.display());
            return ExitCode::FAILURE;
        }
    };

    if entries.is_empty() {
        outln!("No client logs in {}.", dir.display());
        return ExitCode::SUCCESS;
    }

    if dry_run {
        outln!(
            "{} client logs in {} (not removed).",
            entries.len(),
            dir.display()
        );
        return ExitCode::SUCCESS;
    }

    let mut removed = 0_usize;
    let mut failed = 0_usize;
    for path in &entries {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        } else {
            failed += 1;
        }
    }
    outln!("Removed {removed} client logs from {}.", dir.display());
    if failed > 0 {
        eprintln!("phux service: {failed} could not be removed.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// How many client logs are sitting in the state dir, for the install
/// report. Errors are not worth surfacing here — the count is advisory.
/// The scan itself lives in `logs`, beside the verb that reports them.
fn count_client_logs() -> std::io::Result<usize> {
    super::logs::client_log_paths(&phux_server::telemetry::state_dir()).map(|paths| paths.len())
}

/// Escape the five XML metacharacters. A plist value is arbitrary operator
/// input (a path, a bind address), and an unescaped `&` in a path produces a
/// plist launchd silently refuses to load.
fn xml_escape(value: &str) -> String {
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

/// Undo [`xml_escape`], for reading a value back out of an installed plist.
///
/// A single pass rather than chained `replace`s: `&amp;amp;` must decode to
/// the literal `&amp;` the operator wrote, and sequential replacement would
/// decode it twice.
fn xml_unescape(value: &str) -> String {
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
            // Not an entity this ever writes; pass the `&` through verbatim
            // rather than dropping a byte of somebody's path.
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Undo [`systemd_quote`], for reading a value back out of an installed unit.
///
/// One pass, for the same reason [`xml_unescape`] takes one: chained
/// `replace`s would turn the escaped form of a literal `$$` back into `$`.
fn systemd_unquote(value: &str) -> String {
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
fn systemd_escape(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ':' | '='))
    {
        return arg.to_owned();
    }
    format!("\"{}\"", systemd_quote(arg))
}

/// Escape the characters systemd treats specially inside a double-quoted
/// `ExecStart=`/`Environment=` value: the quote itself and the backslash
/// that would escape it, plus systemd's own two expansion sigils — `%`
/// triggers unit-file specifier expansion (`%h`, `%t`, ...) and `$` triggers
/// shell-style variable expansion — both of which run over this value
/// regardless of the surrounding quoting, so a literal `%` or `$` in an
/// operator-supplied path must be doubled to survive unexpanded.
fn systemd_quote(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$")
}

/// Single-quote a value for POSIX `sh`, closing and reopening the quote
/// around any embedded single quote.
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A path as a string, lossily. Every path here is one phux itself derived
/// from `HOME` or an operator flag; a non-UTF-8 byte in one would already
/// have broken the config layer.
fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `$HOME`, or an error when it is unset (or empty).
///
/// An empty fallback here used to let every join downstream silently
/// produce a path relative to the current working directory instead of
/// failing — see [`Manager::unit_path`].
fn home_dir() -> Result<PathBuf, String> {
    home_dir_from(std::env::var_os("HOME"))
}

/// [`home_dir`] with `$HOME` injectable, so a test can drive the unset case
/// without mutating the process environment (`env::set_var` is unsafe
/// under edition 2024 and this crate forbids unsafe code).
fn home_dir_from(home: Option<std::ffi::OsString>) -> Result<PathBuf, String> {
    home.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set; cannot determine the per-user unit directory".to_owned())
}

/// `$XDG_CONFIG_HOME`, falling back to `$HOME/.config` per the XDG base
/// directory spec. Errors when neither is available, for the same reason
/// [`home_dir`] does.
fn config_home() -> Result<PathBuf, String> {
    config_home_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// [`config_home`] with both environment variables injectable; see
/// [`home_dir_from`] for why this crate cannot just mutate the environment
/// in a test instead.
fn config_home_from(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    if let Some(value) = xdg_config_home.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    Ok(home_dir_from(home)?.join(".config"))
}

#[cfg(test)]
mod tests {
    use super::{
        Manager, RESTART_THROTTLE_SECS, Reconcile, SERVICE_MANAGED_ENV, START_LIMIT_BURST,
        ServicePlan, config_home_from, dry_run_text, home_dir_from, launchd_label_for,
        launchd_policy_lines, reconcile_unit, render_launchd_plist, render_systemd_unit,
        render_unit, render_wrapper_script, resolve_plan, sh_quote, systemd_escape,
        systemd_policy_lines, systemd_quote, systemd_unit_for, systemd_unquote,
        unit_socket_override, xml_escape, xml_unescape,
    };
    use std::path::PathBuf;

    /// A launchd plist as `phux service install` wrote them before
    /// phux-zomb.4: `KeepAlive: true`, no throttle. Carries `--hub`, a QUIC
    /// listener and a socket override, because those are precisely the things
    /// a reconcile must not lose (see `reconcile_keeps_what_a_reinstall_would_drop`).
    const LEGACY_PLIST: &str = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>com.phux.server</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/phux</string>
    <string>server</string>
    <string>--hub</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PHUX_QUIC_ADDR</key>
    <string>0.0.0.0:8788</string>
    <key>PHUX_SOCKET</key>
    <string>/tmp/custom/phux.sock</string>
  </dict>
  <key>StandardOutPath</key>
  <string>/home/u/.local/state/phux/server.log</string>
</dict>
</plist>
";

    /// The systemd equivalent: `Restart=always`, no `RestartSec`, no start
    /// limit, and the same operator-supplied listeners baked into it.
    const LEGACY_UNIT: &str = "\
# Generated by `phux service install` (ADR-0055).

[Unit]
Description=phux terminal control plane server

[Service]
Type=simple
ExecStart=/usr/local/bin/phux server --hub
Restart=always
Environment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\"
Environment=\"PHUX_SOCKET=/tmp/custom/phux.sock\"
StandardOutput=append:/home/u/.local/state/phux/server.log

[Install]
WantedBy=default.target
";

    /// Unwrap a `Patched`, failing loudly on any other outcome.
    fn patched(outcome: Reconcile) -> String {
        match outcome {
            Reconcile::Patched(body) => body,
            other => panic!("expected a patch, got {other:?}"),
        }
    }

    /// A plan with every optional field populated, so a renderer test sees
    /// the full shape unless it deliberately clears a field.
    fn plan() -> ServicePlan {
        ServicePlan {
            binary: PathBuf::from("/usr/local/bin/phux"),
            quic: Some("0.0.0.0:8788".to_owned()),
            listen: Some("0.0.0.0:8787".to_owned()),
            tokens: PathBuf::from("/home/u/.local/state/phux/remote-tokens"),
            cert: PathBuf::from("/home/u/.local/state/phux/remote-cert.pem"),
            key: PathBuf::from("/home/u/.local/state/phux/remote-key.pem"),
            socket: None,
            hub: false,
            socket_path: PathBuf::from("/run/user/1000/phux/phux.sock"),
            // The default profile, so every renderer test that predates
            // phux-gyza keeps asserting the names it always did.
            profile: None,
            log: PathBuf::from("/home/u/.local/state/phux/server.log"),
            restore: None,
            wrapper: PathBuf::from("/home/u/.local/state/phux/service-wrapper.sh"),
        }
    }

    #[test]
    fn launchd_plist_carries_the_auth_environment() {
        let plist = render_launchd_plist(&plan());
        // The whole reason this is generated: a unit missing the token
        // store starts a server that rejects every paired device.
        assert!(plist.contains("<key>PHUX_WS_TOKENS</key>"));
        assert!(plist.contains("<string>/home/u/.local/state/phux/remote-tokens</string>"));
        assert!(plist.contains("<key>PHUX_QUIC_ADDR</key>"));
        assert!(plist.contains("<string>0.0.0.0:8788</string>"));
        // Always-on is the point: start at login, and come back from a
        // crash. The *restart policy* — failure-only and throttled — is
        // pinned separately in `both_units_restart_only_on_failure_and_throttle`.
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<string>com.phux.server</string>"));
    }

    /// phux-87rr: both generated units carry the marker
    /// [`crate::commands::server::run_server`] reads back to decide
    /// whether spawned panes need login-shell treatment. Unconditional —
    /// present with no listeners configured too (`omitted_listeners_emit_
    /// no_environment_key` below covers that shape).
    #[test]
    fn both_units_carry_the_service_managed_marker() {
        let plist = render_launchd_plist(&plan());
        assert!(plist.contains(&format!("<key>{SERVICE_MANAGED_ENV}</key>")));
        assert!(plist.contains("<string>1</string>"));

        let unit = render_systemd_unit(&plan());
        assert!(unit.contains(&format!("Environment=\"{SERVICE_MANAGED_ENV}=1\"")));
    }

    #[test]
    fn launchd_runs_the_binary_directly_without_restore() {
        let plist = render_launchd_plist(&plan());
        assert!(plist.contains("<string>/usr/local/bin/phux</string>"));
        assert!(plist.contains("<string>server</string>"));
        assert!(!plist.contains("/bin/sh"));
    }

    #[test]
    fn hub_flag_reaches_direct_units_and_restore_wrapper() {
        let mut plan = plan();
        plan.hub = true;
        assert!(
            render_launchd_plist(&plan)
                .contains("<string>server</string>\n    <string>--hub</string>")
        );
        assert!(render_systemd_unit(&plan).contains("ExecStart=/usr/local/bin/phux server --hub"));

        plan.restore = Some(PathBuf::from("/home/u/archive.json"));
        assert!(render_wrapper_script(&plan).contains("\"$phux\" server --hub &"));
    }

    #[test]
    fn launchd_runs_the_wrapper_when_restore_is_on() {
        let mut plan = plan();
        plan.restore = Some(PathBuf::from("/home/u/.local/state/phux/workspace.json"));
        let plist = render_launchd_plist(&plan);
        assert!(plist.contains("<string>/bin/sh</string>"));
        assert!(plist.contains("<string>/home/u/.local/state/phux/service-wrapper.sh</string>"));
        // The server is started by the wrapper, not by launchd.
        assert!(!plist.contains("<string>server</string>"));
    }

    /// phux-zomb.4: both units must restart on *failure* only, and throttle.
    ///
    /// Pinned together because the two managers express one decision and a
    /// change to either alone is a bug. The failure this guards is not
    /// hypothetical: `KeepAlive: true` with no throttle produced 1487 server
    /// generations against a single log on a developer machine, and made a
    /// deliberately stopped server come straight back.
    #[test]
    fn both_units_restart_only_on_failure_and_throttle() {
        let plist = render_launchd_plist(&plan());
        assert!(
            plist.contains("<key>SuccessfulExit</key>\n    <false/>"),
            "launchd must not restart after a clean exit — `phux kill --server` \
             has to stay dead.\n{plist}"
        );
        assert!(
            !plist.contains("<key>KeepAlive</key>\n  <true/>"),
            "the unconditional KeepAlive is the defect; it must not come back.\n{plist}"
        );
        assert!(
            plist.contains("<key>ThrottleInterval</key>"),
            "an unthrottled respawn hides a crash-loop.\n{plist}"
        );

        let unit = render_systemd_unit(&plan());
        assert!(
            unit.contains("Restart=on-failure"),
            "systemd must match launchd's failure-only policy.\n{unit}"
        );
        assert!(
            !unit.contains("Restart=always"),
            "`Restart=always` is systemd's spelling of the same defect.\n{unit}"
        );
        assert!(
            unit.contains(&format!("RestartSec={RESTART_THROTTLE_SECS}s")),
            "systemd's throttle must match launchd's.\n{unit}"
        );
        // phux-67wg: throttling alone is not a give-up. systemd's default
        // rate limit is 5 starts in 10s, which at a 30s RestartSec can never
        // trip, so a server that fails every start retries forever.
        assert!(
            unit.contains(&format!("StartLimitBurst={START_LIMIT_BURST}")),
            "without a start limit, a permanently-failing start retries forever.\n{unit}"
        );
        let window: u32 = RESTART_THROTTLE_SECS * (START_LIMIT_BURST + 1);
        assert!(
            unit.contains(&format!("StartLimitIntervalSec={window}s")),
            "the limit window must admit the throttle, or the burst can never be reached.\n{unit}"
        );
        assert!(
            window > RESTART_THROTTLE_SECS * START_LIMIT_BURST,
            "a window that does not fit {START_LIMIT_BURST} throttled starts makes the \
             limit unreachable, which is the bug it exists to fix"
        );
    }

    #[test]
    fn systemd_unit_carries_the_same_environment_and_restart_policy() {
        let unit = render_systemd_unit(&plan());
        assert!(unit.contains("ExecStart=/usr/local/bin/phux server"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(
            unit.contains("Environment=\"PHUX_WS_TOKENS=/home/u/.local/state/phux/remote-tokens\"")
        );
        assert!(unit.contains("Environment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\""));
    }

    #[test]
    fn omitted_listeners_emit_no_environment_key() {
        let mut plan = plan();
        plan.quic = None;
        plan.listen = None;
        let plist = render_launchd_plist(&plan);
        assert!(!plist.contains("PHUX_QUIC_ADDR"));
        assert!(!plist.contains("PHUX_WS_ADDR"));
        // The auth material is unconditional — it is not a listener.
        assert!(plist.contains("PHUX_WS_TOKENS"));

        let unit = render_systemd_unit(&plan);
        assert!(!unit.contains("PHUX_QUIC_ADDR"));
        assert!(unit.contains("PHUX_WS_TOKENS"));
    }

    #[test]
    fn socket_override_reaches_both_units_and_the_wrapper() {
        let mut plan = plan();
        plan.socket = Some(PathBuf::from("/tmp/custom/phux.sock"));
        plan.socket_path = PathBuf::from("/tmp/custom/phux.sock");
        plan.restore = Some(PathBuf::from("/home/u/archive.json"));
        assert!(render_launchd_plist(&plan).contains("<key>PHUX_SOCKET</key>"));
        assert!(
            render_systemd_unit(&plan)
                .contains("Environment=\"PHUX_SOCKET=/tmp/custom/phux.sock\"")
        );
        // The wrapper's own phux invocations must target the same socket,
        // or save/restore would talk to a different server than the one it
        // supervises.
        let script = render_wrapper_script(&plan);
        assert_eq!(
            script.matches("--socket '/tmp/custom/phux.sock'").count(),
            3
        );
    }

    #[test]
    fn environment_order_is_stable_across_renders() {
        // A reinstall that only reshuffles keys reads as a real change in
        // `diff`; identical inputs must produce identical bytes.
        assert_eq!(render_launchd_plist(&plan()), render_launchd_plist(&plan()));
        assert_eq!(render_systemd_unit(&plan()), render_systemd_unit(&plan()));
    }

    #[test]
    fn wrapper_is_empty_without_restore() {
        assert!(render_wrapper_script(&plan()).is_empty());
    }

    #[test]
    fn wrapper_saves_on_term_and_restores_after_the_socket_appears() {
        let mut plan = plan();
        plan.restore = Some(PathBuf::from("/home/u/archive.json"));
        let script = render_wrapper_script(&plan);
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("trap 'save;"), "must save on stop");
        assert!(script.contains("TERM INT"));
        assert!(script.contains("workspace restore"));
        // Atomic save: never truncate the last good archive.
        assert!(script.contains("$archive.tmp"));
        assert!(script.contains("mv -f \"$archive.tmp\" \"$archive\""));
        // Bounded wait, not an unbounded one that would hang the unit.
        assert!(script.contains("[ \"$waited\" -lt 100 ]"));
    }

    #[test]
    fn xml_metacharacters_in_paths_are_escaped() {
        // An unescaped `&` produces a plist launchd silently refuses.
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("<x>"), "&lt;x&gt;");
        assert_eq!(xml_escape("say \"hi\""), "say &quot;hi&quot;");
        let mut plan = plan();
        plan.binary = PathBuf::from("/opt/a&b/phux");
        assert!(render_launchd_plist(&plan).contains("<string>/opt/a&amp;b/phux</string>"));
        assert!(!render_launchd_plist(&plan).contains("a&b"));
    }

    #[test]
    fn systemd_quotes_only_when_needed() {
        assert_eq!(systemd_escape("/usr/bin/phux"), "/usr/bin/phux");
        assert_eq!(systemd_escape("server"), "server");
        assert_eq!(systemd_escape("/opt/my phux/bin"), "\"/opt/my phux/bin\"");
        assert_eq!(systemd_escape("a\"b"), "\"a\\\"b\"");
    }

    /// phux-8wm regression: an operator path containing `%` or `$` must
    /// come out doubled, or systemd's specifier (`%h`, `%t`, ...) and
    /// shell-style (`$FOO`, `${FOO}`) expansion silently rewrite the unit's
    /// `ExecStart=`/`Environment=` values into something the operator never
    /// wrote.
    #[test]
    fn systemd_escape_doubles_percent_and_dollar() {
        assert_eq!(systemd_escape("/opt/100%/bin"), "\"/opt/100%%/bin\"");
        assert_eq!(systemd_escape("/opt/$HOME/bin"), "\"/opt/$$HOME/bin\"");
        assert_eq!(systemd_quote("100%"), "100%%");
        assert_eq!(systemd_quote("$FOO"), "$$FOO");
        assert_eq!(systemd_quote("${FOO}"), "$${FOO}");
    }

    /// The same hazard through the real renderer: a token/cert/key path or
    /// socket path containing `%`/`$` must not leak unescaped into the
    /// generated `[Service]` block.
    #[test]
    fn systemd_unit_escapes_percent_and_dollar_in_paths() {
        let mut plan = plan();
        plan.tokens = PathBuf::from("/home/u/100%/$HOME/remote-tokens");
        let unit = render_systemd_unit(&plan);
        assert!(
            unit.contains("100%%") && unit.contains("$$HOME"),
            "unescaped %/$ leaked into the unit:\n{unit}"
        );
        assert!(
            !unit.contains("/100%/") && !unit.contains("/$HOME/"),
            "a bare %/$ must not survive rendering:\n{unit}"
        );
    }

    #[test]
    fn sh_quote_survives_an_embedded_single_quote() {
        assert_eq!(sh_quote("/plain/path"), "'/plain/path'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn unit_paths_are_user_scope() {
        // ADR-0055: never a system-wide unit — that implies a multi-user
        // server, which ADR-0003 does not have.
        let launchd = Manager::Launchd
            .unit_path(None)
            .expect("HOME is set in this test process");
        assert!(launchd.ends_with("Library/LaunchAgents/com.phux.server.plist"));
        assert!(!launchd.starts_with("/Library"));
        let systemd = Manager::Systemd
            .unit_path(None)
            .expect("HOME is set in this test process");
        assert!(systemd.ends_with("systemd/user/phux.service"));
        assert!(!systemd.starts_with("/etc"));
    }

    /// phux-gyza: the unit *path* is profile-scoped too, not just the label
    /// inside it.
    ///
    /// Both halves matter. A shared path means a dev install overwrites the
    /// production unit file; a shared label means it overwrites the loaded
    /// job. Scoping only one would still leave a way to clobber the other.
    #[test]
    fn a_non_default_profile_writes_its_unit_beside_the_default_one() {
        let default_launchd = Manager::Launchd
            .unit_path(None)
            .expect("HOME is set in this test process");
        let dev_launchd = Manager::Launchd
            .unit_path(Some("dev"))
            .expect("HOME is set in this test process");
        assert_ne!(default_launchd, dev_launchd);
        assert!(dev_launchd.ends_with("com.phux.server.dev.plist"));
        assert_eq!(default_launchd.parent(), dev_launchd.parent());

        let default_systemd = Manager::Systemd
            .unit_path(None)
            .expect("HOME is set in this test process");
        let dev_systemd = Manager::Systemd
            .unit_path(Some("dev"))
            .expect("HOME is set in this test process");
        assert_ne!(default_systemd, dev_systemd);
        assert!(dev_systemd.ends_with("phux-dev.service"));
        assert_eq!(default_systemd.parent(), dev_systemd.parent());
    }

    /// The label a launchd plist carries follows the plan's profile, not the
    /// ambient environment — which is what keeps the renderers pure and
    /// testable (phux-gyza).
    #[test]
    fn the_plist_label_follows_the_plans_profile() {
        let default_plist = render_launchd_plist(&plan());
        assert!(
            default_plist.contains("<string>com.phux.server</string>"),
            "got {default_plist}"
        );

        let mut dev = plan();
        dev.profile = Some("dev".to_owned());
        let dev_plist = render_launchd_plist(&dev);
        assert!(
            dev_plist.contains("<string>com.phux.server.dev</string>"),
            "got {dev_plist}"
        );
        assert!(
            !dev_plist.contains("<string>com.phux.server</string>\n"),
            "the dev label must not also emit the bare production one: {dev_plist}"
        );
    }

    /// phux-l83y regression: a dry run renders on every platform.
    ///
    /// ADR-0055 commits that a platform with no unit generator gets "a printed
    /// unit and a manual instruction, not an error". `--print` used to be
    /// gated *behind* `Manager::host()`, so on exactly those platforms the one
    /// subcommand that needs no init system at all — it renders text — failed
    /// instead. `dry_run_text` takes `Option<Manager>` so the `None` case has
    /// to be answered rather than short-circuited.
    #[test]
    fn a_dry_run_renders_even_with_no_unit_generator_for_the_platform() {
        let text = dry_run_text(None, &plan());
        assert!(
            !text.is_empty(),
            "an unsupported platform must still get a unit to adapt"
        );
        // The systemd rendering is the fallback: ADR-0055 groups third
        // platforms with non-systemd Linux, where it is the relevant text.
        assert_eq!(text, dry_run_text(Some(Manager::Systemd), &plan()));
        assert!(text.contains("[Service]"), "got {text}");
    }

    /// The dry run still renders the host's own manager when there is one, and
    /// still appends the wrapper when `--restore` asked for one.
    #[test]
    fn a_dry_run_carries_the_restore_wrapper_when_one_was_planned() {
        let mut with_restore = plan();
        with_restore.restore = Some(PathBuf::from("/home/u/.local/state/phux/workspace.json"));

        let plain = dry_run_text(Some(Manager::Launchd), &plan());
        assert!(plain.contains("<?xml"), "got {plain}");
        assert!(!plain.contains("workspace restore"), "got {plain}");

        let wrapped = dry_run_text(Some(Manager::Launchd), &with_restore);
        assert!(wrapped.contains("<?xml"), "got {wrapped}");
        assert!(wrapped.contains("workspace restore"), "got {wrapped}");
    }

    /// The default profile keeps the historical, unsuffixed names.
    ///
    /// Load-bearing for upgrades: a user who already ran `service install` has
    /// a job loaded under `com.phux.server`. If a profile-aware build renamed
    /// it, `uninstall` and `status` would address a name the init system has
    /// never heard of, and the old job would keep running with nothing able to
    /// stop it. Same reason `DEFAULT_PROFILE` is stored unsuffixed on disk.
    #[test]
    fn the_default_profile_keeps_the_historical_unit_names() {
        assert_eq!(launchd_label_for(None), "com.phux.server");
        assert_eq!(systemd_unit_for(None), "phux.service");
    }

    /// phux-gyza regression: a non-default profile gets its own label.
    ///
    /// Before this, `resolve_plan` scoped the socket, state and log paths by
    /// profile but the label was a single constant — so `phux service install`
    /// from a dev build wrote a unit pointing at `phux-dev` locations *under
    /// the production label*, silently replacing the job supervising the
    /// user's real server. The isolation ADR-0080 makes automatic everywhere
    /// else has to hold here too.
    #[test]
    fn a_non_default_profile_gets_its_own_label_and_unit_name() {
        assert_eq!(launchd_label_for(Some("dev")), "com.phux.server.dev");
        assert_eq!(systemd_unit_for(Some("dev")), "phux-dev.service");

        // Distinct from the default's, which is the whole point.
        assert_ne!(launchd_label_for(Some("dev")), launchd_label_for(None));
        assert_ne!(systemd_unit_for(Some("dev")), systemd_unit_for(None));
    }

    /// phux-8wm regression: with `HOME` unset, the naive
    /// `home_dir().join(...)` used to fold into a *relative* path, so
    /// `phux service install` would silently create
    /// `./Library/LaunchAgents/...` under whatever directory the operator
    /// happened to run it from instead of failing loudly. Both managers
    /// must refuse instead.
    ///
    /// Drives [`home_dir_from`]/[`config_home_from`] directly with the
    /// unset case rather than mutating the real process environment
    /// (`env::set_var`/`remove_var` are unsafe under edition 2024, and this
    /// crate forbids unsafe code).
    #[test]
    fn unit_path_errors_instead_of_writing_into_the_cwd_when_home_is_unset() {
        let home_err = home_dir_from(None)
            .expect_err("HOME-unset must be refused, not silently empty-then-relative");
        assert!(home_err.contains("HOME"), "got {home_err}");

        let config_err = config_home_from(None, None)
            .expect_err("HOME-and-XDG_CONFIG_HOME-unset must be refused");
        assert!(config_err.contains("HOME"), "got {config_err}");

        // XDG_CONFIG_HOME alone is enough even with HOME unset.
        let config = config_home_from(Some("/xdg-config".into()), None)
            .expect("XDG_CONFIG_HOME alone must be sufficient");
        assert_eq!(config, PathBuf::from("/xdg-config"));

        // An empty (but present) HOME is exactly as absent as an unset one.
        let empty_home_err = home_dir_from(Some(std::ffi::OsString::new()))
            .expect_err("an empty HOME must be refused the same as an unset one");
        assert!(empty_home_err.contains("HOME"), "got {empty_home_err}");
    }

    /// The anti-drift test for the whole reconcile (phux-l1yx).
    ///
    /// `reconcile_unit` decides a unit is already current by patching it and
    /// finding nothing changed — there is no separate "is it legacy" predicate
    /// to keep in sync. That is only sound while the generator and the
    /// reconciler agree byte for byte about the policy block. If they ever
    /// disagree by one space, `phux service reconcile` rewrites the file
    /// `phux service install` just wrote, on every run, forever, and reports a
    /// change each time. Both share `launchd_policy_lines` /
    /// `systemd_policy_lines` so that cannot happen; this proves it end to end
    /// through the real renderers.
    #[test]
    fn the_units_this_binary_generates_reconcile_to_themselves() {
        for manager in [Manager::Launchd, Manager::Systemd] {
            let unit = render_unit(manager, &plan());
            assert_eq!(
                reconcile_unit(manager, &unit),
                Reconcile::Current,
                "{manager:?} generates a unit its own reconciler wants to rewrite:\n{unit}"
            );
        }
    }

    /// The reason this verb exists rather than "just re-run install"
    /// (phux-l1yx obstacle 1).
    ///
    /// `--quic`, `--listen`, `--restore`, `--hub` and `--socket` survive only
    /// inside the rendered unit; nothing parses one back into a `ServicePlan`.
    /// So a `phux service install` re-run renders a unit with every flag the
    /// operator does not retype silently DROPPED — their QUIC listener and hub
    /// mode gone, discovered days later from a device that will not attach.
    ///
    /// The reconcile never re-derives them, because it never re-renders. If
    /// this test starts failing, the reconcile has grown a `ServicePlan` and
    /// has become a reinstall wearing a different name.
    #[test]
    fn reconcile_keeps_what_a_reinstall_would_drop() {
        for (manager, legacy, kept) in [
            (
                Manager::Launchd,
                LEGACY_PLIST,
                vec![
                    "<string>--hub</string>",
                    "<key>PHUX_QUIC_ADDR</key>",
                    "<string>0.0.0.0:8788</string>",
                    "<string>/tmp/custom/phux.sock</string>",
                    "<string>/home/u/.local/state/phux/server.log</string>",
                ],
            ),
            (
                Manager::Systemd,
                LEGACY_UNIT,
                vec![
                    "ExecStart=/usr/local/bin/phux server --hub",
                    "Environment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\"",
                    "Environment=\"PHUX_SOCKET=/tmp/custom/phux.sock\"",
                    "StandardOutput=append:/home/u/.local/state/phux/server.log",
                ],
            ),
        ] {
            let body = patched(reconcile_unit(manager, legacy));
            for line in kept {
                assert!(
                    body.contains(line),
                    "{manager:?} reconcile dropped `{line}`:\n{body}"
                );
            }
        }
    }

    /// A legacy plist gains the policy and loses nothing else.
    ///
    /// The `RunAtLoad` assertion is the sharp one: its value is also a bare
    /// `<true/>`, one line above `KeepAlive`'s. A reconcile that matched on
    /// the value instead of scoping to the key it follows would eat the wrong
    /// element and produce a plist launchd silently declines to load — the
    /// exact failure this command is supposed to cure.
    #[test]
    fn reconciling_a_legacy_launchd_plist_replaces_only_the_policy() {
        let body = patched(reconcile_unit(Manager::Launchd, LEGACY_PLIST));
        assert!(
            body.contains(&launchd_policy_lines().join("\n")),
            "the corrected policy is not present as a block:\n{body}"
        );
        assert!(
            !body.contains("<key>KeepAlive</key>\n  <true/>"),
            "the unconditional KeepAlive survived:\n{body}"
        );
        assert!(
            body.contains("<key>RunAtLoad</key>\n  <true/>"),
            "RunAtLoad's own <true/> was consumed:\n{body}"
        );
        // Two lines out (the key and its value), seven in.
        assert_eq!(
            body.lines().count(),
            LEGACY_PLIST.lines().count() - 2 + launchd_policy_lines().len()
        );
    }

    /// The systemd half, including the keys a legacy unit never had at all:
    /// `RestartSec`, `StartLimitIntervalSec` and `StartLimitBurst` are
    /// inserted, not merely corrected.
    #[test]
    fn reconciling_a_legacy_systemd_unit_replaces_only_the_policy() {
        let body = patched(reconcile_unit(Manager::Systemd, LEGACY_UNIT));
        assert!(
            body.contains(&systemd_policy_lines().join("\n")),
            "the corrected policy is not present as a block:\n{body}"
        );
        assert!(
            !body.contains("Restart=always"),
            "the restart-on-any-exit policy survived:\n{body}"
        );
        // The `[Unit]` and `[Install]` sections are none of the reconcile's
        // business and must come through untouched.
        assert!(body.contains("[Unit]\nDescription=phux terminal control plane server"));
        assert!(body.contains("[Install]\nWantedBy=default.target"));
        assert!(body.starts_with("# Generated by `phux service install`"));
    }

    /// Reconciling twice must be a no-op the second time.
    ///
    /// Not a nicety: `phux update` runs this unprompted (phux-bd30) and
    /// reports what it changed. A reconcile that kept "changing" an already
    /// correct unit would print a scary paragraph about restart policy after
    /// every single update, forever, and train people to ignore it.
    #[test]
    fn reconcile_is_idempotent() {
        for (manager, legacy) in [
            (Manager::Launchd, LEGACY_PLIST),
            (Manager::Systemd, LEGACY_UNIT),
        ] {
            let once = patched(reconcile_unit(manager, legacy));
            assert_eq!(
                reconcile_unit(manager, &once),
                Reconcile::Current,
                "{manager:?} reconcile is not a fixed point:\n{once}"
            );
        }
    }

    /// A shape the reconciler cannot parse must be refused, not guessed at.
    ///
    /// A half-rewritten plist is a unit launchd silently refuses to load,
    /// which leaves the user with NO supervisor rather than a badly configured
    /// one — strictly worse than the legacy policy being corrected. The same
    /// applies to a systemd file with no `[Service]` section: there is nowhere
    /// the policy could go that would mean anything.
    #[test]
    fn an_unparseable_unit_is_refused_rather_than_rewritten() {
        let opaque_value = "\
<plist version=\"1.0\">
<dict>
  <key>KeepAlive</key>
  <data>
  QUJD
  </data>
</dict>
</plist>
";
        assert!(
            matches!(
                reconcile_unit(Manager::Launchd, opaque_value),
                Reconcile::Unrecognized(_)
            ),
            "a multi-line value must not be rewritten by guesswork"
        );

        assert!(
            matches!(
                reconcile_unit(Manager::Launchd, "not a plist at all\n"),
                Reconcile::Unrecognized(_)
            ),
            "a file with no top-level dict has nowhere to put the policy"
        );

        assert!(
            matches!(
                reconcile_unit(Manager::Systemd, "[Unit]\nDescription=x\n"),
                Reconcile::Unrecognized(_)
            ),
            "a unit with no [Service] section has nowhere to put the policy"
        );
    }

    /// `KeepAlive` inside `EnvironmentVariables` is an environment variable
    /// named `KeepAlive`, not the restart policy.
    ///
    /// Rewriting it would corrupt the server's environment AND leave the real
    /// policy legacy — a silent double failure. The reconciler scopes its
    /// match to the top-level dict by tracking nesting depth; this pins that.
    #[test]
    fn a_nested_keepalive_key_is_not_the_restart_policy() {
        let nested = "\
<plist version=\"1.0\">
<dict>
  <key>KeepAlive</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>KeepAlive</key>
    <string>not-a-policy</string>
  </dict>
</dict>
</plist>
";
        let body = patched(reconcile_unit(Manager::Launchd, nested));
        assert!(
            body.contains("    <key>KeepAlive</key>\n    <string>not-a-policy</string>"),
            "the nested environment entry was rewritten:\n{body}"
        );
        assert!(
            body.contains(&launchd_policy_lines().join("\n")),
            "the real policy was not corrected:\n{body}"
        );
    }

    /// The reconcile probes the socket the UNIT names, not the one this
    /// process would resolve.
    ///
    /// That probe decides which paragraph the user reads on macOS — "nothing
    /// is running, reloading is free" versus "this costs you every pane".
    /// Reading the wrong socket makes the command confidently tell an operator
    /// with live work that there is nothing to lose.
    #[test]
    fn the_socket_probed_is_the_one_the_unit_pins() {
        assert_eq!(
            unit_socket_override(Manager::Launchd, LEGACY_PLIST),
            Some(PathBuf::from("/tmp/custom/phux.sock"))
        );
        assert_eq!(
            unit_socket_override(Manager::Systemd, LEGACY_UNIT),
            Some(PathBuf::from("/tmp/custom/phux.sock"))
        );

        // A unit with no override leaves the caller on the default path
        // rather than inventing one.
        let mut plain = plan();
        plain.socket = None;
        assert_eq!(
            unit_socket_override(Manager::Launchd, &render_launchd_plist(&plain)),
            None
        );
        assert_eq!(
            unit_socket_override(Manager::Systemd, &render_systemd_unit(&plain)),
            None
        );

        // And it survives the escaping the renderers apply on the way in —
        // otherwise a socket path containing `%`, `$` or `&` would be probed
        // as the literal escaped text, which no server is ever listening on.
        let mut awkward = plan();
        awkward.socket = Some(PathBuf::from("/tmp/100%/$HOME/a&b/phux.sock"));
        assert_eq!(
            unit_socket_override(Manager::Launchd, &render_launchd_plist(&awkward)),
            awkward.socket
        );
        assert_eq!(
            unit_socket_override(Manager::Systemd, &render_systemd_unit(&awkward)),
            awkward.socket
        );
    }

    /// The unescapers are exact inverses of the escapers.
    ///
    /// Chained `replace` calls would not be: the escaped form of a literal
    /// `$$` is `$$$$`, and decoding it in two passes yields `$`. The values
    /// under test are operator-supplied paths, so getting this wrong silently
    /// probes a path nobody has.
    #[test]
    fn the_unescapers_invert_the_escapers() {
        for value in [
            "/plain/path",
            "100%",
            "$FOO",
            "$$",
            "%%",
            "a&b",
            "&amp;",
            "<x>",
            "say \"hi\"",
            "back\\slash",
            "\\$mixed%",
        ] {
            assert_eq!(xml_unescape(&xml_escape(value)), value, "xml: {value}");
            assert_eq!(
                systemd_unquote(&systemd_quote(value)),
                value,
                "systemd: {value}"
            );
        }
    }

    /// phux-87rr acceptance criterion 4: whatever `PATH` happens to be
    /// active when `phux service install` runs must never be frozen into
    /// the generated unit — the init system supplies its own `PATH` at
    /// spawn time regardless of what built the unit, so anything captured
    /// here would only ever be a stale snapshot of some past shell (the
    /// canonical case: a `nix develop` or direnv session, which is
    /// exactly what runs this test suite).
    ///
    /// Deliberately does not mutate `PATH` to prove this: this crate
    /// `forbid(unsafe_code)`s, and `env::set_var`/`remove_var` are unsafe
    /// under edition 2024, so an injected-marker version of this test is
    /// not an option here (see `commands::overlay`'s
    /// `run_tailscale_ip`/`run_tailscale_ip_with_deadline` split for the
    /// established alternative — dependency injection — used where that
    /// matters more than a simple read does here). Instead this reads the
    /// test process's own ambient `PATH` — under `nix develop`, already a
    /// real instance of the "transient installer shell" case — and
    /// asserts `resolve_plan` never echoes it anywhere. A real regression
    /// catch, not a simulated one.
    #[test]
    fn install_never_captures_the_process_path() {
        let ambient_path = std::env::var("PATH").unwrap_or_default();
        assert!(
            !ambient_path.is_empty(),
            "test process has no PATH; this assertion would be vacuous"
        );

        let plan = resolve_plan(None, None, false, None, false).expect("resolve_plan");
        assert!(
            plan.environment().iter().all(|(key, _)| *key != "PATH"),
            "the generated unit's environment must never carry a PATH key at all — \
             the init system supplies its own"
        );
        assert!(
            !render_launchd_plist(&plan).contains(&ambient_path),
            "launchd plist captured the process's ambient PATH"
        );
        assert!(
            !render_systemd_unit(&plan).contains(&ambient_path),
            "systemd unit captured the process's ambient PATH"
        );
    }
}
