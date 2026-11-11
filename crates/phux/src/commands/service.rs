//! `phux service` — generate and manage the per-user service unit that keeps
//! a phux server running across logout and reboot (ADR-0055): a `launchd`
//! `LaunchAgent` on macOS, a systemd user unit on Linux.
//!
//! The server's environment is materialized into the unit; a hand-written unit
//! that omits `PHUX_WS_TOKENS` silently rejects every paired device. Per-user
//! by construction (ADR-0003: one server per user).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::socket::{self, SocketState};

use crate::commands::sh_quote;

mod reconcile;
mod unit;

pub(crate) use reconcile::{Reconcile, reconcile_unit};
use reconcile::{plist_container_end, settled};
pub(crate) use unit::{ServicePlan, render_launchd_plist, render_systemd_unit};
use unit::{systemd_escape, systemd_unquote, xml_escape, xml_unescape};

/// launchd's reverse-DNS job label for the default profile, and the basename
/// of the plist it loads.
const LAUNCHD_LABEL: &str = "com.phux.server";

/// systemd's unit name for the default profile. `--user` scope, so it lives
/// under `$XDG_CONFIG_HOME/systemd/user/`.
const SYSTEMD_UNIT: &str = "phux.service";

/// launchd's job label for the active profile. The default profile keeps the
/// bare `com.phux.server` so an already-loaded job is not orphaned on upgrade;
/// other profiles are suffixed so a dev-profile install cannot replace the
/// production job (ADR-0080).
fn launchd_label() -> String {
    launchd_label_for(profile_suffix().as_deref())
}

/// systemd's unit name for the active profile. See [`launchd_label`] for why
/// the default profile keeps the bare name.
fn systemd_unit() -> String {
    systemd_unit_for(profile_suffix().as_deref())
}

/// [`launchd_label`] with the profile injected, so tests need not mutate the
/// process environment.
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
pub(crate) fn profile_suffix() -> Option<String> {
    (!phux_config::instance::is_default_profile()).then(phux_config::instance::profile)
}

/// Minimum seconds between supervised restarts (launchd `ThrottleInterval` /
/// systemd `RestartSec`): slow enough that a crash-loop stays legible in the
/// log and to `phux doctor`.
const RESTART_THROTTLE_SECS: u32 = 30;

/// Consecutive failed starts systemd tolerates before giving up; matches the
/// crash-loop threshold of `phux doctor`'s `server-health` check.
const START_LIMIT_BURST: u32 = 5;

/// Marker `phux service install` writes into the unit's own environment and
/// [`crate::commands::server::run_server`] reads back to decide whether spawned
/// panes need login-shell treatment. Only a unit this `phux` wrote carries it,
/// which makes it reliable where sniffing `PATH` or the parent process is not.
pub(crate) const SERVICE_MANAGED_ENV: &str = "PHUX_SERVICE_MANAGED";

/// Which init system this host's unit targets, resolved from the compile
/// target. Both renderers compile everywhere so their tests run everywhere.
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
    pub(crate) const fn host() -> Option<Self> {
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

    /// Where the unit file for this manager belongs. Fails rather than returning a
    /// relative path when `HOME`/`XDG_CONFIG_HOME` are unset. `profile` is the
    /// ADR-0080 profile suffix (`None` for the default).
    pub(crate) fn unit_path(self, profile: Option<&str>) -> Result<PathBuf, String> {
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

    /// [`Self::unit_path`] for a caller about to write, remove, load, or
    /// unload the unit: a development build is refused the production unit
    /// (`phux_config::production::refuse_dev_on_production_state`), so a
    /// dev `phux service install` can never put a dev binary under the
    /// day-to-day server's launchd or systemd entry.
    pub(crate) fn writable_unit_path(self, profile: Option<&str>) -> Result<PathBuf, String> {
        let path = self.unit_path(profile)?;
        phux_config::production::refuse_dev_on_production_state(&path)?;
        Ok(path)
    }
}

/// Render the `--restore` wrapper script. The server keeps its sessions in
/// its organization store across a restart, and `--autosave` (ADR-0150)
/// keeps exporting an archive as they change. launchd has no `ExecStopPre`,
/// so the wrapper adds the final export on `TERM` for both platforms.
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
    // `--hub` sits right after `server`, where `ensure_hub_in_wrapper` looks.
    let hub_arg = if plan.hub { " --hub" } else { "" };

    format!(
        "#!/bin/sh\n\
         # Generated by `phux service install --restore` (ADR-0055, ADR-0150).\n\
         # Edits are overwritten on the next install.\n\
         #\n\
         # The server keeps its sessions across a restart, and `--autosave`\n\
         # exports them to the archive atomically as they change. This wrapper\n\
         # adds a final export on stop. Processes died with the host: after a\n\
         # restart, `phux restore` relaunches them in their sessions.\n\
         set -u\n\
         \n\
         phux={binary}\n\
         archive={archive}\n\
         socket={socket}\n\
         \n\
         # Save is best-effort on every path: a stop that cannot reach the\n\
         # server must still stop. `archive export --output` writes a temp\n\
         # file and renames it, so a half-written archive never replaces the\n\
         # last good one.\n\
         save() {{\n\
         \x20   [ -S \"$socket\" ] || return 0\n\
         \x20   \"$phux\" archive export{socket_arg} --output \"$archive\"\n\
         }}\n\
         \n\
         # Trap before the server exists so a stop during startup is still\n\
         # handled; `kill` on an unset server is a no-op we tolerate.\n\
         server=''\n\
         trap 'save; [ -n \"$server\" ] && kill -TERM \"$server\" 2>/dev/null; \
         wait \"$server\" 2>/dev/null; exit 0' TERM INT\n\
         \n\
         \"$phux\" server{hub_arg} --autosave \"$archive\"{socket_arg} &\n\
         server=$!\n\
         \n\
         wait \"$server\"\n"
    )
}

/// The removed export spelling a wrapper written before `phux archive`
/// calls, and its replacement.
const OLD_WRAPPER_SAVE: &str = "\"$phux\" workspace save";
const WRAPPER_EXPORT: &str = "\"$phux\" archive export";

/// Rewrite an installed restore wrapper's `workspace save` call to `archive
/// export`; `None` when it has none. The binary no longer parses the old
/// spelling, so an unpatched wrapper's save fails until this runs.
pub(crate) fn modernize_wrapper(body: &str) -> Option<String> {
    body.contains(OLD_WRAPPER_SAVE)
        .then(|| body.replace(OLD_WRAPPER_SAVE, WRAPPER_EXPORT))
}

/// Patch the installed restore wrapper, if there is one, to the current
/// export spelling; says so unless `quiet`. Never fatal: a wrapper it cannot
/// patch keeps running its server, only its save step fails.
fn reconcile_wrapper(quiet: bool) {
    let wrapper = phux_server::telemetry::state_dir().join("service-wrapper.sh");
    let Ok(body) = std::fs::read_to_string(&wrapper) else {
        return;
    };
    let Some(patched) = modernize_wrapper(&body) else {
        return;
    };
    match std::fs::write(&wrapper, patched) {
        Ok(()) if !quiet => outln!(
            "Rewrote {} to export with `phux archive export`.",
            wrapper.display()
        ),
        Ok(()) => {}
        Err(err) => warn!("service: could not rewrite {}: {err}", wrapper.display()),
    }
}

/// The `PHUX_SOCKET` a unit pins, if any. Read from the unit because the
/// question is whether the server *this unit* supervises is alive.
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

// In-place `--hub`: `phux host add --role satellite` patches `--hub` into the
// installed argv (never re-rendering, which would drop flags), or writes and
// arms a new hub unit when none exists. Nothing is stopped.

/// What ensuring `--hub` on this machine's per-user service did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalHub {
    /// The installed unit (or restore wrapper) already ran with `--hub`.
    Already,
    /// `--hub` was inserted; every other byte of the unit was left alone.
    Patched,
    /// No unit existed; a hub unit was written and armed, not loaded.
    Installed,
    /// The unit could not be made a hub. The satellite is still registered.
    Skipped(String),
}

impl LocalHub {
    /// Stable token for the satellite-enroll JSON document.
    pub(crate) const fn as_json_str(&self) -> &'static str {
        match self {
            Self::Already => "already",
            Self::Patched => "patched",
            Self::Installed => "installed",
            Self::Skipped(_) => "skipped",
        }
    }
}

/// Outcome of a pure `--hub` patch against a unit body or restore wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HubEnsure {
    Current,
    Patched(String),
    /// `--hub` lives in the restore wrapper this unit execs, not in argv.
    Wrapper(PathBuf),
    Unrecognized(&'static str),
}

