//! Validation vocabulary: the canonical action-name and hook-event-name
//! lists, plus a did-you-mean helper (phux-i0e8.3.1).
//!
//! phux-config owns the vocabulary that `phux config check` (and the
//! runtime error paths) validate against. The string values here are the
//! single source of truth; the historical definition sites re-export them
//! so call sites did not churn:
//!
//! - [`ACTION_NAMES`] moved in from
//!   `phux-client::attach::input_dispatch`, which re-exports it. The
//!   dispatcher's `run_action` match arms and the command-palette
//!   registry are pinned to this list by unit tests in phux-client.
//! - The hook event consts ([`AFTER_NEW_PANE`] .. [`AGENT_STATE_CHANGED`])
//!   moved in from `phux-server::hooks`, which re-exports them.
//!   [`HOOK_EVENTS`] is the aggregate list for validators.
//!
//! [`hook_context_keys`] and [`hook_action_is_executable`]
//! (phux-i0e8.3.3) describe what a `[[hooks.<event>]]` entry can
//! legitimately reference: the context keys an event's `when` clauses
//! can match, and whether the action ever executes server-side. Both
//! are pinned to `phux-server::hooks` by agreement tests there.
//!
//! [`did_you_mean`] is a small in-crate Levenshtein suggester so
//! diagnostics can turn "unknown action `kill-pain`" into "did you mean
//! `kill-pane`?" without a new dependency.

use crate::Action;

