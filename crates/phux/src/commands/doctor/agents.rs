//! Agent-facing checks: the `install-claude` shim and the `phux skill` stub.

use super::Check;

/// Is the installed Claude shim the one this binary writes? The shim is a
/// script written once at install time, so upgrading phux does not upgrade it,
/// and old schemas misbehave (e.g. schema 1 stands the detector down). Warn,
/// never fail: doctor's exit code gates scripts unrelated to Claude.
pub(super) fn check_agent_shim() -> Check {
    use crate::commands::agent::shim;

    let Some(path) = shim::installed_shim_path() else {
        return Check::warn(
            "agent-shim",
            "cannot tell where the claude-in-phux shim would live because HOME is unset",
            "set HOME and re-run `phux doctor`",
        );
    };
    shim_check(shim::installed_shim_schema(&path), shim::SHIM_SCHEMA, &path)
}

/// Has `phux skill setup` (or `npx skills add no-phux/skills`) taught any
/// agent about phux? Not installed is normal, so it passes with a hint.
pub(super) fn check_skill() -> Check {
    match crate::skill::setup::doctor_summary() {
        Err(err) => Check::warn("skill", err, "set HOME and re-run `phux doctor`"),
        Ok((_, stale)) if !stale.is_empty() => Check::warn(
            "skill",
            format!("phux skill stub is stale for {}", stale.join(", ")),
            "run `phux skill setup` to update it",
        ),
        Ok((installed, _)) if !installed.is_empty() => Check::pass(
            "skill",
            format!("phux skill installed for {}", installed.join(", ")),
        ),
        Ok(_) => Check {
            hint: Some("run `phux skill setup` to teach your agents about phux".to_owned()),
            ..Check::pass("skill", "no phux skill installed for any agent")
        },
    }
}

/// The pure half of [`check_agent_shim`].
pub(super) fn shim_check(installed: Option<u32>, current: u32, path: &std::path::Path) -> Check {
    let where_ = path.display();
    match installed {
        // `install-claude` is opt-in; never installed is normal.
        None => Check::pass("agent-shim", "no claude-in-phux shim installed"),
        Some(found) if found == current => Check::pass(
            "agent-shim",
            format!("claude-in-phux shim at {where_} is current (schema {current})"),
        ),
        Some(found) if found < current => Check::warn(
            "agent-shim",
            format!(
                "claude-in-phux shim at {where_} is schema {found}, but this phux writes \
                 schema {current}"
            ),
            format!(
                "re-run `phux agent install-claude` — {}",
                stale_shim_consequence(found)
            ),
        ),
        // Newer on disk than this binary writes: an older phux.
        Some(found) => Check::warn(
            "agent-shim",
            format!(
                "claude-in-phux shim at {where_} is schema {found}, newer than the schema \
                 {current} this phux writes"
            ),
            "this binary is older than the installed shim: upgrade it with `phux update`, \
             or re-run `phux agent install-claude` to pin the shim to this binary",
        ),
    }
}

/// What the user is living with, per stale schema (tracks the
/// `install-claude` upgrade notice).
const fn stale_shim_consequence(found: u32) -> &'static str {
    match found {
        0 | 1 => {
            "schema 1 declares an agent state on every Claude hook, which stands the \
             server-side detector down for the whole session, so a dead Claude keeps a \
             live badge"
        }
        2 => {
            "schema 2 rewrites the agent record on every Claude hook, which resets the \
             detected state at the end of every turn and makes `phux agent wait` report \
             the agent as departed"
        }
        3 => {
            "schema 3 leaves lifecycle timing to screen detection and cannot publish the \
             Claude Stop hook's exact `done` edge"
        }
        4 => {
            "schema 4 never reads the hook payload, so it cannot open or feed the pane's \
             agent session stream and `PreToolUse`/`PostToolUse` are not wired"
        }
        5 => {
            "schema 5 feeds lifecycle records only, so the pane's agent session stream \
             carries no transcript of prompts, tool results, or replies"
        }
        _ => "the installed shim predates this binary's wrapper behavior",
    }
}
