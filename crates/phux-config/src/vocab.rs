//! Validation vocabulary: the canonical action and hook-event names, what a
//! hook entry can reference, and a did-you-mean suggester.
//!
//! These lists are the single source of truth; the client dispatcher and the
//! server's hook dispatcher are pinned to them by agreement tests.

use crate::Action;

/// Canonical names of every action the client dispatcher handles.
pub const ACTION_NAMES: &[&str] = &[
    "split-pane",
    "move-pane",
    "kill-pane",
    "new-window",
    "go-to-directory",
    "kill-window",
    "next-window",
    "previous-window",
    "select-window",
    "move-window",
    "rename-window",
    "rename-session",
    "focus-direction",
    "resize-pane",
    "show-help",
    "getting-started",
    "copy-mode",
    "detach",
    "next-pane",
    "previous-pane",
    "last-pane",
    "toggle-zoom",
    "toggle-sidebar",
    "command-palette",
    "context-menu",
    "window-picker",
    "session-picker",
    "agent-fleet",
    "focus-pane",
    "next-attention",
    "return-from-attention",
    "switch-session",
    "switch-host",
    "new-session",
    "take-input",
    "give-input",
    "signal-terminal",
    "set-pane",
    "plugin-action",
    "plugin-pane",
    "reload-config",
    "settings",
    "report-bug",
];

/// Hook point: pane creation (`docs/consumers/tui.md` §9).
pub const AFTER_NEW_PANE: &str = "after-new-pane";
/// Hook point: inner process exit.
pub const PANE_EXIT: &str = "pane-exit";
/// Hook point: a client changed focus to a pane.
pub const FOCUS_CHANGED: &str = "focus-changed";
/// Hook point: client attach completed.
pub const CLIENT_ATTACHED: &str = "client-attached";
/// Hook point: client detach (any reason).
pub const CLIENT_DETACHED: &str = "client-detached";
/// Hook point: a pane's derived agent state changed (ADR-0046).
pub const AGENT_STATE_CHANGED: &str = "agent-state-changed";

/// Every valid hook event name (`docs/consumers/tui.md` §9).
pub const HOOK_EVENTS: &[&str] = &[
    AFTER_NEW_PANE,
    PANE_EXIT,
    FOCUS_CHANGED,
    CLIENT_ATTACHED,
    CLIENT_DETACHED,
    AGENT_STATE_CHANGED,
];

/// The context keys a hook event can carry (sorted, optional keys
/// included); `None` for an unknown event. A `when` key outside the list can
/// never match.
#[must_use]
pub fn hook_context_keys(event: &str) -> Option<&'static [&'static str]> {
    Some(match event {
        AFTER_NEW_PANE => &["session", "terminal-id"],
        PANE_EXIT => &["exit-code", "terminal-id"],
        FOCUS_CHANGED => &["client-id", "terminal-id"],
        CLIENT_ATTACHED | CLIENT_DETACHED => &["client-id", "session"],
        AGENT_STATE_CHANGED => &["agent-kind", "agent-name", "from", "terminal-id", "to"],
        _ => return None,
    })
}

/// Whether a hook action can ever execute server-side.
///
/// Only `run` with a non-blank string or non-empty all-string `command`
/// does; everything else (including the deliberate `noop`) consumes the
/// event and runs nothing.
#[must_use]
pub fn hook_action_is_executable(action: &Action) -> bool {
    let Action::Parameterized(parameterized) = action else {
        return false;
    };
    if parameterized.action != "run" {
        return false;
    }
    match parameterized.args.get("command") {
        Some(toml::Value::String(command)) => !command.trim().is_empty(),
        Some(toml::Value::Array(items)) => {
            !items.is_empty() && items.iter().all(|item| item.as_str().is_some())
        }
        _ => false,
    }
}

/// Largest Levenshtein distance still offered as a suggestion: covers the
/// common typo shapes without suggesting for garbage.
const MAX_SUGGESTION_DISTANCE: usize = 2;

/// The closest candidate within `MAX_SUGGESTION_DISTANCE` (ties go to the
/// earlier one), e.g. `kill-pain` suggests `kill-pane`.
#[must_use]
pub fn did_you_mean<'a>(input: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(usize, &'a str)> = None;
    for &candidate in candidates {
        let distance = levenshtein(input, candidate);
        if distance <= MAX_SUGGESTION_DISTANCE
            && best.is_none_or(|(best_distance, _)| distance < best_distance)
        {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// Levenshtein edit distance over `char`s (two-row dynamic program).
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut cur: Vec<usize> = vec![0; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != cb);
            let delete = prev[j + 1] + 1;
            let insert = cur[j] + 1;
            cur[j + 1] = substitute.min(delete).min(insert);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::{
        ACTION_NAMES, HOOK_EVENTS, did_you_mean, hook_action_is_executable, hook_context_keys,
        levenshtein,
    };
    use crate::Action;

    #[test]
    fn suggestions_pick_the_closest_plausible_candidate() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        for (input, candidates, want) in [
            ("kill-pain", ACTION_NAMES, Some("kill-pane")),
            ("detach ", ACTION_NAMES, Some("detach")),
            ("kill-pane", ACTION_NAMES, Some("kill-pane")),
            ("pane-exited", HOOK_EVENTS, Some("pane-exit")),
            ("focus-change", HOOK_EVENTS, Some("focus-changed")),
            ("frobnicate", ACTION_NAMES, None),
            ("x", ACTION_NAMES, None),
            ("on-startup", HOOK_EVENTS, None),
            ("ab", &["abc", "abd"], Some("abc")),
        ] {
            assert_eq!(did_you_mean(input, candidates), want, "{input}");
        }
    }

    #[test]
    fn every_hook_event_has_sorted_context_keys_and_unknowns_have_none() {
        for &event in HOOK_EVENTS {
            let keys = hook_context_keys(event).expect("known event");
            assert!(!keys.is_empty() && keys.is_sorted(), "{event}: {keys:?}");
        }
        assert_eq!(hook_context_keys("pane-exited"), None);
    }

    #[allow(clippy::expect_used, reason = "tests")]
    fn action(toml_inline: &str) -> Action {
        #[derive(serde::Deserialize)]
        struct Holder {
            action: Action,
        }
        let holder: Holder = toml::from_str(toml_inline).expect("valid action");
        holder.action
    }

    #[test]
    fn only_run_with_a_usable_command_is_executable() {
        for executable in [
            "action = { kind = \"run\", command = \"echo hi\" }",
            "action = { kind = \"run\", command = [\"say\", \"done\"] }",
        ] {
            assert!(
                hook_action_is_executable(&action(executable)),
                "{executable}"
            );
        }
        for dead in [
            "action = \"noop\"",
            "action = \"kill-pane\"",
            "action = { kind = \"message\", text = \"hi\" }",
            "action = { kind = \"run\" }",
            "action = { kind = \"run\", command = \"   \" }",
            "action = { kind = \"run\", command = [] }",
            "action = { kind = \"run\", command = 3 }",
        ] {
            assert!(!hook_action_is_executable(&action(dead)), "{dead}");
        }
    }
}