/// Make this machine's per-user service a federation hub without dropping
/// baked-in listeners or stopping a live server. Failures are skipped: the
/// satellite registration already succeeded.
pub(crate) fn ensure_local_hub() -> LocalHub {
    let Some(manager) = Manager::host() else {
        return LocalHub::Skipped("no unit generator for this platform".to_owned());
    };
    let unit_path = match manager.writable_unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => return LocalHub::Skipped(err),
    };
    if !unit_path.exists() {
        return install_hub_unit(manager, &unit_path);
    }
    let Ok(body) = std::fs::read_to_string(&unit_path) else {
        return LocalHub::Skipped(format!("could not read {}", unit_path.display()));
    };
    match ensure_hub_in_unit(manager, &body) {
        HubEnsure::Current => LocalHub::Already,
        HubEnsure::Patched(patched) => write_hub_patch(&unit_path, patched, manager),
        HubEnsure::Wrapper(path) => patch_wrapper_file(&path),
        HubEnsure::Unrecognized(reason) => LocalHub::Skipped(reason.to_owned()),
    }
}

/// Write a new hub unit and arm it, never load it (that would collide with a
/// live server, ADR-0088). The adoption marker makes the next cold `phux`
/// start this unit.
fn install_hub_unit(manager: Manager, unit_path: &Path) -> LocalHub {
    let plan = match resolve_plan(None, None, false, None, true) {
        Ok(plan) => plan,
        Err(err) => return LocalHub::Skipped(err),
    };
    if let Err(err) = write_unit_files(manager, &plan, unit_path) {
        return LocalHub::Skipped(err);
    }
    if let Err(err) = arm_unit(manager) {
        return LocalHub::Skipped(err);
    }
    if let Err(err) = mark_adoption_pending(unit_path) {
        // Unit is written and armed; only the automatic hand-over is lost.
        warn!("service: {err}");
    }
    LocalHub::Installed
}

fn write_hub_patch(unit_path: &Path, patched: String, manager: Manager) -> LocalHub {
    if let Err(err) = std::fs::write(unit_path, patched) {
        return LocalHub::Skipped(format!("could not write {}: {err}", unit_path.display()));
    }
    // systemd re-reads ExecStart on daemon-reload without touching the running
    // service; launchd keeps the loaded argv until bootout (ADR-0083).
    if manager == Manager::Systemd {
        let _ = run_tool(
            "systemctl",
            &["--user".to_owned(), "daemon-reload".to_owned()],
        );
    }
    LocalHub::Patched
}

fn patch_wrapper_file(path: &Path) -> LocalHub {
    let Ok(body) = std::fs::read_to_string(path) else {
        return LocalHub::Skipped(format!("could not read restore wrapper {}", path.display()));
    };
    match ensure_hub_in_wrapper(&body) {
        HubEnsure::Current => LocalHub::Already,
        HubEnsure::Patched(patched) => {
            if let Err(err) = std::fs::write(path, patched) {
                return LocalHub::Skipped(format!("could not write {}: {err}", path.display()));
            }
            LocalHub::Patched
        }
        HubEnsure::Wrapper(_) => LocalHub::Skipped(
            "restore wrapper does not start the server via \"$phux\" server".to_owned(),
        ),
        HubEnsure::Unrecognized(reason) => LocalHub::Skipped(reason.to_owned()),
    }
}

fn ensure_hub_in_unit(manager: Manager, body: &str) -> HubEnsure {
    match manager {
        Manager::Launchd => ensure_hub_in_launchd(body),
        Manager::Systemd => ensure_hub_in_systemd(body),
    }
}

/// Insert `--hub` after the `server` argv in a launchd `ProgramArguments`
/// array, or name the restore wrapper when the unit execs `/bin/sh`.
fn ensure_hub_in_launchd(body: &str) -> HubEnsure {
    let lines: Vec<&str> = body.split('\n').collect();
    let Some((array_start, array_end)) = program_arguments_range(&lines) else {
        return HubEnsure::Unrecognized("it has no ProgramArguments array");
    };
    let args = plist_array_strings(&lines, array_start, array_end);
    if args.iter().any(|(_, value)| value == "--hub") {
        return HubEnsure::Current;
    }
    if args.first().is_some_and(|(_, value)| value == "/bin/sh") {
        return args.get(1).map_or(
            HubEnsure::Unrecognized("ProgramArguments runs /bin/sh with no script"),
            |(_, path)| HubEnsure::Wrapper(PathBuf::from(path)),
        );
    }
    let Some(&(server_at, _)) = args.iter().find(|(_, value)| value == "server") else {
        return HubEnsure::Unrecognized("ProgramArguments does not run `phux server`");
    };
    let indent = line_indent(lines[server_at]);
    let mut kept: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    kept.insert(server_at + 1, format!("{indent}<string>--hub</string>"));
    HubEnsure::Patched(kept.join("\n"))
}

/// Byte range of the `ProgramArguments` `<array>…</array>`, or `None` when
/// the key is missing or its value is not a balanced array.
fn program_arguments_range(lines: &[&str]) -> Option<(usize, usize)> {
    let key_at = lines
        .iter()
        .position(|line| line.trim() == "<key>ProgramArguments</key>")?;
    let array_start = lines[key_at + 1..]
        .iter()
        .position(|line| line.trim() == "<array>")
        .map(|offset| key_at + 1 + offset)?;
    let array_end = plist_container_end(lines, array_start)?;
    Some((array_start, array_end))
}

/// `(line_index, unescaped value)` for each `<string>` in `[start, end)`.
fn plist_array_strings(lines: &[&str], start: usize, end: usize) -> Vec<(usize, String)> {
    (start + 1..end.saturating_sub(1))
        .filter_map(|index| {
            let inner = lines[index]
                .trim()
                .strip_prefix("<string>")?
                .strip_suffix("</string>")?;
            Some((index, xml_unescape(inner)))
        })
        .collect()
}

/// Insert `--hub` after the `server` token on `ExecStart=`, or name the
/// restore wrapper when the unit execs `/bin/sh`.
fn ensure_hub_in_systemd(body: &str) -> HubEnsure {
    let lines: Vec<&str> = body.split('\n').collect();
    let Some(idx) = lines
        .iter()
        .position(|line| line.trim().starts_with("ExecStart="))
    else {
        return HubEnsure::Unrecognized("it has no ExecStart");
    };
    let value = lines[idx]
        .trim()
        .strip_prefix("ExecStart=")
        .unwrap_or(lines[idx]);
    let tokens: Vec<&str> = value.split_whitespace().collect();
    if tokens.contains(&"--hub") {
        return HubEnsure::Current;
    }
    if tokens.first().copied() == Some("/bin/sh") {
        return tokens.get(1).map_or(
            HubEnsure::Unrecognized("ExecStart runs /bin/sh with no script"),
            |path| HubEnsure::Wrapper(PathBuf::from(*path)),
        );
    }
    if !tokens.contains(&"server") {
        return HubEnsure::Unrecognized("ExecStart does not run `phux server`");
    }
    let indent = line_indent(lines[idx]);
    let mut kept: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    kept[idx] = format!(
        "{indent}ExecStart={}",
        insert_after_word(value, "server", "--hub")
    );
    HubEnsure::Patched(kept.join("\n"))
}

/// Insert `--hub` immediately after `"$phux" server` in a restore wrapper.
fn ensure_hub_in_wrapper(body: &str) -> HubEnsure {
    const NEEDLE: &str = "\"$phux\" server";
    const WITH_HUB: &str = "\"$phux\" server --hub";
    if body.contains(WITH_HUB) {
        return HubEnsure::Current;
    }
    let Some(at) = body.find(NEEDLE) else {
        return HubEnsure::Unrecognized(
            "restore wrapper does not start the server via \"$phux\" server",
        );
    };
    let mut patched = String::with_capacity(body.len() + 6);
    patched.push_str(&body[..at]);
    patched.push_str(WITH_HUB);
    patched.push_str(&body[at + NEEDLE.len()..]);
    HubEnsure::Patched(patched)
}

fn line_indent(line: &str) -> &str {
    let trimmed = line.trim_start();
    &line[..line.len() - trimmed.len()]
}

/// Insert `insert` after the first whole-word `word` in `value`, keeping the
/// original spacing around every other token.
fn insert_after_word(value: &str, word: &str, insert: &str) -> String {
    let mut out = String::with_capacity(value.len() + insert.len() + 1);
    let mut placed = false;
    for (index, part) in value.split(' ').enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(part);
        if !placed && part == word {
            out.push(' ');
            out.push_str(insert);
            placed = true;
        }
    }
    out
}

