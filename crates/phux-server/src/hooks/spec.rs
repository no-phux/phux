//! The generated reference's view of the hook events.

use super::{
    AFTER_NEW_PANE, AGENT_STATE_CHANGED, CLIENT_ATTACHED, CLIENT_DETACHED, FOCUS_CHANGED,
    PANE_EXIT, WORKTREE_CREATED, vocab,
};

/// One hook event as the generated reference documents it: the vocab's
/// name and context keys plus this dispatcher's "fires when" prose. A test
/// pins it to the real constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEventSpec {
    /// Canonical event name, a member of [`phux_config::vocab::HOOK_EVENTS`].
    pub name: &'static str,
    /// When the server fires the event — one sentence of prose.
    pub doc: &'static str,
    /// The event's context keys, sorted (optional ones included).
    pub context_keys: &'static [&'static str],
}

/// Every hook event, in [`vocab::HOOK_EVENTS`] order, for the generated
/// `docs/reference/hooks.md`.
#[must_use]
pub fn hook_event_specs() -> Vec<HookEventSpec> {
    vocab::HOOK_EVENTS
        .iter()
        .map(|&name| HookEventSpec {
            name,
            doc: event_doc(name),
            context_keys: vocab::hook_context_keys(name).unwrap_or(&[]),
        })
        .collect()
}

/// The "fires when" sentence for an event name (a test enforces coverage).
fn event_doc(name: &str) -> &'static str {
    match name {
        AFTER_NEW_PANE => {
            "A pane's actor spawned: fires right after pane creation, \
             before the inner process has produced output."
        }
        PANE_EXIT => {
            "A pane's inner process exited. `exit-code` is present only \
             when the OS reported a code (absent for a signal-killed \
             child)."
        }
        FOCUS_CHANGED => "A client's focus landed on a pane.",
        CLIENT_ATTACHED => "A client's attach completed.",
        CLIENT_DETACHED => {
            "An attached client detached for any reason (explicit detach \
             or transport drop). `session` is absent if the session was \
             reaped before the detach ran."
        }
        AGENT_STATE_CHANGED => {
            "The detector's published agent state for a pane actually \
             changed. `from` is absent on a first sighting; \
             `agent-name` is absent for an anonymous agent; a withdrawn \
             record arrives as `to = \"unknown\"`."
        }
        WORKTREE_CREATED => {
            "`phux worktree new` created a checkout, before its session \
             starts. The CLI fires it, not the server, and \
             waits for each hook; `repo` is the primary checkout."
        }
        other => unreachable!("no doc prose for hook event `{other}`"),
    }
}
