/// Shared `inputSchema` description for the `target` selector argument.
const TARGET_DESC: &str = "Target selector: session, session:group, \
    session:group.pane, @paneid, or `.` for the focused session. `=` is \
    unsupported because MCP has no attached-client focus history. Omit for \
    the focused session.";

/// The MCP tool catalog `tools/list` returns: name, description, and
/// JSON-Schema input and output shapes.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one flat JSON literal — the tool catalog; splitting it hurts readability"
)]
pub(crate) fn catalog() -> Value {
    let mut tools = json!([
        {
            "name": "phux_ls",
            "description": "List phux sessions on the running server (names, lifetimes, members, attached-client counts).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "socket": { "type": "string", "description": "Override the UDS path. Defaults to PHUX_SOCKET or the daemon default." }
                }
            }
        },
        {
            "name": "phux_snapshot",
            "description": "Capture a pane as structured screen data (side-effect-free; does not attach or resize).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "scrollback": { "type": "number", "description": "Include scrollback history. 0 = all retained history; N = the most-recent N rows. Omit for the viewport only." },
                    "cells": { "type": "boolean", "description": "When true, include per-cell OSC-133 marks and styles. Default false." },
                    "format": { "type": "string", "enum": ["html", "vt"], "description": "Render through the server's libghostty-vt Formatter instead of the lines/cells projection: 'html' for inline-styled markup, 'vt' for re-playable VT escape sequences (base64 in the `rendered.data` field). Populates the reply's `rendered` field. Cannot be combined with `cells`." },
                    "tail": { "type": "number", "description": "Return only the last N rendered rows (history, then viewport). 0 = all, capped at 10000. The viewport is a floor — a grid is never returned in part — so a window narrower than the viewport returns more rows than asked, never fewer, and `truncated` reports what was dropped. With `format`, this instead bounds how far back the rendered capture reaches (same request as `scrollback`)." },
                    "unwrap": { "type": "boolean", "description": "Join soft-wrapped rows into logical lines (rows as written, not as painted), so a match straddling a wrap is findable. Cannot be combined with `cells`: cell coordinates are grid coordinates and do not survive the join. With `format`, this instead asks the server's Formatter to join soft-wrapped rows in the rendered capture." },
                    "socket": { "type": "string" }
                }
            }
        },
        {
            "name": "phux_send_keys",
            "description": "Send input to a pane. Each key is a named key (Enter, Tab, C-c, ...) or a literal string, tmux-style.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "keys": { "type": "array", "items": { "type": "string" } },
                    "socket": { "type": "string" }
                },
                "required": ["target", "keys"]
            }
        },
        {
            "name": "phux_upload",
            "description": "Upload a file from the MCP server filesystem (not the pane cwd), without attaching, resizing, or submitting input. Returns the authoritative terminal-owning host path. A path does not imply provider media understanding. Refused/unknown outcomes are errors carrying the receipt; do not blindly retry.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "path": { "type": "string", "minLength": 1, "description": "Local path on the calling MCP server filesystem." },
                    "extension": { "type": "string", "pattern": "^[A-Za-z0-9]{1,16}$" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "description": "Overall resolution/read/upload deadline. Default 60 seconds." },
                    "socket": { "type": "string" }
                },
                "required": ["target", "path"],
                "additionalProperties": false
            }
        },
        {
            "name": "phux_paste",
            "description": "Paste text into a pane as one paste event. The server adds bracketed-paste markers when the pane has DEC mode 2004 on, so multiline text lands intact (no per-character auto-indent). A paste inserts without submitting; follow with phux_send_keys Enter to run it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "text": { "type": "string", "description": "The payload to paste, verbatim (newlines included)." },
                    "untrusted": { "type": "boolean", "description": "Mark the payload untrusted: the pane's untrusted-paste policy (reject by default) may silently drop an unsafe payload. Default false — the caller vouches for content it composed." },
                    "socket": { "type": "string" }
                },
                "required": ["target", "text"]
            }
        },
        {
            "name": "phux_run",
            "description": "Run a command in a pane and report its exit code, output, and duration. Assumes a POSIX shell, and refuses when a shell is not in the pane's foreground — against vim, less, or another agent the command line would land as keystrokes, not as a command.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "number", "minimum": 1, "maximum": 3600, "description": "Give up after this many seconds. Default 600; bounded to 1..=3600." },
                    "force": { "type": "boolean", "description": "Skip the available-shell precondition and type the command line whatever is running in the pane. Default false." },
                    "socket": { "type": "string" }
                },
                "required": ["target", "command"]
            }
        },
        {
            "name": "phux_wait",
            "description": "Poll a pane until it contains text (`until`) or settles (`idle_ms`). Returns whether the condition was met.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "until": { "type": "string", "description": "Succeed once any visible line contains this substring." },
                    "idle_ms": { "type": "number", "description": "Succeed once the screen holds still this long. Default when `until` is absent." },
                    "timeout_secs": { "type": "number", "description": "Give up after this many seconds. Default: wait forever." },
                    "socket": { "type": "string" }
                }
            }
        },
        {
            "name": "phux_new",
            "description": "Create a named session without attaching, returning its name and seed pane id through the canonical phux new --json surface. A named session is persistent: it stays, empty, after its last process exits, until phux_rm or phux_kill deletes it. Pass lifetime=ephemeral for one that ends with its last process.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Name for the new session. Required; a name already in use is rejected." },
                    "lifetime": { "type": "string", "enum": ["persistent", "ephemeral"], "description": "Whether the session survives its last process (persistent, the default) or ends with it (ephemeral)." },
                    "command": { "type": "array", "items": { "type": "string" }, "description": "Initial command (argv) for the seed pane. Omit or pass an empty array to use the server's default shell." },
                    "cwd": { "type": "string", "description": "Working directory for the seed pane." },
                    "idempotency_key": { "type": "string", "minLength": 32, "maxLength": 32, "pattern": "^[0-9a-fA-F]{32}$", "description": "Make the create safe to retry: a repeat with the same 32-hex-digit key returns the first create's result instead of failing on the name. The server must advertise spawn_idempotency (phux_status features); otherwise the call is refused with unsupported_server." },
                    "socket": { "type": "string" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "phux_kill",
            "description": "Kill what a selector resolves to: a whole session, a group, a pane, or `#tag`. Killing a whole session ends every member, work added meanwhile included, and deletes the session in one server step. Requires confirm=true.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "confirm": { "type": "boolean", "const": true, "description": "Required explicit destructive-operation confirmation." },
                    "idempotency_key": { "type": "string", "minLength": 32, "maxLength": 32, "pattern": "^[0-9a-fA-F]{32}$", "description": "Make the kill safe to retry: a repeat with the same 32-hex-digit key answers the first kill's result and kills nothing. Refused with unsupported_server when the server does not advertise keyed_signal." },
                    "socket": { "type": "string" }
                },
                "required": ["target", "confirm"]
            }
        },
        {
            "name": "phux_detach",
            "description": "Force-detach every client attached to a session, or every client attached anywhere on the server if `session` is omitted, from outside any attach UI. This is the control-plane counterpart to the interactive `C-a d` self-detach, not a self-detach: it never attaches, and there is no live view of this MCP connection's own to end. Use it to reclaim a session that is attached (or wedged) elsewhere so it is free for the next attach. The session and its panes are unaffected — only the viewing clients are disconnected, each cleanly (a `DETACHED` frame, clean TUI exit). Returns `detached`, the number of clients actually disconnected; `0` means nobody was attached there, which is success, not a miss. Requires confirm=true: this forcibly ejects whatever human or agent is currently attached, without their say-so.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "session": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Session to detach clients from. Omit to detach every attached client on the server." },
                    "confirm": { "type": "boolean", "const": true, "description": "Required explicit confirmation: this forcibly disconnects whoever is attached." },
                    "socket": { "type": "string" }
                },
                "required": ["confirm"]
            }
        },
        {
            "name": "phux_watch",
            "description": "Collect server-pushed events (command_started/finished, title_changed, asked, bell, pane_spawned/closed, dirty, idle, cwd_changed, terminal_control, and the journal notices journal_gap/source_gap) plus agent_state changes. On a server with an event journal each event item also carries `seq`, `ts_ms`, and the `actor` that caused it. Omit target for every local Terminal in the fleet; the client enumerates and follows resource lifecycle events while installing one metadata subscription per Terminal. Bounded one-shot: returns after max_events or timeout_secs. An agent_state item reports one change to a pane's phux.agent/v1 record — name, kind, session, the new state, effective attention, and `from` when this call already saw a prior record; a present-and-null `state` is the tombstone (the record went away). Observing an agent reach a state here is not a completion gate — see phux_agent_wait.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Pane selector to watch. Omit to watch server-wide events and every local Terminal's agent_state changes." },
                    "max_events": { "type": "number", "description": "Return after collecting this many items, agent_state included. Omit for no count cap." },
                    "timeout_secs": { "type": "number", "description": "Return after this many seconds regardless of count. Strongly recommended — without it the call blocks until the server exits." },
                    "socket": { "type": "string" }
                }
            }
        },
        crate::ask_tool::schema(),
        crate::cli_tools::launch_schema(),
        crate::cli_tools::spawn_schema(),
        crate::cli_tools::signal_schema(),
        crate::cli_tools::tag_schema(),
        crate::cli_tools::rename_schema(),
        crate::cli_tools::insert_schema(),
        crate::cli_tools::move_schema(),
        crate::cli_tools::swap_schema(),
        crate::cli_tools::archive_schema(),
        crate::cli_tools::project_inspect_schema(),
        crate::plugin_tools::action_schema(),
        crate::plugin_tools::profile_schema(),
    ]);
    if let Value::Array(entries) = &mut tools {
        entries.extend(crate::org_tools::schemas());
        entries.extend(crate::agent_tools::schemas());
        entries.extend(crate::diagnostic_tools::schemas());
        entries.extend(crate::perf_tools::schemas());
        entries.extend(crate::coord_tools::schemas());
        entries.extend(crate::resource_tools::schemas());
        entries.extend(crate::approval_tools::schemas());
        entries.extend(crate::terminal_tools::schemas());
    }
    // Hints derived from the kind catalog, never hand-set (ADR-0125).
    if let Value::Array(entries) = &mut tools {
        entries.push(crate::contracts::operation_status_schema());
    }
    crate::annotations::annotate(&mut tools);
    crate::contracts::enhance(&mut tools);
    tools
}