/// `phux service reconcile` — bring an installed unit's restart policy up to
/// date without stopping its server. systemd picks it up via `daemon-reload`;
/// launchd cannot re-read a loaded job, so the output says the fix lands at
/// next login and what applying it now would cost.
pub(crate) fn run_reconcile(print: bool) -> ExitCode {
    let Some(manager) = Manager::host() else {
        info!(
            "service: no unit generator for this platform, so `phux service install`\n\
             never wrote a unit here. There is nothing to reconcile."
        );
        return ExitCode::FAILURE;
    };

    let unit_path = match manager.writable_unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            error!("service: {err}");
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
            error!("service: could not read {}: {err}", unit_path.display());
            return ExitCode::FAILURE;
        }
    };

    if !print {
        reconcile_wrapper(false);
    }
    match reconcile_unit(manager, &body) {
        Reconcile::Unrecognized(why) => {
            warn!(
                "service: {} was left alone — {why}.\n\
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
                error!("service: could not write {}: {err}", unit_path.display());
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
            report_policy_reach(manager, &unit_path, live, true);
            ExitCode::SUCCESS
        }
    }
}

/// Say, per platform, whether the policy just written is in effect, and what
/// making it so would cost. Shared by `reconcile` and the post-update path.
fn report_policy_reach(manager: Manager, unit_path: &Path, live: bool, print: bool) {
    report_policy_reach_with(manager, unit_path, live, print, run_tool);
}

