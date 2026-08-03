//! The generated hooks reference: every event the server fires, with its
//! context keys, the environment projection, and per-event prose.
//!
//! Names and context keys come from `phux_config::vocab` (the validators'
//! single source of truth); the per-event prose and the `PHUX_*`
//! projection come from `phux_server::hooks` ([`hook_event_specs`],
//! [`context_env_var`] — the same function the dispatcher injects the
//! child environment with). The
//! `spec_table_roundtrips_through_the_constructors` test in phux-server
//! pins the spec table to the real `HookEvent` constructors, and the
//! freshness test in `super::tests` then forces this page's regeneration
//! on any drift.

use phux_server::hooks::{context_env_var, hook_event_specs};

use super::Page;

/// Render `docs/reference/hooks.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "A hook runs an action when the server observes a named event. \
         Config hooks are TOML arrays-of-tables — one `[[hooks.<event>]]` \
         entry per `{ when, action }` pair:\n\n\
         ```toml\n\
         [[hooks.pane-exit]]\n\
         when   = { exit-code = 0 }\n\
         action = \"noop\"\n\n\
         [[hooks.pane-exit]]\n\
         when   = { exit-code = \"*\" }\n\
         action = { kind = \"run\", command = \"say 'pane exited'\" }\n\
         ```\n\n\
         The matching rules are deliberately tiny. Every `when` clause \
         must hold (AND); `\"*\"` matches unconditionally, even when the \
         key is absent; a key ending in `-startswith` prefix-matches the \
         base context key; anything else is an exact string match \
         (non-string TOML scalars compare via their canonical rendering, \
         so `exit-code = 0` matches the context value `\"0\"`). **First \
         match wins** per event: a matching entry consumes the event \
         whether or not its action runs. Only a `run` action with a \
         usable `command` (a non-blank string, executed via `/bin/sh -c`, \
         or a non-empty argv array, executed directly) executes \
         server-side; `noop` is the deliberate match-and-do-nothing \
         sentinel, and other action kinds (e.g. `message`) are \
         client-side. `phux config check` flags unknown event names, \
         `when` keys outside an event's context, and actions that can \
         never execute.\n\n\
         The table lists every event the server fires. Context keys are \
         what `when` clauses can match; each key also rides into the hook \
         child as the environment variable shown beside it. Keys marked \
         with a trailing `?` may be absent on a given firing — a `when` \
         clause naming an absent key simply does not match (except \
         `\"*\"`).\n\n\
         | Event | Context key | Environment | Fires when |\n\
         |---|---|---|---|\n",
    );
    for spec in hook_event_specs() {
        let mut first = true;
        for &key in spec.context_keys {
            let (event_cell, doc_cell) = if first {
                (format!("`{}`", spec.name), spec.doc.replace('|', "\\|"))
            } else {
                (String::new(), String::new())
            };
            first = false;
            let optional = if key_is_optional(spec.name, key) {
                "?"
            } else {
                ""
            };
            let _ = writeln!(
                body,
                "| {event_cell} | `{key}`{optional} | `{}` | {doc_cell} |",
                context_env_var(key),
            );
        }
    }

    body.push_str(
        "\nEvery hook child additionally receives `PHUX_EVENT` (the event \
         name) and `PHUX_SOCKET` (the UDS path the firing server listens \
         on, so a bare `phux` invocation inside a hook script targets \
         that server). Plugin `[[events]]` hooks — every enabled plugin \
         hook whose `on` names the event fires; first-match-wins applies \
         to config entries only — also receive `PHUX_PLUGIN_ID`, \
         `PHUX_PLUGIN_EVENT_ID`, and `PHUX_PLUGIN_ROOT`, and run with the \
         plugin root as their working directory.\n\n\
         Execution is fire-and-forget and bounded: events queue through a \
         non-blocking bounded channel (a full queue drops the event), a \
         fixed number of hook children run concurrently, and each child \
         runs under a timeout with kill-on-drop. A slow hook never blocks \
         the server.\n\n\
         Semantics, examples, and the notification pattern live in \
         `docs/consumers/tui.md` section 9.\n",
    );

    Page {
        file: "hooks.md",
        title: "phux hooks reference",
        summary: "Every hook event the server fires, with its context \
                  keys, environment projection, and matching rules.",
        tldr: "Every `[[hooks.<event>]]` event the server fires, the \
               context keys a `when` clause can match, and the `PHUX_*` \
               environment each hook child receives. Rendered from the \
               same vocabulary `phux config check` validates against and \
               the dispatcher injects with, so an event is listed here \
               exactly when the server fires it.",
        body,
    }
}

