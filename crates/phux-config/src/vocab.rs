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
//! [`did_you_mean`] is a small in-crate Levenshtein suggester so
//! diagnostics can turn "unknown action `kill-pain`" into "did you mean
//! `kill-pane`?" without a new dependency.

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
    use super::{ACTION_NAMES, HOOK_EVENTS, did_you_mean, levenshtein};

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