fn report_policy_reach_with(
    manager: Manager,
    unit_path: &Path,
    live: bool,
    print: bool,
    run_tool: impl FnOnce(&str, &[String]) -> Result<(), String>,
) {
    match manager {
        Manager::Systemd => {
            let reload = run_tool(
                "systemctl",
                &["--user".to_owned(), "daemon-reload".to_owned()],
            );
            if !print {
                return;
            }
            match reload {
                Ok(()) => outln!(
                    "systemd re-read the unit. The running server kept running, and the corrected\n\
                 policy governs its next exit."
                ),
                Err(err) => {
                    warn!("service: {err}");
                    outln!(
                        "The file is correct, but systemd is still holding the definition it loaded\n\
                         earlier. Run `systemctl --user daemon-reload` to pick this up; it stops\n\
                         nothing."
                    );
                }
            }
        }
        Manager::Launchd => {
            if !print {
                return;
            }
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

/// Reconcile an installed unit after `phux update` replaced the binary:
/// patch the restart-policy keys and the supervised binary path. Automatic
/// because it is non-destructive; silent unless it changed something and
/// `print` is set, and never fatal.
pub(crate) fn reconcile_after_update(print: bool) {
    let Some(manager) = Manager::host() else {
        return;
    };
    let Ok(unit_path) = manager.writable_unit_path(profile_suffix().as_deref()) else {
        return;
    };
    let Ok(original) = std::fs::read_to_string(&unit_path) else {
        return;
    };
    reconcile_wrapper(!print);
    let mut body = original.clone();
    let mut policy_changed = false;
    let mut binary_changed = false;

    if let Reconcile::Patched(patched) = reconcile_unit(manager, &body) {
        body = patched;
        policy_changed = true;
    }
    if let Ok(exe) = phux_config::server_bundle::server_exe()
        && let Reconcile::Patched(patched) = rewrite_unit_binary(manager, &body, &exe)
    {
        body = patched;
        binary_changed = true;
    }
    if !policy_changed && !binary_changed {
        return;
    }
    if std::fs::write(&unit_path, &body).is_err() {
        return;
    }

    if print {
        outln!();
        if policy_changed {
            outln!(
                "Your service unit predated the corrected restart policy; phux rewrote it in\n\
                 place. Nothing was stopped."
            );
        }
        if binary_changed {
            outln!(
                "Your service unit still named a different phux binary; phux pointed it at\n\
                 this install. Nothing was stopped."
            );
        }
        outln!("  unit    {}", unit_path.display());
        outln!();
    }
    let live = print
        && socket::probe(
            &unit_socket_override(manager, &original)
                .unwrap_or_else(phux_server::runtime::default_socket_path),
        ) == SocketState::Live;
    report_policy_reach(manager, &unit_path, live, print);
}

/// Rewrite the supervised binary path in an installed unit, leaving every
/// other byte (including flags a reinstall would drop) alone.
pub(crate) fn rewrite_unit_binary(manager: Manager, body: &str, binary: &Path) -> Reconcile {
    match manager {
        Manager::Launchd => rewrite_launchd_binary(body, binary),
        Manager::Systemd => rewrite_systemd_binary(body, binary),
    }
}

fn rewrite_launchd_binary(body: &str, binary: &Path) -> Reconcile {
    let wanted = xml_escape(&path_string(binary));
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut in_args = false;
    let mut saw_first = false;
    let mut changed = false;
    for line in &lines {
        let trimmed = line.trim();
        if trimmed == "<key>ProgramArguments</key>" {
            in_args = true;
            out.push((*line).to_owned());
            continue;
        }
        if in_args && trimmed == "</array>" {
            in_args = false;
            out.push((*line).to_owned());
            continue;
        }
        if in_args
            && !saw_first
            && let Some(old) = trimmed
                .strip_prefix("<string>")
                .and_then(|rest| rest.strip_suffix("</string>"))
        {
            saw_first = true;
            if old == wanted {
                out.push((*line).to_owned());
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            out.push(format!("{}<string>{wanted}</string>", " ".repeat(indent)));
            changed = true;
            continue;
        }
        out.push((*line).to_owned());
    }
    if !saw_first {
        return Reconcile::Unrecognized("its ProgramArguments has no first <string> to rewrite");
    }
    if changed {
        settled(body, &out)
    } else {
        Reconcile::Current
    }
}

fn rewrite_systemd_binary(body: &str, binary: &Path) -> Reconcile {
    let wanted = systemd_escape(&path_string(binary));
    let mut out = Vec::new();
    let mut saw_exec = false;
    let mut changed = false;
    for line in body.split('\n') {
        let Some(rest) = line.strip_prefix("ExecStart=") else {
            out.push(line.to_owned());
            continue;
        };
        saw_exec = true;
        let Some((first, tail)) = split_first_exec_arg(rest) else {
            return Reconcile::Unrecognized("its ExecStart= line has no binary path to rewrite");
        };
        if first == wanted {
            out.push(line.to_owned());
            continue;
        }
        out.push(format!("ExecStart={wanted}{tail}"));
        changed = true;
    }
    if !saw_exec {
        return Reconcile::Unrecognized("no ExecStart= line to rewrite");
    }
    if changed {
        settled(body, &out)
    } else {
        Reconcile::Current
    }
}

/// Split `ExecStart=`'s remainder into the first argument and the rest of
/// the line (including the leading space before remaining args).
fn split_first_exec_arg(rest: &str) -> Option<(String, &str)> {
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    if rest.starts_with('"') {
        let mut escaped = false;
        for (index, ch) in rest.char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                let end = index + ch.len_utf8();
                return Some((rest[..end].to_owned(), &rest[end..]));
            }
        }
        return None;
    }
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some((rest[..end].to_owned(), &rest[end..]))
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
    let binary = phux_config::server_bundle::server_exe()
        .map_err(|err| format!("could not resolve the running phux binary: {err}"))?;
    let state = phux_server::telemetry::state_dir();
    let socket_path = socket
        .clone()
        .unwrap_or_else(phux_server::runtime::default_socket_path);
    let (cert, key) = phux_server::transport::tls::operator_tls_paths(|var| std::env::var_os(var));

    Ok(ServicePlan {
        binary,
        quic,
        listen,
        tokens: std::env::var_os("PHUX_WS_TOKENS")
            .map_or_else(phux_server::auth::default_token_store_path, PathBuf::from),
        cert,
        key,
        socket,
        hub,
        socket_path,
        profile: profile_suffix(),
        // The one canonical server log, shared with `phux service logs`.
        log: phux_server::telemetry::server_log_path(),
        restore: restore.then(|| state.join("workspace.json")),
        wrapper: state.join("service-wrapper.sh"),
    })
}

/// A dev unit must not bake in production credentials, which an inherited
/// `PHUX_WS_TOKENS` / `PHUX_WS_TLS_*` (a production pane exports them) would
/// otherwise put in its environment.
fn refuse_dev_on_production_credentials(plan: &ServicePlan) -> Result<(), String> {
    // An unset pair resolves to this build's own state dir, never production's.
    for path in [Some(&plan.tokens), plan.cert.as_ref(), plan.key.as_ref()]
        .into_iter()
        .flatten()
    {
        phux_config::production::refuse_dev_on_production_state(path)?;
    }
    Ok(())
}

/// Render the unit for a manager, so callers do not match on it twice.
fn render_unit(manager: Manager, plan: &ServicePlan) -> String {
    match manager {
        Manager::Launchd => render_launchd_plist(plan),
        Manager::Systemd => render_systemd_unit(plan),
    }
}

/// Render the unit (and restore wrapper) for `--print` without touching the
/// filesystem. With no generator for this platform, the systemd unit is the
/// reference rendering (ADR-0055).
fn dry_run_text(manager: Option<Manager>, plan: &ServicePlan) -> String {
    let mut text = render_unit(manager.unwrap_or(Manager::Systemd), plan);
    if plan.restore.is_some() {
        text.push('\n');
        text.push_str(&render_wrapper_script(plan));
    }
    text
}

/// What an install does about a server that already holds the socket
/// (ADR-0088).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Takeover {
    /// Refuse: a supervised server could not bind the incumbent's socket and
    /// would retry a failing start forever. The default.
    Refuse,
    /// Write and arm the unit without loading it (`--adopt`); supervision begins
    /// the next time a server starts.
    Adopt,
}

/// `phux service install` — write the unit and hand it to the init system.
/// Rerunning reloads an existing unit.
pub(crate) fn run_install(
    quic: Option<std::net::SocketAddr>,
    listen: Option<String>,
    restore: bool,
    socket: Option<PathBuf>,
    hub: bool,
    takeover: Takeover,
    print: bool,
) -> ExitCode {
    let plan = match resolve_plan(
        quic.map(|addr| addr.to_string()),
        listen,
        restore,
        socket,
        hub,
    ) {
        Ok(plan) => plan,
        Err(err) => {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
    };

    // `--print` renders to stdout and touches nothing, on every platform.
    if print {
        out!("{}", dry_run_text(Manager::host(), &plan));
        return ExitCode::SUCCESS;
    }

    let Some(manager) = Manager::host() else {
        // No generator: print the unit as a starting point (ADR-0055), but exit
        // non-zero because nothing was installed.
        out!("{}", dry_run_text(None, &plan));
        info!(
            "service: no unit generator for this platform. Nothing was installed.\n\
             The unit above is a starting point -- adapt it for your init system, or run\n\
             `phux server` under your own supervisor."
        );
        return ExitCode::FAILURE;
    };

    // Refuse rather than install a unit that cannot work: the supervised server
    // would fail to bind the live incumbent's socket and retry forever. Stopping
    // the incumbent would kill its panes; `--adopt` arms instead (ADR-0088).
    let incumbent_live = socket::probe(&plan.socket_path) == SocketState::Live;
    if incumbent_live && takeover == Takeover::Refuse {
        error!(
            "service: a server is already running on {}\n\
             \n\
             Installing now would supervise a server that cannot bind that socket, and the\n\
             unit would retry a failing start every {RESTART_THROTTLE_SECS}s indefinitely.\n\
             \n\
             To install without stopping it, re-run with --adopt: the unit is written and\n\
             armed instead of loaded, the running server keeps its panes, and supervision\n\
             takes over the next time a server starts.\n\
             \n\
             \x20   phux service install --adopt\n\
             \n\
             Stopping the running server first and re-running plainly also works, but it\n\
             ends its panes and their processes:\n\
             \n\
             \x20   phux ls --socket {}    # see what would be lost\n",
            plan.socket_path.display(),
            plan.socket_path.display(),
        );
        // If the unit is merely legacy, point at the non-destructive `reconcile`.
        if let Ok(unit_path) = manager.unit_path(profile_suffix().as_deref())
            && let Ok(body) = std::fs::read_to_string(&unit_path)
            && matches!(reconcile_unit(manager, &body), Reconcile::Patched(_))
        {
            hint!(
                "The unit at {} predates the corrected restart policy. If bringing\n\
                 that policy up to date is what you were after, `phux service reconcile` does\n\
                 it in place — no stop, no lost panes, and none of the flags baked into that\n\
                 unit are re-derived or dropped.\n",
                unit_path.display()
            );
        }
        return ExitCode::FAILURE;
    }

    let unit_path = match manager.writable_unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
    };

    // The policy first: a unit that could never start is never written.
    if let Err(err) =
        settle_policy(&plan).and_then(|()| write_unit_files(manager, &plan, &unit_path))
    {
        error!("service: {err}");
        return ExitCode::FAILURE;
    }

    // Adoption: same unit on disk, but armed rather than loaded (ADR-0088).
    if incumbent_live {
        // A failed arming only loses the login trigger; the phux-side hand-over
        // still works, so report it and keep the success.
        if let Err(err) = arm_unit(manager) {
            warn!(
                "service: the unit is written, but the init system would not record\n\
                 it as wanted at login ({err}). Supervision still takes over the next time a\n\
                 server starts; it will not come up by itself after a reboot until this is\n\
                 resolved."
            );
        }
        if let Err(err) = mark_adoption_pending(&unit_path) {
            warn!("service: {err}");
        }
        report_adopt(manager, &plan, &unit_path);
        return ExitCode::SUCCESS;
    }

    match reload(manager, &unit_path) {
        Ok(()) => {}
        Err(err) => {
            error!("service: unit written, but the init system rejected it: {err}");
            return ExitCode::FAILURE;
        }
    }

    // A loaded unit supersedes any armed one.
    clear_adoption_pending();

    report_install(manager, &plan, &unit_path);
    ExitCode::SUCCESS
}

/// The listener-address variables a supervised server may inherit from the
/// environment the install runs in (a systemd user manager can import it).
const LISTENER_ENV: [&str; 3] = ["PHUX_WS_ADDR", "PHUX_QUIC_ADDR", "PHUX_WT_ADDR"];

/// Whether the installing environment names a listener address the unit's
/// server could inherit.
fn inherited_listener_env() -> bool {
    LISTENER_ENV
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// [`ensure_paired_policy`] against the active `config.toml` and the
/// installing environment.
fn settle_policy(plan: &ServicePlan) -> Result<(), String> {
    ensure_paired_policy(
        plan,
        &phux_config::loader::config_path(),
        inherited_listener_env(),
    )
}

/// The remote entry point the supervised server will open, if any: the
/// unit's own listener, a listener address it inherits, or a configured
/// `[[connector]]` (the server refuses each with no mode, ADR-0179).
fn remote_entry_point(
    plan: &ServicePlan,
    config: &phux_config::Config,
    inherited_env: bool,
) -> Option<&'static str> {
    if plan.quic.is_some() || plan.listen.is_some() {
        return Some("--quic/--listen");
    }
    if inherited_env {
        return Some("PHUX_WS_ADDR/PHUX_QUIC_ADDR/PHUX_WT_ADDR");
    }
    (!config.connector.is_empty()).then_some("[[connector]]")
}

/// Write `[policy] mode = "paired"` into `config`, creating the file and
/// the `[policy]` table when they are missing. Existing keys and comments
/// stay (`phux_config::settings::write_edit`).
///
/// `phux service install` calls this when a unit opens a remote entry
/// point, which is how `phux host add` settles the mode. `phux pair
/// --enable` uses the same write.
pub(crate) fn write_paired_mode(config: &Path) -> Result<(), String> {
    phux_config::settings::write_edit(
        config,
        "policy.mode",
        phux_config::settings::Edit::Set(toml::Value::String("paired".to_owned())),
    )
    .map_err(|err| format!("could not set [policy] mode in {}: {err}", config.display()))?;
    Ok(())
}

/// A unit whose server opens a remote entry point starts only under
/// `[policy] mode = "paired"` (ADR-0179). With no mode in `config` that is
/// what the operator asked for by naming one, so write it, and say so;
/// `local` contradicts it and is refused, leaving the config alone. Nothing
/// changes for a unit with no remote entry point.
fn ensure_paired_policy(
    plan: &ServicePlan,
    config: &Path,
    inherited_env: bool,
) -> Result<(), String> {
    let loaded = phux_config::loader::load_from(config)
        .map_err(|err| format!("{}: {err}; run `phux config check`", config.display()));
    let direct = plan.quic.is_some() || plan.listen.is_some() || inherited_env;
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(err) if direct => return Err(err),
        // No listener of its own and an unreadable config: no connector is
        // known to settle, and the server reports the config itself.
        Err(_) => return Ok(()),
    };
    let Some(entry) = remote_entry_point(plan, &loaded, inherited_env) else {
        return Ok(());
    };
    match loaded.policy.mode {
        Some(phux_config::PolicyMode::Paired) => Ok(()),
        Some(phux_config::PolicyMode::Local) => Err(format!(
            "{} sets [policy] mode = \"local\", which admits no remote entry point, but the \
             service would open one ({entry}); set it to \"paired\" (enrolled client \
             certificates only) or install without it",
            config.display()
        )),
        None => {
            write_paired_mode(config)?;
            info!(
                "service: set [policy] mode = \"paired\" in {} for {entry}: the remote entry \
                 point serves only clients holding an enrolled certificate (`phux host add`, \
                 `phux pair --enroll`)",
                config.display()
            );
            Ok(())
        }
    }
}