/// Canonical names of every action the client dispatcher handles.
///
/// This is the single source of truth for the action set. The
/// dispatcher's `run_action` match arms and the command-palette registry
/// (both in phux-client) are checked against this list by unit tests so
/// the three cannot drift: adding a `run_action` arm without adding it
/// here (and to the registry) fails CI.
pub const ACTION_NAMES: &[&str] = &[
    "split-pane",
    "kill-pane",
    "new-window",
    "kill-window",
    "next-window",
    "previous-window",
    "select-window",
    "rename-window",
    "rename-session",
    "focus-direction",
    "resize-pane",
    "show-help",
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
    "new-session",
    "take-input",
    "give-input",
    "signal-terminal",
    "set-pane",
    "plugin-action",
    "plugin-pane",
    "reload-config",
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
///
/// A `[[hooks.<name>]]` table whose `<name>` is not in this list never
/// fires; validators use this list to flag it instead of failing open.
pub const HOOK_EVENTS: &[&str] = &[
    AFTER_NEW_PANE,
    PANE_EXIT,
    FOCUS_CHANGED,
    CLIENT_ATTACHED,
    CLIENT_DETACHED,
    AGENT_STATE_CHANGED,
];

/// The context keys the server can put on a hook event, sorted
/// ascending; `None` for an event name outside [`HOOK_EVENTS`].
///
/// Code-is-truth: the `HookEvent` constructors in `phux-server::hooks`
/// are the source, and an agreement test there builds each event with
/// every optional key present and asserts the key set matches this
/// table (the `docs/consumers/tui.md` §9 table is reconciled to the
/// same lists). Optional keys are included — `exit-code` is absent for
/// a signal-killed child, `agent-name` for an anonymous agent — since a
/// `when` clause may legitimately reference them; it simply never
/// matches an event where the key is absent.
///
/// A `when` key outside its event's list can never match anything (the
/// `-startswith` suffix strips off before the lookup), which is exactly
/// the silent-no-op trap validators use this function to flag.
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

/// Whether a `[[hooks.<event>]]` action can ever execute server-side.
///
/// Pure-predicate mirror of the dispatcher's action resolver
/// (`action_argv` in `phux-server::hooks`, pinned by an agreement test
/// there): only a parameterized `run` action with a usable `command` —
/// a non-blank string or a non-empty all-string array — ever spawns a
/// child. Everything else (bare names including the deliberate `noop`
/// sentinel, client-side kinds like `message`, `run` without a usable
/// `command`) matches, consumes the event under first-match-wins, and
/// runs nothing. Validators special-case `noop` themselves: it is
/// non-executable by design, not a mistake.
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

/// Maximum Levenshtein distance at which [`did_you_mean`] still offers a
/// suggestion. Distance 2 covers the common typo shapes (one wrong
/// letter plus one slip, a doubled letter, a trailing `-ed`) without
/// suggesting unrelated names for garbage input.
const MAX_SUGGESTION_DISTANCE: usize = 2;

/// Suggest the closest candidate to `input`, if any is close enough.
///
/// Returns the candidate with the smallest Levenshtein distance from
/// `input`, provided that distance is at most
/// `MAX_SUGGESTION_DISTANCE` (a private const, currently 2); ties go
/// to the earlier candidate. An
/// exact match returns that candidate (distance 0). Returns `None` when
/// nothing is close enough to be a plausible typo target.
///
/// Intended for diagnostics: `did_you_mean("kill-pain", ACTION_NAMES)`
/// yields `Some("kill-pane")`.
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

/// Levenshtein edit distance between `a` and `b` (unit-cost insert,
/// delete, substitute), computed over `char`s with the classic two-row
/// dynamic program.
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
    fn levenshtein_ground_truth() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("kill-pain", "kill-pane"), 2);
        assert_eq!(levenshtein("pane-exited", "pane-exit"), 2);
    }

    #[test]
    fn hit_suggests_the_closest_action_name() {
        // The umbrella bead's motivating typo: "kill-pain" parses,
        // validates, and silently does nothing today.
        assert_eq!(did_you_mean("kill-pain", ACTION_NAMES), Some("kill-pane"));
        assert_eq!(did_you_mean("detach ", ACTION_NAMES), Some("detach"));
    }

    #[test]
    fn hit_suggests_the_closest_hook_event() {
        assert_eq!(did_you_mean("pane-exited", HOOK_EVENTS), Some("pane-exit"));
        assert_eq!(
            did_you_mean("focus-change", HOOK_EVENTS),
            Some("focus-changed")
        );
    }

    #[test]
    fn miss_returns_none_for_distant_input() {
        assert_eq!(did_you_mean("frobnicate", ACTION_NAMES), None);
        assert_eq!(did_you_mean("x", ACTION_NAMES), None);
        assert_eq!(did_you_mean("on-startup", HOOK_EVENTS), None);
    }

    #[test]
    fn exact_match_returns_that_candidate() {
        assert_eq!(did_you_mean("kill-pane", ACTION_NAMES), Some("kill-pane"));
        assert_eq!(did_you_mean("pane-exit", HOOK_EVENTS), Some("pane-exit"));
    }

    #[test]
    fn tie_goes_to_the_earlier_candidate() {
        // Both candidates are at distance 1; the first listed wins.
        assert_eq!(did_you_mean("ab", &["abc", "abd"]), Some("abc"));
    }

    /// Every valid event has a context-key list (sorted, so validators
    /// and the doc table can rely on the order); anything else is
    /// `None` — never an empty "valid but keyless" list.
    #[test]
    fn every_hook_event_has_sorted_context_keys_and_unknowns_have_none() {
        for &event in HOOK_EVENTS {
            let keys = hook_context_keys(event)
                .unwrap_or_else(|| panic!("no context keys for valid event `{event}`"));
            assert!(!keys.is_empty(), "empty context for `{event}`");
            assert!(keys.is_sorted(), "unsorted context for `{event}`: {keys:?}");
        }
        assert_eq!(hook_context_keys("pane-exited"), None);
        assert_eq!(hook_context_keys(""), None);
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

    /// The predicate's positive space is exactly "run with a usable
    /// command"; the agreement test in phux-server pins it to the
    /// dispatcher's `action_argv`.
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

    #[test]
    fn hook_events_list_matches_the_consts() {
        assert_eq!(
            HOOK_EVENTS,
            &[
                "after-new-pane",
                "pane-exit",
                "focus-changed",
                "client-attached",
                "client-detached",
                "agent-state-changed",
            ]
        );
    }
}
