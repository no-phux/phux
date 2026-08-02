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

/// launchd's reverse-DNS job label, and the basename of the plist it loads.
const LAUNCHD_LABEL: &str = "com.phux.server";

/// systemd's unit name. `--user` scope, so it lives under
/// `$XDG_CONFIG_HOME/systemd/user/`.
const SYSTEMD_UNIT: &str = "phux.service";

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
    fn unit_path(self) -> PathBuf {
        match self {
            Self::Launchd => home_dir()
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
            Self::Systemd => config_home()
                .join("systemd")
                .join("user")
                .join(SYSTEMD_UNIT),
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
        let mut env = Vec::with_capacity(6);
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
    let _ = writeln!(out, "  <string>{LAUNCHD_LABEL}</string>");

    out.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    for arg in plan.program_arguments() {
        let _ = writeln!(out, "    <string>{}</string>", xml_escape(&arg));
    }
    out.push_str("  </array>\n");

    out.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
    out.push_str("  <key>KeepAlive</key>\n  <true/>\n");
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
/// `Restart=always` is launchd's `KeepAlive`; `WantedBy=default.target` is
/// its `RunAtLoad`. `RestartSec` matches the hub link's floor (ADR-0052's
/// 500 ms) so a crash-looping server backs off the same way everywhere.
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
    out.push_str("Restart=always\n");
    out.push_str("RestartSec=500ms\n");
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

/// `phux service install` — write the unit and hand it to the init system.
///
/// Idempotent: an existing unit is reconciled (unloaded, rewritten, reloaded)
/// rather than refused, so rerunning after changing a listener address is the
/// documented way to change it.
pub(crate) fn run_install(
    quic: Option<String>,
    listen: Option<String>,
    restore: bool,
    socket: Option<PathBuf>,
    hub: bool,
    print: bool,
) -> ExitCode {
    let plan = match resolve_plan(quic, listen, restore, socket, hub) {
        Ok(plan) => plan,
        Err(err) => {
            eprintln!("phux service: {err}");
            return ExitCode::FAILURE;
        }
    };

    let Some(manager) = Manager::host() else {
        eprintln!(
            "phux service: no unit generator for this platform; \
             run `phux server` under your own supervisor."
        );
        return ExitCode::FAILURE;
    };

    // `--print` is a dry run: render everything to stdout, touch nothing.
    // The unit is the reviewable artifact, so being able to read it before
    // it lands is worth a flag.
    if print {
        out!("{}", render_unit(manager, &plan));
        if plan.restore.is_some() {
            outln!();
            out!("{}", render_wrapper_script(&plan));
        }
        return ExitCode::SUCCESS;
    }

    if let Err(err) = write_unit_files(manager, &plan) {
        eprintln!("phux service: {err}");
        return ExitCode::FAILURE;
    }

    match reload(manager, &plan) {
        Ok(()) => {}
        Err(err) => {
            eprintln!("phux service: unit written, but the init system rejected it: {err}");
            return ExitCode::FAILURE;
        }
    }

    report_install(manager, &plan);
    ExitCode::SUCCESS
}

/// Write the unit file (and the wrapper, when `--restore` is on), creating
/// the directories the init system expects.
fn write_unit_files(manager: Manager, plan: &ServicePlan) -> Result<(), String> {
    let unit_path = manager.unit_path();
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

    std::fs::write(&unit_path, render_unit(manager, plan))
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
    format!("gui/{}/{LAUNCHD_LABEL}", uid())
}

/// This process's real user id, for launchd's domain syntax.
fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Hand the written unit to the init system, replacing any loaded copy.
fn reload(manager: Manager, plan: &ServicePlan) -> Result<(), String> {
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
                    path_string(&manager.unit_path()),
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
                    SYSTEMD_UNIT.to_owned(),
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
fn report_install(manager: Manager, plan: &ServicePlan) {
    outln!("phux service installed.");
    outln!("  unit    {}", manager.unit_path().display());
    outln!("  binary  {}", plan.binary.display());
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
    let Some(manager) = Manager::host() else {
        eprintln!("phux service: no unit generator for this platform.");
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
                SYSTEMD_UNIT.to_owned(),
            ],
        ),
    };
    if let Err(err) = unloaded {
        // A unit that was not loaded is the expected case when uninstalling
        // twice; say so and keep going to the file removal.
        eprintln!("phux service: note: {err}");
    }

    let unit_path = manager.unit_path();
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
    let Some(manager) = Manager::host() else {
        eprintln!("phux service: no unit generator for this platform.");
        return ExitCode::FAILURE;
    };

    let unit_path = manager.unit_path();
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
            .args(["--user", "status", SYSTEMD_UNIT])
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
/// value: the quote itself, and the backslash that would escape it.
fn systemd_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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

/// `$HOME`, or an empty path when it is unset — the caller's own file
/// operations then fail with a real error rather than this guessing.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from)
}

/// `$XDG_CONFIG_HOME`, falling back to `$HOME/.config` per the XDG base
/// directory spec.
fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home_dir().join(".config"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::{
        Manager, ServicePlan, render_launchd_plist, render_systemd_unit, render_wrapper_script,
        sh_quote, systemd_escape, xml_escape,
    };
    use std::path::PathBuf;

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
        // Always-on is the point.
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains("<string>com.phux.server</string>"));
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

    #[test]
    fn systemd_unit_carries_the_same_environment_and_restart_policy() {
        let unit = render_systemd_unit(&plan());
        assert!(unit.contains("ExecStart=/usr/local/bin/phux server"));
        assert!(unit.contains("Restart=always"));
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

    #[test]
    fn sh_quote_survives_an_embedded_single_quote() {
        assert_eq!(sh_quote("/plain/path"), "'/plain/path'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn unit_paths_are_user_scope() {
        // ADR-0055: never a system-wide unit — that implies a multi-user
        // server, which ADR-0003 does not have.
        let launchd = Manager::Launchd.unit_path();
        assert!(launchd.ends_with("Library/LaunchAgents/com.phux.server.plist"));
        assert!(!launchd.starts_with("/Library"));
        let systemd = Manager::Systemd.unit_path();
        assert!(systemd.ends_with("systemd/user/phux.service"));
        assert!(!systemd.starts_with("/etc"));
    }
}