/// Put the unit in front of the init system without starting it. launchd
/// bootstraps every plist in `~/Library/LaunchAgents` at login, so writing
/// the file is the arming; systemd needs `enable` without `--now`.
fn arm_unit(manager: Manager) -> Result<(), String> {
    match manager {
        Manager::Launchd => Ok(()),
        Manager::Systemd => {
            run_tool(
                "systemctl",
                &["--user".to_owned(), "daemon-reload".to_owned()],
            )?;
            run_tool(
                "systemctl",
                &["--user".to_owned(), "enable".to_owned(), systemd_unit()],
            )
        }
    }
}

/// Ask the init system to start the armed unit now (once the socket is free).
fn start_armed_unit(manager: Manager, unit_path: &Path) -> Result<(), String> {
    match manager {
        Manager::Launchd => run_tool(
            "launchctl",
            &[
                "bootstrap".to_owned(),
                format!("gui/{}", uid()),
                path_string(unit_path),
            ],
        ),
        Manager::Systemd => run_tool(
            "systemctl",
            &["--user".to_owned(), "start".to_owned(), systemd_unit()],
        ),
    }
}

/// Where an armed-but-unloaded unit is recorded (profile-scoped). A file,
/// because an armed unit and a deliberately stopped one look identical on
/// disk and to the init system.
fn adoption_marker_path() -> PathBuf {
    phux_server::telemetry::state_dir().join("service-adopt-pending")
}

/// Record that `unit_path` is armed and waiting for the incumbent to exit.
fn mark_adoption_pending(unit_path: &Path) -> Result<(), String> {
    let marker = adoption_marker_path();
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    std::fs::write(&marker, format!("{}\n", unit_path.display())).map_err(|err| {
        format!(
            "unit armed, but {} could not be written ({err}), so the hand-over will not happen \
             on its own",
            marker.display()
        )
    })
}

/// Forget a pending adoption (best-effort).
fn clear_adoption_pending() {
    let _ = std::fs::remove_file(adoption_marker_path());
}

/// The user-facing explanation of an armed supervision unit, shared by
/// `phux service status` and `phux doctor`. Prose without newlines, so each
/// caller wraps it.
pub(crate) const ARMED_SUPERVISION_EXPLANATION: &str = "the running server keeps its panes and stays unsupervised, so a crash before the \
     hand-over is not caught by anything; supervision begins at the next login, or at the \
     first `phux` command after that server exits, and `phux service uninstall` cancels it";

/// Whose supervision a [`supervision_state`] question is about. A unit armed
/// for another profile or `--socket` must not affect this server.
#[derive(Clone, Copy)]
enum Subject<'a> {
    /// The instance that would bind `socket_path`.
    Server(&'a Path),
    /// This profile's unit, whatever socket it was installed against.
    Unit,
}

/// What an adoption marker says about supervision (not whether the unit runs).
enum SupervisionState {
    /// No pending adoption for this subject.
    NotArmed,
    /// The marker's unit is gone; owners of the state sweep it.
    MarkerWithoutUnit,
    /// Armed: written and deliberately unloaded, waiting for the incumbent to exit.
    Armed { manager: Manager, unit: PathBuf },
}

/// The single predicate behind every "is supervision armed?" question: a
/// marker exists, its unit is readable, and (for a server subject) the unit's
/// socket is that server's socket. Never sweeps; callers that own the state
/// act on [`SupervisionState::MarkerWithoutUnit`].
fn supervision_state(subject: Subject<'_>) -> SupervisionState {
    if !adoption_marker_path().exists() {
        return SupervisionState::NotArmed;
    }
    let Some(manager) = Manager::host() else {
        return SupervisionState::NotArmed;
    };
    let Ok(unit) = manager.unit_path(profile_suffix().as_deref()) else {
        return SupervisionState::NotArmed;
    };
    let Ok(body) = std::fs::read_to_string(&unit) else {
        return SupervisionState::MarkerWithoutUnit;
    };
    match subject {
        Subject::Unit => SupervisionState::Armed { manager, unit },
        Subject::Server(socket_path) if unit_supervises(manager, &body, socket_path) => {
            SupervisionState::Armed { manager, unit }
        }
        Subject::Server(_) => SupervisionState::NotArmed,
    }
}

/// The unit an armed adoption is recorded against for the server on
/// `socket_path`, for `phux doctor`. Read-only.
pub(crate) fn armed_adoption_unit(socket_path: &Path) -> Option<PathBuf> {
    match supervision_state(Subject::Server(socket_path)) {
        SupervisionState::Armed { unit, .. } => Some(unit),
        SupervisionState::NotArmed | SupervisionState::MarkerWithoutUnit => None,
    }
}

/// Does `body` describe a unit whose server would bind `socket_path`?
fn unit_supervises(manager: Manager, body: &str, socket_path: &Path) -> bool {
    unit_socket_override(manager, body).unwrap_or_else(phux_server::runtime::default_socket_path)
        == socket_path
}

/// Whether a pending-adoption record is spent: its unit vanished, or the init
/// system is already running it.
const fn adoption_marker_is_spent(state: &SupervisionState, unit_running: bool) -> bool {
    match state {
        SupervisionState::MarkerWithoutUnit => true,
        SupervisionState::Armed { .. } => unit_running,
        SupervisionState::NotArmed => false,
    }
}

/// Ask the init system whether it is running this profile's unit.
fn unit_is_running(manager: Manager) -> bool {
    probe_unit(manager).is_ok_and(|output| output.status.success())
}

/// The captured init-system probe `phux service status` already ran.
fn probe_unit(manager: Manager) -> std::io::Result<std::process::Output> {
    match manager {
        Manager::Launchd => std::process::Command::new("launchctl")
            .args(["print", &launchd_target()])
            .output(),
        Manager::Systemd => std::process::Command::new("systemctl")
            .args(["--user", "status", &systemd_unit()])
            .output(),
    }
}

/// Sweep an adoption marker that can no longer be pending for `socket_path`
/// (login bootstrap starts the unit without going through
/// [`complete_pending_adoption`]).
pub(crate) fn sweep_stale_adoption_marker(socket_path: &Path) {
    let state = supervision_state(Subject::Server(socket_path));
    let running = match state {
        SupervisionState::Armed { manager, .. } => unit_is_running(manager),
        SupervisionState::MarkerWithoutUnit | SupervisionState::NotArmed => false,
    };
    if adoption_marker_is_spent(&state, running) {
        clear_adoption_pending();
    }
}

/// Outcome of trying to complete an armed adoption from the auto-spawn path.
pub(crate) enum Handover {
    /// The init system accepted the start; the caller must wait for the socket
    /// rather than spawn a competing server.
    Started,
    /// Nothing was started; the caller auto-spawns as usual.
    NotTaken,
}

/// Complete an armed adoption if one is pending for `socket_path`, called
/// from the auto-spawn path once the incumbent has exited. Only a pending
/// adoption diverts, and only once, so a deliberately stopped server stays
/// stopped (ADR-0080). `quiet` honors the `--json` stderr contract.
pub(crate) fn complete_pending_adoption(socket_path: &Path, quiet: bool) -> Handover {
    let (manager, unit_path) = match supervision_state(Subject::Server(socket_path)) {
        SupervisionState::Armed { manager, unit } => (manager, unit),
        SupervisionState::MarkerWithoutUnit => {
            clear_adoption_pending();
            return Handover::NotTaken;
        }
        SupervisionState::NotArmed => return Handover::NotTaken,
    };
    match start_armed_unit(manager, &unit_path) {
        Ok(()) => {
            // Cleared once the start request succeeds; a supervised server that then
            // fails is a crash-loop for `phux doctor`, not something to retrigger.
            clear_adoption_pending();
            if !quiet {
                info!(
                    "handing this server over to {} — the unit armed by `phux service \
                     install --adopt` is now live",
                    match manager {
                        Manager::Launchd => "launchd",
                        Manager::Systemd => "systemd",
                    }
                );
            }
            Handover::Started
        }
        Err(err) => {
            // Fall through to an ordinary spawn; the marker stays for the next try.
            if !quiet {
                warn!("could not start the armed service unit ({err}); starting a server");
            }
            Handover::NotTaken
        }
    }
}

