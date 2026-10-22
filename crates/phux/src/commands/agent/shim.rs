//! Install the `claude` adoption shim: plain `claude` becomes a phux session.
//!
//! The shim is deliberately installed into a phux-owned directory and activated
//! by one bounded shell-rc block. It never overwrites the real Claude binary.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod install;
mod rc;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_install;
#[cfg(test)]
mod tests_stream;
mod wrapper;

use install::{install_claude, uninstall_claude};
pub(crate) use install::{installed_shim_path, installed_shim_schema};

const BLOCK_BEGIN: &str = "# >>> phux agent shims >>>";
const BLOCK_END: &str = "# <<< phux agent shims <<<";
const MANIFEST: &str = "claude-install.json";

/// Behavioral version of the generated wrapper, stamped into the script so
/// an installed copy (which a binary upgrade does not rewrite) can be
/// recognized as stale by `install-claude` and `phux doctor`.
///
/// 1 declared a `state` on every hook (standing the detector down); 2 wrote
/// identity on every hook (clobbering derived state each turn); 3 writes
/// identity once at session start; 4 feeds per-turn edges through
/// `report-state`; 5 reads the hook payload and, when the server serves
/// `AgentSession` resources, feeds a session stream with `agent emit`; 6 adds
/// the `phux.transcript/v1` entries for prompts, tool results, and replies.
pub(crate) const SHIM_SCHEMA: u32 = 6;

/// Prefix of the wrapper's schema stamp line. A `#` comment, so it is inert
/// to `/bin/sh` and greppable without executing anything.
const SCHEMA_MARKER: &str = "# phux-shim-schema: ";

/// The Claude Code hooks the installer registers: `(event, matcher, arm)`,
/// each becoming `<shim> --phux-hook <arm>`. Every arm must be handled by the
/// wrapper's `stream_state`.
const HOOKS: &[(&str, &str, &str)] = &[
    ("SessionStart", "", "start"),
    ("UserPromptSubmit", "", "working"),
    ("PreToolUse", "", "tool-start"),
    ("PostToolUse", "", "tool-end"),
    ("PostToolUseFailure", "", "tool-end"),
    ("PermissionRequest", "", "blocked"),
    (
        "Notification",
        "permission_prompt|idle_prompt|elicitation_dialog",
        "blocked",
    ),
    ("Stop", "", "done"),
    ("SessionEnd", "", "clear"),
];

pub(super) fn run_install_claude(shell: Option<&str>, real: Option<&Path>) -> ExitCode {
    let shell = shell.map_or_else(detected_shell, str::to_owned);
    match install_claude(&shell, real) {
        Ok(report) => {
            let shim = report.shim.display();
            match report.replaced {
                None => outln!("installed claude-in-phux shim at {shim}"),
                Some(prior) if prior >= SHIM_SCHEMA => {
                    outln!("reinstalled claude-in-phux shim at {shim} (schema {SHIM_SCHEMA})");
                }
                Some(prior) => {
                    outln!(
                        "upgraded claude-in-phux shim at {shim} (schema {prior} -> {SHIM_SCHEMA})"
                    );
                    let was = if prior <= 1 {
                        "declared an agent state on every Claude hook, which stood the \
                         server-side detector down for the whole session"
                    } else if prior == 2 {
                        "rewrote the agent record on every Claude hook, which reset the \
                         detected state at the end of every turn and made `phux agent wait` \
                         report the agent as departed"
                    } else if prior == 3 {
                        "left lifecycle timing to screen detection and could not publish \
                         the Claude Stop hook's exact `done` edge"
                    } else if prior == 4 {
                        "reported lifecycle edges to the detector but never read the hook \
                         payload, so it could not open or feed the pane's agent session \
                         stream"
                    } else {
                        "fed the agent session stream lifecycle records only, so phones \
                         and other stream readers could not show the conversation"
                    };
                    outln!(
                        "schema {prior} {was}; a Claude already running picks the new shim up \
                         on its next session"
                    );
                }
            }
            outln!("activated it in {}", report.rc.display());
            outln!("open a new shell, then plain `claude` launches inside phux");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

pub(super) fn run_uninstall_claude() -> ExitCode {
    match uninstall_claude() {
        Ok(Some(rc)) => {
            outln!(
                "removed claude-in-phux shim and activation from {}",
                rc.display()
            );
            ExitCode::SUCCESS
        }
        Ok(None) => {
            outln!("claude-in-phux shim is not installed");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

fn detected_shell() -> String {
    std::env::var_os("SHELL")
        .and_then(|path| PathBuf::from(path).file_name().map(ToOwned::to_owned))
        .and_then(|name| name.to_str().map(str::to_owned))
        .filter(|name| matches!(name.as_str(), "zsh" | "bash" | "fish"))
        .unwrap_or_else(|| "zsh".to_owned())
}

fn fail(message: &str) -> ExitCode {
    error!("agent: {message}");
    ExitCode::FAILURE
}

struct InstallReport {
    shim: PathBuf,
    rc: PathBuf,
    /// Schema of the shim this install replaced, if one was already on disk.
    /// `None` on a first install.
    replaced: Option<u32>,
}