/// Whether `key` can be absent on a firing of `event` — the `?` marker.
///
/// Mirrors the constructors' `Option` parameters (`phux_server::hooks`):
/// `exit-code` is absent for a signal-killed child, `session` when none
/// applies, `agent-name` for an anonymous agent, `from` on a first
/// sighting. Pinned by `optional_markers_match_the_constructors` below,
/// which drives each constructor's optional parameter to `None`.
fn key_is_optional(event: &str, key: &str) -> bool {
    matches!(
        (event, key),
        ("after-new-pane" | "client-detached", "session")
            | ("pane-exit", "exit-code")
            | ("agent-state-changed", "agent-name" | "from")
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use phux_config::vocab::HOOK_EVENTS;

    use super::{key_is_optional, page};

    /// The page carries a first-column row for every canonical event
    /// name and one row per context key overall.
    #[test]
    fn hooks_page_has_a_row_per_event_and_per_context_key() {
        let page = page();
        for &event in HOOK_EVENTS {
            assert!(
                page.body.contains(&format!("| `{event}` |")),
                "generated hooks reference has no row for `{event}`"
            );
        }
        let rows = page
            .body
            .lines()
            .filter(|line| line.starts_with("| ") && !line.starts_with("| Event"))
            .count();
        let keys: usize = HOOK_EVENTS
            .iter()
            .map(|&event| phux_config::vocab::hook_context_keys(event).map_or(0, <[&str]>::len))
            .sum();
        assert_eq!(rows, keys, "one table row per (event, context key) pair");
    }

    /// Raw `|` inside doc prose would shear a table row; the renderer
    /// must keep every row at four columns.
    #[test]
    fn table_rows_keep_their_column_count() {
        let page = page();
        for line in page
            .body
            .lines()
            .filter(|line| line.starts_with("| ") && !line.starts_with("| Event"))
        {
            let columns = line.matches(" | ").count();
            assert_eq!(columns, 3, "row sheared by an unescaped pipe: {line}");
        }
    }

    /// The `?` markers agree with the constructors: driving every
    /// optional parameter to `None` yields exactly the keys NOT marked
    /// optional, so the page cannot promise a key a firing may omit
    /// (or hedge on one that is always present).
    #[test]
    fn optional_markers_match_the_constructors() {
        use phux_server::hooks::HookEvent;

        let terminal = phux_protocol::ids::TerminalId::local(7);
        let client = phux_server::state::ClientId(3);
        let minimal = [
            HookEvent::after_new_pane(&terminal, None),
            HookEvent::pane_exit(&terminal, None),
            HookEvent::focus_changed(&terminal, client),
            HookEvent::client_attached(client, "work"),
            HookEvent::client_detached(client, None),
            HookEvent::agent_state_changed(&terminal, "claude", "", None, "idle"),
        ];
        assert_eq!(
            minimal.len(),
            HOOK_EVENTS.len(),
            "an event is missing from this agreement test"
        );
        for event in minimal {
            let always_present: BTreeSet<&str> = event.context.keys().map(String::as_str).collect();
            let documented = phux_config::vocab::hook_context_keys(&event.name)
                .unwrap_or_else(|| panic!("unknown event `{}`", event.name));
            for &key in documented {
                assert_eq!(
                    key_is_optional(&event.name, key),
                    !always_present.contains(key),
                    "optional marker wrong for `{}`.`{key}`",
                    event.name
                );
            }
        }
    }
}