/// Report an `--adopt` install: what was written, what was not done, and when
/// supervision begins. Must not read as "the running server is supervised".
fn report_adopt(manager: Manager, plan: &ServicePlan, unit_path: &Path) {
    outln!("phux service armed (nothing was stopped).");
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
    outln!("  logs    {}", plan.log.display());
    outln!("  panes   untouched — the running server was not signalled");
    outln!();
    outln!(
        "The server on {} keeps running exactly as it was: same process, same panes,\n\
         same shells and agents. It is NOT supervised — neither launchd nor systemd can\n\
         restart-manage a process it did not start, so no command could have made it so\n\
         without replacing it.",
        plan.socket_path.display()
    );
    outln!();
    outln!("Supervision takes over at whichever of these comes first:");
    outln!();
    let at_login = match manager {
        Manager::Launchd => "when launchd bootstraps the unit above",
        Manager::Systemd => "when systemd starts the unit it now wants",
    };
    outln!("  * your next login or reboot, {at_login};");
    outln!(
        "  * the next `phux` command after that server exits, which starts the supervised\n\
         \x20   one instead of forking an unsupervised replacement."
    );
    outln!();
    outln!(
        "To hand over now, at the cost of the running panes and their in-flight shells\n\
         and agents (`phux ls` shows what would be lost):"
    );
    outln!();
    outln!("    phux kill --server");
    match manager {
        Manager::Launchd => outln!(
            "    launchctl bootstrap gui/{} {}",
            uid(),
            unit_path.display()
        ),
        Manager::Systemd => outln!("    systemctl --user start {}", systemd_unit()),
    }

    if let Some(profile) = profile_suffix() {
        outln!();
        outln!(
            "This unit is scoped to the `{profile}` profile — its own label, socket and\n\
             state. It does not supervise, replace, or interfere with a default-profile\n\
             server. Set PHUX_PROFILE={profile} to reach the server it starts."
        );
    }
}

/// Write the unit file (and the wrapper, when `--restore` is on), creating
/// the directories the init system expects.
fn write_unit_files(manager: Manager, plan: &ServicePlan, unit_path: &Path) -> Result<(), String> {
    refuse_dev_on_production_credentials(plan)?;
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

/// The launchd service target in this user's GUI domain (`gui/$UID`, per
/// ADR-0055), so the agent runs only while the user has a session.
fn launchd_target() -> String {
    format!("gui/{}/{}", uid(), launchd_label())
}

/// This process's real user id, for launchd's domain syntax.
fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Hand the written unit to the init system, replacing any loaded copy.
fn reload(manager: Manager, unit_path: &Path) -> Result<(), String> {
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
            )
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
    let output =
        super::server::ensure::service_output(std::process::Command::new(program).args(args))
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
            "The server keeps its sessions across a restart and exports them to\n\
             this archive as they change. Running processes die with the host:\n\
             after a restart, `phux restore` relaunches them in their sessions,\n\
             in the right directories."
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

    // A non-default profile supervises a different socket than a released `phux`
    // attaches to; say so, or "my sessions are gone" follows.
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
    // No generator here means no unit was ever written to remove.
    let Some(manager) = Manager::host() else {
        info!(
            "service: no unit generator for this platform, so `phux service install`\n\
             never wrote a unit here. Remove whatever supervises `phux server` by hand."
        );
        return ExitCode::FAILURE;
    };

    // Resolved (and refused to a dev build aimed at production) before the
    // unload, which is itself a production mutation.
    let unit_path = match manager.writable_unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
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
        warn!("service: {err}");
    }

    match std::fs::remove_file(&unit_path) {
        Ok(()) => outln!("Removed {}", unit_path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            outln!("No unit at {}", unit_path.display());
        }
        Err(err) => {
            error!("service: could not remove {}: {err}", unit_path.display());
            return ExitCode::FAILURE;
        }
    }

    let wrapper = phux_server::telemetry::state_dir().join("service-wrapper.sh");
    if wrapper.exists() && std::fs::remove_file(&wrapper).is_ok() {
        outln!("Removed {}", wrapper.display());
    }

    // Revoke any armed adoption so the next cold start does not load the unit
    // just deleted. Every marker goes, whatever it named.
    if adoption_marker_path().exists() {
        clear_adoption_pending();
        outln!("Cancelled the pending adoption; nothing will take this socket over.");
    }

    if manager == Manager::Systemd {
        let _ = run_tool(
            "systemctl",
            &["--user".to_owned(), "daemon-reload".to_owned()],
        );
    }

    outln!();
    outln!("Sessions on the running server ended with it. The session archive,");
    outln!("token store, and certificate were left in place.");
    ExitCode::SUCCESS
}

/// `phux service status` — is a unit installed, and is the init system
/// running it?
pub(crate) fn run_status() -> ExitCode {
    let Some(manager) = Manager::host() else {
        info!(
            "service: no unit generator for this platform, so there is no phux unit\n\
             to report on. `phux doctor` still checks the server itself."
        );
        return ExitCode::FAILURE;
    };

    let unit_path = match manager.unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
    };
    if !unit_path.exists() {
        outln!("not installed (no unit at {})", unit_path.display());
        outln!("Install one with `phux service install`.");
        return ExitCode::FAILURE;
    }
    outln!("unit  {}", unit_path.display());

    // Delegate liveness to the init system with its output captured; its stderr
    // ("Bad request. / Could not find service") never reaches the terminal.
    let probe = || probe_unit(manager);

    // `Subject::Unit`: this verb is about the unit, whatever its socket.
    let armed = matches!(
        supervision_state(Subject::Unit),
        SupervisionState::Armed { .. }
    );
    match status_report(armed, probe) {
        Ok(report) => {
            if armed && report.running {
                // A running job under an armed record means the hand-over completed.
                clear_adoption_pending();
            }
            out!("{}", report.text);
            if armed || report.running {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            error!("service: could not query the init system: {err}");
            ExitCode::FAILURE
        }
    }
}

/// What `phux service status` prints once the unit exists, and how the verb
/// exits.
struct StatusReport {
    /// Everything the verb writes to stdout past the `unit` line.
    text: String,
    /// Whether the init system is running the unit. The exit code (`armed ||
    /// running`) and the marker sweep (`armed && running`) follow from it.
    running: bool,
}

/// The report as a pure function of the armed record and the probe. The
/// probe's stderr never reaches it, and an armed unit's not-found answer is
/// the expected state (ADR-0088), not a fault. The probe still runs when armed
/// because login bootstrap can complete the hand-over without clearing the
/// marker.
fn status_report(
    armed: bool,
    probe: impl FnOnce() -> std::io::Result<std::process::Output>,
) -> std::io::Result<StatusReport> {
    let output = probe()?;
    let running = output.status.success();
    let init_report = String::from_utf8_lossy(&output.stdout);
    let text = match (armed, running) {
        (true, false) => format!(
            "state armed — installed with --adopt and waiting for the running server to exit\n\
             \n\
             {ARMED_SUPERVISION_EXPLANATION}\n\
             \n\
             The init system is not running the unit — for an armed unit that is the\n\
             expected state, not a fault.\n"
        ),
        (true, true) => format!(
            "The hand-over armed by `phux service install --adopt` has completed: the init\n\
             system is running this unit now.\n\
             \n\
             {init_report}"
        ),
        (false, true) => init_report.into_owned(),
        (false, false) => {
            format!("{init_report}installed, but the init system is not running it.\n")
        }
    };
    Ok(StatusReport { text, running })
}

/// `phux service logs` — tail the canonical server log (the same file the
/// unit and the auto-spawn path write), via the same code as
/// `phux logs --server`.
pub(crate) fn run_logs(follow: bool, lines: u32) -> ExitCode {
    let log = phux_server::telemetry::server_log_path();
    let missing = format!(
        "phux service: no log at {} yet.\n\
         A server writes it when it next starts; `phux logs` lists every log path.",
        log.display()
    );
    super::logs::tail_file(&log, follow, lines, &missing)
}

