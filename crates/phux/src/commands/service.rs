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

mod adoption;
mod linger;
mod live_unit;
mod policy;
mod reconcile;
pub(crate) mod supervision;
mod unit;

pub(crate) use adoption::{
    ARMED_SUPERVISION_EXPLANATION, Handover, armed_adoption_unit, complete_pending_adoption,
    sweep_stale_adoption_marker,
};
use adoption::{
    Subject, SupervisionState, adoption_marker_path, clear_adoption_pending, mark_adoption_pending,
    probe_unit, supervision_state, unit_is_running, unit_supervises,
};
pub(crate) use linger::probe_linger;
pub(crate) use live_unit::{PORT_TAKEN, listeners_on_resume};
use policy::settle_policy;
pub(crate) use policy::write_paired_mode;

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
pub(crate) fn systemd_unit() -> String {
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
    let wrapper = phux_config::instance::state_dir().join("service-wrapper.sh");
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
    live_unit::unit_env(manager, body, "PHUX_SOCKET").map(PathBuf::from)
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
                    .unwrap_or_else(phux_config::socket::default_socket_path),
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
                .unwrap_or_else(phux_config::socket::default_socket_path),
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
    let state = phux_config::instance::state_dir();
    let socket_path = socket
        .clone()
        .unwrap_or_else(phux_config::socket::default_socket_path);
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
        log: phux_config::instance::server_log_path(),
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
#[expect(
    clippy::too_many_lines,
    reason = "the install's refusals and hand-over branches read in order; each is already one call"
)]
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

    if let Some(taken) = live_unit::taken_port(&plan, incumbent_live) {
        error!("service: {taken}; pick another port, or free that one. Nothing was changed.");
        return ExitCode::FAILURE;
    }

    if incumbent_live && already_supervised(manager, &plan) {
        return live_unit::refresh_running(manager, &plan);
    }

    let unit_path = match manager.writable_unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
    };

    // The policy first: a unit that could never start is never written.
    let mut before = live_unit::Before::read(manager, &unit_path);
    if let Err(err) = before
        .settle_policy(&plan)
        .and_then(|()| write_unit_files(manager, &plan, &unit_path))
    {
        return before.refuse(&plan, &err);
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
        // The running server takes the unit's listeners now, panes kept.
        let rebound = match live_unit::bind_listeners(&plan) {
            Ok(rebound) => rebound,
            Err(err) => return before.refuse(&plan, &err),
        };
        report_adopt(manager, &plan, &unit_path, rebound);
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

/// `--adopt` over a server this unit already runs has nothing to hand over,
/// so installers and onboarding can run it whatever state the host is in.
/// Reports and ensures linger when so.
fn already_supervised(manager: Manager, plan: &ServicePlan) -> bool {
    let Ok(unit_path) = manager.unit_path(profile_suffix().as_deref()) else {
        return false;
    };
    let supervises = std::fs::read_to_string(&unit_path)
        .is_ok_and(|body| unit_supervises(manager, &body, &plan.socket_path));
    if !supervises || !unit_is_running(manager) {
        return false;
    }
    outln!("phux service already running (nothing to hand over).");
    outln!("  unit    {}", unit_path.display());
    report_linger_for(manager);
    true
}

/// On Linux a user unit only outlives logout and starts at boot when the user
/// lingers; turn it on (once, here, for every caller of `install`).
fn report_linger_for(manager: Manager) {
    if manager == Manager::Systemd {
        linger::report_linger(linger::ensure_linger());
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

/// Report an `--adopt` install: what was written, what was not done, and when
/// supervision begins. Must not read as "the running server is supervised".
fn report_adopt(manager: Manager, plan: &ServicePlan, unit_path: &Path, rebound: bool) {
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
    if rebound {
        outln!("  panes   kept — the running server reloaded in place to open its listener");
    } else {
        outln!("  panes   untouched — the running server was not signalled");
    }
    outln!();
    outln!(
        "The server on {} keeps running: same process, same panes,\n\
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
    report_linger_for(manager);
}

/// Write the unit file (and the wrapper, under `--restore`), making the
/// directories the init system expects.
fn write_unit_files(manager: Manager, plan: &ServicePlan, unit_path: &Path) -> Result<(), String> {
    refuse_dev_on_production_credentials(plan)?;
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    if let Some(parent) = plan.log.parent() {
        phux_server::owner_dir::create(parent)
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
    report_linger_for(manager);
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

    let wrapper = phux_config::instance::state_dir().join("service-wrapper.sh");
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
    let log = phux_config::instance::server_log_path();
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
    let dir = phux_config::instance::state_dir();
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
    super::logs::client_log_paths(&phux_config::instance::state_dir()).map(|paths| paths.len())
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

/// Run `phux service`: the installed-supervisor lifecycle verbs.
pub(crate) fn run(args: crate::commands::ServiceArgs, socket: Option<PathBuf>) -> ExitCode {
    use crate::commands::ServiceAction;
    match args.action {
        ServiceAction::Install {
            quic,
            listen,
            restore,
            hub,
            adopt,
            print,
        } => run_install(
            quic,
            listen,
            restore,
            socket,
            hub,
            if adopt {
                Takeover::Adopt
            } else {
                Takeover::Refuse
            },
            print,
        ),
        ServiceAction::Reconcile { print } => run_reconcile(print),
        ServiceAction::Uninstall => run_uninstall(),
        ServiceAction::Status => run_status(),
        ServiceAction::Logs { follow, lines } => run_logs(follow, lines),
        ServiceAction::PruneLogs { dry_run } => run_prune_logs(dry_run),
    }
}

#[cfg(test)]
mod tests;
