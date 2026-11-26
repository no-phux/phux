//! The environment a pane child must not inherit from the server.
//!
//! The pane's shell, login mode, `TERM`, ids and cwd are launch intent,
//! built once by [`phux_terminal_api::launch`].

#![allow(
    clippy::redundant_pub_crate,
    reason = "private submodule re-exported by spawn at crate visibility"
)]

use portable_pty::CommandBuilder;

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

/// Rex's nested-terminal resume token. A server started inside a Rex pane
/// carries it, and OSC 7577 says a nested terminal must not pass it on.
const OSC7577_TOKEN: &str = "OSC7577_TOKEN";

/// Strip Rex's `OSC7577_TOKEN` from a pane child's environment.
pub(crate) fn clear_osc7577_token(cmd: &mut CommandBuilder) {
    cmd.env_remove(OSC7577_TOKEN);
}