/// `phux service prune-logs` — delete the per-pid client logs. Explicit, never
/// a side effect of install.
pub(crate) fn run_prune_logs(dry_run: bool) -> ExitCode {
    let dir = phux_server::telemetry::state_dir();
    let entries = match super::logs::client_log_paths(&dir) {
        Ok(entries) => entries,
        Err(err) => {
            error!("service: could not read {}: {err}", dir.display());
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
        error!("service: {failed} could not be removed.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// How many client logs are in the state dir (advisory, for the install report).
fn count_client_logs() -> std::io::Result<usize> {
    super::logs::client_log_paths(&phux_server::telemetry::state_dir()).map(|paths| paths.len())
}

/// A path as a string, lossily.
fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `$HOME`, or an error when it is unset or empty (never a cwd-relative path).
fn home_dir() -> Result<PathBuf, String> {
    home_dir_from(std::env::var_os("HOME"))
}

/// [`home_dir`] with `$HOME` injectable for tests.
fn home_dir_from(home: Option<std::ffi::OsString>) -> Result<PathBuf, String> {
    home.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set; cannot determine the per-user unit directory".to_owned())
}

/// `$XDG_CONFIG_HOME`, falling back to `$HOME/.config`.
fn config_home() -> Result<PathBuf, String> {
    config_home_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// [`config_home`] with both environment variables injectable.
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
    use super::reconcile::tests::{LEGACY_PLIST, LEGACY_UNIT, patched};
    use super::unit::tests::plan;
    use super::{
        HubEnsure, Manager, Reconcile, SupervisionState, adoption_marker_is_spent, arm_unit,
        config_home_from, dry_run_text, ensure_hub_in_unit, ensure_hub_in_wrapper,
        ensure_paired_policy, home_dir_from, render_launchd_plist, render_systemd_unit,
        render_unit, render_wrapper_script, report_policy_reach_with, resolve_plan,
        rewrite_unit_binary, status_report, unit_socket_override, unit_supervises,
    };
    use std::path::Path;
    use std::path::PathBuf;

    /// A captured init-system invocation, for driving [`status_report`]
    /// without an init system. `raw` is a wait(2) status: `0` is success,
    /// `1 << 8` is exit code 1.
    fn probe_output(raw: i32, stdout: &str, stderr: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(raw),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// What `launchctl print` writes to stderr for a job that is not loaded.
    const LAUNCHCTL_NOT_FOUND_STDERR: &str =
        "Bad request.\nCould not find service \"com.phux.server\" in domain for user gui: 501\n";

    #[test]
    fn silent_systemd_policy_reconciliation_still_reloads_the_unit() {
        let called = std::cell::Cell::new(false);
        report_policy_reach_with(
            Manager::Systemd,
            std::path::Path::new("unused"),
            false,
            false,
            |program, args| {
                called.set(true);
                assert_eq!(program, "systemctl");
                assert_eq!(args, ["--user", "daemon-reload"]);
                Ok(())
            },
        );
        assert!(
            called.get(),
            "JSON mode must suppress output, not side effects"
        );
    }

    /// An armed unit's not-found answer is the expected state (ADR-0088): the
    /// report stays in the armed vocabulary and forwards no tool stderr.
    #[test]
    fn an_armed_units_not_found_answer_is_translated_not_reported_as_a_fault() {
        let report = status_report(true, || {
            Ok(probe_output(1 << 8, "", LAUNCHCTL_NOT_FOUND_STDERR))
        })
        .expect("the probe ran");

        assert!(
            !report.running,
            "an armed unit is deliberately unloaded, so the probe fails — and the caller's \
             `armed || running` verdict still exits zero, while `armed && running` leaves the \
             marker in place"
        );
        assert!(report.text.contains("state armed"), "{}", report.text);
        assert!(
            report.text.contains(super::ARMED_SUPERVISION_EXPLANATION),
            "the armed paragraph is the shared explanation verbatim: {}",
            report.text
        );
        assert!(
            report.text.contains("expected state, not a fault"),
            "the not-found answer must be translated into the armed vocabulary: {}",
            report.text
        );
        for leaked in [
            "Bad request",
            "Could not find service",
            "installed, but the init system",
        ] {
            assert!(
                !report.text.contains(leaked),
                "leaked into the armed report: {leaked:?}\n{}",
                report.text
            );
        }
    }

    /// A not-running unit gets phux's verdict plus the probe's stdout, never its
    /// stderr.
    #[test]
    fn a_failed_probe_is_rendered_in_phux_vocabulary_not_the_tools_stderr() {
        let report = status_report(false, || {
            Ok(probe_output(
                3 << 8,
                "Active: inactive (dead)\n",
                LAUNCHCTL_NOT_FOUND_STDERR,
            ))
        })
        .expect("the probe ran");

        assert!(
            !report.running,
            "neither armed nor running is the one combination that exits non-zero"
        );
        assert!(
            report
                .text
                .contains("installed, but the init system is not running it"),
            "{}",
            report.text
        );
        assert!(
            report.text.contains("Active: inactive (dead)"),
            "the tool's stdout is forwarded — it is the report proper: {}",
            report.text
        );
        assert!(
            !report.text.contains("Bad request"),
            "the tool's stderr leaked: {}",
            report.text
        );
    }

    /// Armed plus running means the hand-over completed (login bootstrap does not
    /// clear the marker).
    #[test]
    fn an_armed_marker_over_a_running_job_reports_completion_and_asks_for_a_sweep() {
        let report = status_report(true, || {
            Ok(probe_output(
                0,
                "com.phux.server = {\n\tstate = running\n}\n",
                "",
            ))
        })
        .expect("the probe ran");

        assert!(
            report.running,
            "a running job under an armed marker is `armed && running` — the caller's cue to \
             sweep the stale record"
        );
        assert!(report.text.contains("has completed"), "{}", report.text);
        assert!(
            report.text.contains("state = running"),
            "the init system's report is forwarded: {}",
            report.text
        );
        assert!(
            !report.text.contains("state armed"),
            "a completed hand-over must not still read as armed: {}",
            report.text
        );
    }

    /// The live-server path sweeps spent records and leaves a pending adopt alone.
    #[test]
    fn a_spent_adoption_marker_is_the_live_path_sweep() {
        let armed = || SupervisionState::Armed {
            manager: Manager::Systemd,
            unit: PathBuf::from("/tmp/phux-test.service"),
        };
        assert!(
            adoption_marker_is_spent(&SupervisionState::MarkerWithoutUnit, false),
            "a marker whose unit has vanished can never complete"
        );
        assert!(
            adoption_marker_is_spent(&armed(), true),
            "armed + running is the completed login hand-over"
        );
        assert!(
            !adoption_marker_is_spent(&armed(), false),
            "armed + not-running is still waiting for the incumbent"
        );
        assert!(
            !adoption_marker_is_spent(&SupervisionState::NotArmed, true),
            "no marker means there is nothing to sweep, even if a unit is running"
        );
    }

    /// A running job's report is forwarded from stdout without its stderr.
    #[test]
    fn a_running_jobs_report_is_forwarded_without_its_stderr() {
        let report = status_report(false, || {
            Ok(probe_output(
                0,
                "com.phux.server = {\n}\n",
                "noise on stderr\n",
            ))
        })
        .expect("the probe ran");

        assert!(report.running);
        assert!(report.text.contains("com.phux.server"), "{}", report.text);
        assert!(!report.text.contains("noise on stderr"), "{}", report.text);
    }

    /// A unit with a remote listener writes `paired` where no mode is set,
    /// keeps an existing `paired`, and refuses `local`; a unit without one
    /// leaves the config alone (ADR-0179).
    #[test]
    fn a_remote_listener_unit_settles_the_policy_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");

        let mut local_only = plan();
        local_only.quic = None;
        local_only.listen = None;
        ensure_paired_policy(&local_only, &config, false).expect("no listener, nothing to settle");
        assert!(!config.exists(), "the config is left alone");

        std::fs::write(&config, "[sidebar]\nwidth = 30\n").expect("config");
        ensure_paired_policy(&plan(), &config, false).expect("an unset mode becomes paired");
        let written = std::fs::read_to_string(&config).expect("config");
        assert!(
            written.contains("width = 30") && written.contains("mode = \"paired\""),
            "{written}"
        );
        ensure_paired_policy(&plan(), &config, false).expect("paired stays");
        assert_eq!(std::fs::read_to_string(&config).expect("config"), written);

        std::fs::write(&config, "[policy]\nmode = \"local\"\n").expect("config");
        let refused = ensure_paired_policy(&plan(), &config, false).expect_err("local refuses");
        assert!(refused.contains("\"local\""), "{refused}");
        assert_eq!(
            std::fs::read_to_string(&config).expect("config"),
            "[policy]\nmode = \"local\"\n",
            "a refused install changes nothing"
        );
    }

    /// A relay-only host (`[[connector]]`) or one whose server inherits a
    /// listener address opens a remote entry point too, so its unit settles
    /// the mode the same way rather than crash-looping (ADR-0179).
    #[test]
    fn a_connector_or_inherited_listener_settles_the_policy_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        let mut local_only = plan();
        local_only.quic = None;
        local_only.listen = None;

        let connector = "[[connector]]\nrelay = \"wss://relay.example:443\"\n";
        std::fs::write(&config, connector).expect("config");
        ensure_paired_policy(&local_only, &config, false).expect("a connector becomes paired");
        let written = std::fs::read_to_string(&config).expect("config");
        assert!(
            written.contains("relay.example") && written.contains("mode = \"paired\""),
            "{written}"
        );

        std::fs::write(&config, format!("{connector}[policy]\nmode = \"local\"\n"))
            .expect("config");
        let refused = ensure_paired_policy(&local_only, &config, false).expect_err("local refuses");
        assert!(refused.contains("[[connector]]"), "{refused}");

        std::fs::write(&config, "").expect("config");
        ensure_paired_policy(&local_only, &config, true)
            .expect("an inherited listener address becomes paired");
        let written = std::fs::read_to_string(&config).expect("config");
        assert!(written.contains("mode = \"paired\""), "{written}");
    }

    /// Unit paths are per-user and profile-scoped, so a dev install cannot
    /// overwrite the production unit file.
    #[test]
    fn a_non_default_profile_writes_its_unit_beside_the_default_one() {
        let default_launchd = Manager::Launchd
            .unit_path(None)
            .expect("HOME is set in this test process");
        let dev_launchd = Manager::Launchd
            .unit_path(Some("dev"))
            .expect("HOME is set in this test process");
        assert!(default_launchd.ends_with("Library/LaunchAgents/com.phux.server.plist"));
        assert!(dev_launchd.ends_with("com.phux.server.dev.plist"));
        assert_eq!(default_launchd.parent(), dev_launchd.parent());

        let default_systemd = Manager::Systemd
            .unit_path(None)
            .expect("HOME is set in this test process");
        let dev_systemd = Manager::Systemd
            .unit_path(Some("dev"))
            .expect("HOME is set in this test process");
        assert!(default_systemd.ends_with("systemd/user/phux.service"));
        assert!(dev_systemd.ends_with("phux-dev.service"));
        assert_eq!(default_systemd.parent(), dev_systemd.parent());
    }

    /// Tests are dev builds: the production unit is refused to every writer,
    /// the dev profile's unit beside it is not.
    #[test]
    fn a_dev_build_is_refused_the_production_unit() {
        for manager in [Manager::Launchd, Manager::Systemd] {
            let default = manager.unit_path(None).expect("HOME is set");
            assert_eq!(
                manager.writable_unit_path(None).is_err(),
                phux_config::production::refuse_dev_on_production_state(&default).is_err(),
                "{}",
                default.display()
            );
            assert!(manager.writable_unit_path(Some("dev")).is_ok());
        }
    }

    /// The plist label follows the plan's profile.
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

    /// A dry run renders on every platform; with no generator it is the systemd
    /// unit (ADR-0055).
    #[test]
    fn a_dry_run_renders_even_with_no_unit_generator_for_the_platform() {
        let text = dry_run_text(None, &plan());
        assert!(
            !text.is_empty(),
            "an unsupported platform must still get a unit to adapt"
        );
        assert_eq!(text, dry_run_text(Some(Manager::Systemd), &plan()));
        assert!(text.contains("[Service]"), "got {text}");
    }

    /// With `HOME` (and `XDG_CONFIG_HOME`) unset or empty, unit paths are
    /// refused rather than resolved relative to the cwd.
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

    #[test]
    fn rewrite_unit_binary_points_launchd_and_systemd_at_the_new_install() {
        let next = Path::new("/Users/me/.local/bin/phux");
        let launchd = patched(rewrite_unit_binary(Manager::Launchd, LEGACY_PLIST, next));
        assert!(
            launchd.contains("<string>/Users/me/.local/bin/phux</string>"),
            "{launchd}"
        );
        assert!(
            !launchd.contains("<string>/usr/local/bin/phux</string>"),
            "{launchd}"
        );
        assert!(
            launchd.contains("<string>--hub</string>"),
            "binary rewrite must not drop flags:\n{launchd}"
        );

        let systemd = patched(rewrite_unit_binary(Manager::Systemd, LEGACY_UNIT, next));
        assert!(
            systemd.contains("ExecStart=/Users/me/.local/bin/phux server --hub"),
            "{systemd}"
        );
        assert!(
            systemd.contains("Environment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\""),
            "binary rewrite must not drop env:\n{systemd}"
        );
    }

    #[test]
    fn rewrite_unit_binary_is_a_fixed_point_when_the_path_already_matches() {
        let already = Path::new("/usr/local/bin/phux");
        assert_eq!(
            rewrite_unit_binary(Manager::Launchd, LEGACY_PLIST, already),
            Reconcile::Current
        );
        assert_eq!(
            rewrite_unit_binary(Manager::Systemd, LEGACY_UNIT, already),
            Reconcile::Current
        );
    }

    /// Patching `--hub` into a generated unit equals generating with `--hub`, and
    /// patching again is a no-op.
    #[test]
    fn ensuring_hub_matches_generating_with_hub() {
        for manager in [Manager::Launchd, Manager::Systemd] {
            let without = render_unit(manager, &plan());
            let mut with_hub = plan();
            with_hub.hub = true;
            let expected = render_unit(manager, &with_hub);
            match ensure_hub_in_unit(manager, &without) {
                HubEnsure::Patched(patched) => assert_eq!(
                    patched, expected,
                    "{manager:?} --hub patch drifted from the hub=true renderer"
                ),
                other => panic!("{manager:?} expected a patch, got {other:?}\n{without}"),
            }
            assert!(
                matches!(ensure_hub_in_unit(manager, &expected), HubEnsure::Current),
                "{manager:?} a hub unit must be a no-op to patch"
            );
        }
    }

    /// With `--restore` the unit execs `/bin/sh`, so `--hub` goes into the
    /// wrapper's server line only.
    #[test]
    fn ensuring_hub_on_a_restore_unit_names_the_wrapper() {
        let mut plan = plan();
        plan.restore = Some(PathBuf::from("/home/u/.local/state/phux/workspace.json"));
        for manager in [Manager::Launchd, Manager::Systemd] {
            match ensure_hub_in_unit(manager, &render_unit(manager, &plan)) {
                HubEnsure::Wrapper(path) => assert_eq!(path, plan.wrapper),
                other => panic!("{manager:?} expected Wrapper, got {other:?}"),
            }
        }

        let without = render_wrapper_script(&plan);
        plan.hub = true;
        let expected = render_wrapper_script(&plan);
        match ensure_hub_in_wrapper(&without) {
            HubEnsure::Patched(patched) => assert_eq!(patched, expected),
            other => panic!("expected a wrapper patch, got {other:?}\n{without}"),
        }
        assert!(matches!(
            ensure_hub_in_wrapper(&expected),
            HubEnsure::Current
        ));
        // The archive export line must not gain --hub.
        assert!(
            !expected.contains("archive export --hub"),
            "only the server start line takes --hub:\n{expected}"
        );
    }

    #[test]
    fn ensuring_hub_refuses_an_unparseable_unit() {
        assert!(matches!(
            ensure_hub_in_unit(Manager::Launchd, "not a plist"),
            HubEnsure::Unrecognized(_)
        ));
        assert!(matches!(
            ensure_hub_in_unit(Manager::Systemd, "[Unit]\nDescription=no exec\n"),
            HubEnsure::Unrecognized(_)
        ));
    }

    /// The reconcile probes the socket the unit pins, through the renderers'
    /// escaping.
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

    /// The installer's ambient `PATH` is never frozen into the unit; the init
    /// system supplies its own.
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

    /// Only the unit that owns this socket completes a pending adoption
    /// (ADR-0088), checked against really rendered units.
    #[test]
    fn only_the_unit_that_owns_this_socket_completes_an_adoption() {
        let overridden = PathBuf::from("/tmp/custom/phux.sock");
        let plan = resolve_plan(None, None, false, Some(overridden.clone()), false)
            .expect("resolve_plan with a socket override");

        for manager in [Manager::Launchd, Manager::Systemd] {
            let body = render_unit(manager, &plan);
            assert!(
                unit_supervises(manager, &body, &overridden),
                "{manager:?}: a unit carrying PHUX_SOCKET must match that socket"
            );
            assert!(
                !unit_supervises(manager, &body, &PathBuf::from("/tmp/somewhere-else.sock")),
                "{manager:?}: a unit for another socket must not complete this adoption"
            );
        }

        let default = resolve_plan(None, None, false, None, false).expect("resolve_plan");
        for manager in [Manager::Launchd, Manager::Systemd] {
            let body = render_unit(manager, &default);
            assert!(
                unit_supervises(manager, &body, &default.socket_path),
                "{manager:?}: a default-socket unit must match the resolved default"
            );
            assert!(
                !unit_supervises(manager, &body, &overridden),
                "{manager:?}: a default-socket unit must not claim an overridden socket"
            );
        }
    }

    /// Arming never starts anything; on launchd it runs no command, and the
    /// written plist still carries `RunAtLoad`.
    #[test]
    fn arming_a_launchd_unit_runs_no_command() {
        arm_unit(Manager::Launchd)
            .expect("arming a launchd unit is writing the file, which the caller already did");

        let plan = resolve_plan(None, None, false, None, false).expect("resolve_plan");
        assert!(
            render_launchd_plist(&plan).contains("<key>RunAtLoad</key>\n  <true/>"),
            "an armed plist must still start the server at bootstrap"
        );
    }
}
