//! Pane command construction: the shell, login mode, and the environment
//! a pane child starts with.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private submodule re-exported by spawn at crate visibility"
)]

use portable_pty::CommandBuilder;
use tracing::warn;

/// The argv flag that puts `shell` into login mode.
///
/// Matched on basename: `-l` for bash/zsh/sh, `--login` for fish, `None` for anything else.
/// Unknown shells get no flag because an unrecognized flag can fail the exec
/// (ADR-0073).
#[must_use]
pub fn login_flag_for_shell(shell: &str) -> Option<&'static str> {
    let name = std::path::Path::new(shell)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(shell);
    match name {
        "bash" | "zsh" | "sh" => Some("-l"),
        "fish" => Some("--login"),
        _ => None,
    }
}

/// Add `shell`'s login flag to `cmd` when `login` is set and the shell is known.
fn apply_login_mode(cmd: &mut CommandBuilder, shell: &str, login: bool) {
    if !login {
        return;
    }
    if let Some(flag) = login_flag_for_shell(shell) {
        cmd.arg(flag);
    }
}

/// A [`CommandBuilder`] for a plain interactive `shell`.
///
/// `login` requests login mode (for service-managed servers, whose panes
/// otherwise lack profile-provided `PATH`); a terminal-launched server
/// passes `false` because re-sourcing profiles is not idempotent.
///
/// `TERM` is [`DEFAULT_TERM`]: ghostty's terminfo advertises kitty keyboard
/// support, which breaks ncurses apps such as htop. `defaults.term` and
/// `SPAWN_RESOURCE.term` opt back in.
#[must_use]
pub fn default_shell_command(shell: &str, login: bool) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    apply_login_mode(&mut cmd, shell, login);
    cmd.env("TERM", DEFAULT_TERM);
    cmd
}

/// Baseline `TERM` for [`default_shell_command`] and [`shell_command`];
/// matches the `defaults.term` schema default, which [`apply_term`]
/// overrides at runtime.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// Override `TERM` on `cmd` with the server's configured `defaults.term`.
pub fn apply_term(cmd: &mut CommandBuilder, term: &str) {
    cmd.env("TERM", term);
}

/// Inject `PHUX_TERMINAL_ID`, the pane's local wire id, so in-pane processes
/// can name their pane. Only `Local` ids yield a value. Paired with
/// [`apply_server_socket`], which names the server.
pub fn apply_terminal_id(
    cmd: &mut CommandBuilder,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
) {
    if let Some(id) = wire_terminal_id.local_id() {
        cmd.env("PHUX_TERMINAL_ID", id.to_string());
    }
}

/// Inject `PHUX_SOCKET` so in-pane `phux` verbs reach this server rather
/// than the default socket. `None` leaves the inherited value alone.
pub fn apply_server_socket(cmd: &mut CommandBuilder, socket_path: Option<&std::path::Path>) {
    if let Some(path) = socket_path {
        cmd.env("PHUX_SOCKET", path.as_os_str());
    }
}

/// Strip the server-private `PHUX_UPGRADE_*` handoff from a pane child's
/// environment, so a `phux server` inside the pane cannot re-exec into the
/// outer server's binary on upgrade.
pub(crate) fn clear_upgrade_handoff_env(cmd: &mut CommandBuilder) {
    for key in phux_resource::handoff_env::HANDOFF_ENV_VARS {
        cmd.env_remove(key);
    }
}

/// Claude Code's nested-session markers. A `claude` that inherits
/// `CLAUDE_CODE_CHILD_SESSION` silently keeps no transcript.
const CLAUDE_CODE_ENV_PREFIX: &str = "CLAUDE_CODE_";
const CLAUDE_CODE_ENV_BARE: &str = "CLAUDECODE";

/// Strip Claude Code's `CLAUDE_CODE_*` / `CLAUDECODE` markers from a pane
/// child's environment. A server started from inside a Claude Code session
/// carries them forever, and every later `claude` in a pane would then run
/// as a nested session with no transcript. Matched by prefix because Claude
/// Code owns that vocabulary.
pub(crate) fn clear_agent_host_env(cmd: &mut CommandBuilder) {
    let leaked: Vec<String> = cmd
        .iter_full_env_as_str()
        .filter(|&(key, _)| key.starts_with(CLAUDE_CODE_ENV_PREFIX) || key == CLAUDE_CODE_ENV_BARE)
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in leaked {
        cmd.env_remove(key);
    }
}

/// Apply a wire-supplied cwd to `cmd`.
///
/// A cwd already on `cmd` (from a server-wide override command) wins. A path
/// that is not an enterable directory is dropped with a warning, so a stale
/// client path degrades to the default directory instead of failing the
/// spawn. `session` is for the log only. The archived directory string is
/// kept on `phux.session.directory/v1` by workspace restore, not on this
/// child.
pub fn apply_spawn_cwd(builder: &mut CommandBuilder, cwd: Option<&str>, session: &str) {
    let Some(path) = cwd else {
        return;
    };
    if builder.get_cwd().is_some() {
        return;
    }
    if dir_is_enterable(std::path::Path::new(path)) {
        builder.cwd(path);
    } else {
        warn!(
            session = %session,
            cwd = %path,
            "wire cwd is not an enterable directory; \
             falling back to the default spawn directory",
        );
    }
}

/// Env mark that forbids falling back to another directory when the recorded cwd cannot be entered.
pub(crate) const STRICT_CWD_KEY: &str = "PHUX_FD_SHRINK_STRICT_CWD";

/// Apply recorded cwd intent without permitting any executor fallback directory.
pub fn apply_strict_spawn_cwd(builder: &mut CommandBuilder, cwd: &std::path::Path) {
    builder.cwd(cwd);
    builder.env(STRICT_CWD_KEY, "1");
}

/// `path` is a directory the child can `chdir` into (search permission, not
/// just `is_dir`). Best effort: TOCTOU can still fail the spawn.
fn dir_is_enterable(path: &std::path::Path) -> bool {
    path.is_dir() && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

/// A [`CommandBuilder`] that runs `command` via `<shell> [-l] -c`, so quoting
/// behaves as at a prompt and the pane closes when the command exits.
#[must_use]
pub fn shell_command(shell: &str, command: &str, login: bool) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    apply_login_mode(&mut cmd, shell, login);
    cmd.arg("-c");
    cmd.arg(command);
    cmd.env("TERM", DEFAULT_TERM);
    cmd
}
