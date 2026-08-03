---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux hooks reference

**TL;DR.** Every `[[hooks.<event>]]` event the server fires, the context keys a `when` clause can match, and the `PHUX_*` environment each hook child receives. Rendered from the same vocabulary `phux config check` validates against and the dispatcher injects with, so an event is listed here exactly when the server fires it.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

A hook runs an action when the server observes a named event. Config hooks are TOML arrays-of-tables — one `[[hooks.<event>]]` entry per `{ when, action }` pair:

```toml
[[hooks.pane-exit]]
when   = { exit-code = 0 }
action = "noop"

[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "say 'pane exited'" }
```

The matching rules are deliberately tiny. Every `when` clause must hold (AND); `"*"` matches unconditionally, even when the key is absent; a key ending in `-startswith` prefix-matches the base context key; anything else is an exact string match (non-string TOML scalars compare via their canonical rendering, so `exit-code = 0` matches the context value `"0"`). **First match wins** per event: a matching entry consumes the event whether or not its action runs. Only a `run` action with a usable `command` (a non-blank string, executed via `/bin/sh -c`, or a non-empty argv array, executed directly) executes server-side; `noop` is the deliberate match-and-do-nothing sentinel, and other action kinds (e.g. `message`) are client-side. `phux config check` flags unknown event names, `when` keys outside an event's context, and actions that can never execute.

The table lists every event the server fires. Context keys are what `when` clauses can match; each key also rides into the hook child as the environment variable shown beside it. Keys marked with a trailing `?` may be absent on a given firing — a `when` clause naming an absent key simply does not match (except `"*"`).

| Event | Context key | Environment | Fires when |
|---|---|---|---|
| `after-new-pane` | `session`? | `PHUX_SESSION` | A pane's actor spawned: fires right after pane creation, before the inner process has produced output. |
|  | `terminal-id` | `PHUX_TERMINAL_ID` |  |
| `pane-exit` | `exit-code`? | `PHUX_EXIT_CODE` | A pane's inner process exited. `exit-code` is present only when the OS reported a code (absent for a signal-killed child). |
|  | `terminal-id` | `PHUX_TERMINAL_ID` |  |
| `focus-changed` | `client-id` | `PHUX_CLIENT_ID` | A client's focus landed on a pane. |
|  | `terminal-id` | `PHUX_TERMINAL_ID` |  |
| `client-attached` | `client-id` | `PHUX_CLIENT_ID` | A client's attach completed. |
|  | `session` | `PHUX_SESSION` |  |
| `client-detached` | `client-id` | `PHUX_CLIENT_ID` | An attached client detached for any reason (explicit detach or transport drop). `session` is absent if the session was reaped before the detach ran. |
|  | `session`? | `PHUX_SESSION` |  |
| `agent-state-changed` | `agent-kind` | `PHUX_AGENT_KIND` | The detector's published agent state for a pane actually changed (ADR-0046). `from` is absent on a first sighting; `agent-name` is absent for an anonymous agent; a withdrawn record arrives as `to = "unknown"`. |
|  | `agent-name`? | `PHUX_AGENT_NAME` |  |
|  | `from`? | `PHUX_FROM` |  |
|  | `terminal-id` | `PHUX_TERMINAL_ID` |  |
|  | `to` | `PHUX_TO` |  |

Every hook child additionally receives `PHUX_EVENT` (the event name) and `PHUX_SOCKET` (the UDS path the firing server listens on, so a bare `phux` invocation inside a hook script targets that server). Plugin `[[events]]` hooks — every enabled plugin hook whose `on` names the event fires; first-match-wins applies to config entries only — also receive `PHUX_PLUGIN_ID`, `PHUX_PLUGIN_EVENT_ID`, and `PHUX_PLUGIN_ROOT`, and run with the plugin root as their working directory.

Execution is fire-and-forget and bounded: events queue through a non-blocking bounded channel (a full queue drops the event), a fixed number of hook children run concurrently, and each child runs under a timeout with kill-on-drop. A slow hook never blocks the server.

Semantics, examples, and the notification pattern live in `docs/consumers/tui.md` section 9.
