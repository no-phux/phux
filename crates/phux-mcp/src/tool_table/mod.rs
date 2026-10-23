//! The MCP tool table: one row per tool naming the CLI verb it mirrors (or
//! why it has none), how it executes, and what it can touch on the wire.
//!
//! Depends only on `phux-protocol` because it is compiled into three places:
//! this adapter (annotations, the CLI residue gate), the parity test, and
//! the `phux` binary's `docs/reference/parity.md` generator (via `#[path]`).

#![allow(
    dead_code,
    reason = "compiled into three crates via #[path]; each uses a different subset"
)]

mod cli_only;
mod hints;

#[allow(
    unused_imports,
    reason = "each of the three crates compiling this table reads a different subset"
)]
pub(crate) use {
    cli_only::CLI_ONLY,
    hints::{DESTRUCTIVE_SOURCE, Hints, UNMAPPED, hints, hints_for},
};

use phux_protocol::wire::frame::{
    APPROVAL_DECIDE_KEY_PREFIX, SESSION_CREATE_KEY, SESSION_NAME_KEY, WHOAMI_KEY,
};

/// Which CLI verb a tool mirrors, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The `phux` invocation path the tool mirrors (`ls`, `agent wait`).
    Cli(&'static str),
    /// No CLI verb: the tool exists only for automation, for this reason.
    AutomationOnly(&'static str),
}

/// How the adapter executes a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exec {
    /// In-process over `phux-client` (or another library): no subprocess,
    /// and the result is built by the function the CLI verb itself calls.
    InProcess,
    /// The residue: the adapter runs the canonical CLI with argv, for this
    /// reason. `CliAdapter` refuses any tool not marked this way.
    Cli(&'static str),
}

/// What one tool can touch.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Touches {
    /// Catalog methods, by wire name or metadata key.
    Wire(&'static [&'static str]),
    /// Runs a local program (a plugin action): arbitrary effects.
    LocalExec,
    /// Reads local configuration only.
    LocalRead,
    /// Writes this host's work store (mail, claims): no server state, no
    /// PTY, nothing ended.
    LocalWrite,
}

/// One tool.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row {
    /// The MCP tool name.
    pub(crate) name: &'static str,
    /// The CLI verb it mirrors.
    pub(crate) surface: Surface,
    /// How it executes.
    pub(crate) exec: Exec,
    /// What it can touch; the source of its annotations.
    pub(crate) touches: Touches,
}

/// The read methods every targeted tool resolves its selector with.
const RESOLVE: &str = "GET_STATE";

/// A write to an ordinary metadata key: tags, a layout, an agent record.
///
/// Named apart from `SET_METADATA` because that method's rows include the
/// config-reload doorbell, which needs `SIGNAL`; an ordinary write's hints
/// come from the row the classifier gives one ([`phux_protocol::kinds::frame_rule`]).
pub(crate) const METADATA_WRITE: &str = "SET_METADATA (ordinary key)";

/// A `GET_PERF` that resets nothing: the report read alone.
///
/// Named apart from `GET_PERF` because that method's rows include
/// `{ reset: true }`, which needs `BIND`; a plain read's hints come from the
/// row the classifier gives one ([`phux_protocol::kinds::command_rule`]).
pub(crate) const PERF_READ: &str = "GET_PERF (no reset)";

/// Why an agent read stays on the CLI: the detector projection.
const DETECTOR: &str = "the detector projection (manifest replay over the pane's screen, plus \
     `[[plugins]]` agent declarations) is built in the phux binary, not phux-client";

/// Why the `AgentSession` verbs stay on the CLI.
const AGENT_SESSION: &str = "the Terminal-to-AgentSession resolution and the result documents \
     are assembled in the CLI command (`commands/agent/resource_session.rs`)";

const fn cli(name: &'static str, verb: &'static str, touches: Touches) -> Row {
    Row {
        name,
        surface: Surface::Cli(verb),
        exec: Exec::InProcess,
        touches,
    }
}

const fn residue(
    name: &'static str,
    verb: &'static str,
    why: &'static str,
    touches: Touches,
) -> Row {
    Row {
        name,
        surface: Surface::Cli(verb),
        exec: Exec::Cli(why),
        touches,
    }
}

/// Every tool in the catalog.
pub(crate) const TOOLS: &[Row] = &[
    Row {
        name: "phux_operation_status",
        surface: Surface::AutomationOnly(
            "observe an adapter-scoped mutation without blocking on approval",
        ),
        exec: Exec::InProcess,
        touches: Touches::LocalRead,
    },
    cli("phux_ls", "ls", Touches::Wire(&[RESOLVE])),
    cli(
        "phux_upload",
        "upload",
        Touches::Wire(&[RESOLVE, "PUT_FILE"]),
    ),
    cli(
        "phux_snapshot",
        "snapshot",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    cli(
        "phux_send_keys",
        "send-keys",
        Touches::Wire(&[RESOLVE, "ROUTE_INPUT"]),
    ),
    cli(
        "phux_paste",
        "paste",
        Touches::Wire(&[RESOLVE, "ROUTE_INPUT"]),
    ),
    residue(
        "phux_run",
        "run",
        "the sentinel bracketing, the child's mirrored exit code, and the run deadline are the \
         CLI command's, and a failing command must still return its RunResult",
        // `GET_METADATA`: the available-shell precondition reads the
        // server-owned `phux.pane-occupant/v1` record before typing.
        Touches::Wire(&[RESOLVE, "GET_METADATA", "ROUTE_INPUT", "GET_SCREEN"]),
    ),
    cli("phux_wait", "wait", Touches::Wire(&[RESOLVE, "GET_SCREEN"])),
    residue(
        "phux_new",
        "new",
        "`phux new` auto-starts a local server when none is listening, which runs the phux \
         binary; `phux_client::session` has the create, but not that server lifecycle",
        Touches::Wire(&[
            RESOLVE,
            "SESSION_CREATE",
            SESSION_CREATE_KEY,
            METADATA_WRITE,
            "GET_METADATA",
        ]),
    ),
    cli(
        "phux_kill",
        "kill",
        Touches::Wire(&[
            RESOLVE,
            "KILL_RESOURCES",
            "KILL_RESOURCE",
            "GET_ORGANIZATION",
            "RESOURCE_TERMINATE",
            "RESOURCE_REMOVE",
            "SESSION_DELETE",
        ]),
    ),
    cli(
        "phux_rm",
        "rm",
        Touches::Wire(&[
            RESOLVE,
            "GET_ORGANIZATION",
            "RESOURCE_REMOVE",
            "SESSION_DELETE",
        ]),
    ),
    cli(
        "phux_relaunch",
        "relaunch",
        Touches::Wire(&[RESOLVE, "GET_ORGANIZATION", "RESOURCE_RELAUNCH"]),
    ),
    cli(
        "phux_organize",
        "organize",
        Touches::Wire(&[
            RESOLVE,
            "GET_METADATA",
            "GET_ORGANIZATION",
            "SESSION_CREATE",
            "RESOURCE_MOVE_MANY",
            "SESSION_RENAME",
        ]),
    ),
    cli("phux_detach", "detach", Touches::Wire(&["DETACH_CLIENTS"])),
    cli(
        "phux_watch",
        "watch",
        Touches::Wire(&[RESOLVE, "SUBSCRIBE_EVENTS", "SUBSCRIBE_METADATA"]),
    ),
    cli("phux_ask", "ask", Touches::Wire(&[RESOLVE, "REPORT_ASKED"])),
    residue(
        "phux_launch",
        "launch",
        "integration resolution (enabled plugins, manifests, argv templates, native-session \
         restore) lives in the CLI command",
        Touches::Wire(&[
            RESOLVE,
            "SPAWN_RESOURCE",
            METADATA_WRITE,
            "GET_METADATA",
            "KILL_RESOURCE",
        ]),
    ),
    cli(
        "phux_spawn",
        "spawn",
        Touches::Wire(&[
            RESOLVE,
            "SPAWN_RESOURCE",
            METADATA_WRITE,
            "GET_METADATA",
            "KILL_RESOURCE",
        ]),
    ),
    cli(
        "phux_signal",
        "signal",
        Touches::Wire(&[RESOLVE, "SIGNAL_TERMINAL"]),
    ),
    cli(
        "phux_tag",
        "tag",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    cli(
        "phux_rename",
        "rename",
        Touches::Wire(&[SESSION_NAME_KEY, "SESSION_RENAME"]),
    ),
    cli(
        "phux_insert_pane",
        "insert-pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    cli(
        "phux_move_pane",
        "mv",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE, "MOVE_RESOURCE"]),
    ),
    cli(
        "phux_swap_pane",
        "swap-pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    residue(
        "phux_archive",
        "archive",
        "the archive capture, its agent-session bridge, and the import live in the CLI command",
        Touches::Wire(&[
            RESOLVE,
            "GET_METADATA",
            METADATA_WRITE,
            "GET_ORGANIZATION",
            "SESSION_CREATE",
        ]),
    ),
    residue(
        "phux_project_inspect",
        "project inspect",
        "git worktree inspection runs `git` from the CLI command; phux-client has no git",
        Touches::LocalRead,
    ),
    cli("phux_plugin_action", "config run", Touches::LocalExec),
    Row {
        name: "phux_plugin_profile",
        surface: Surface::AutomationOnly(
            "lists the profiles configured plugin manifests declare; `phux plugin list` and \
             `phux config plugins` list the manifests, not their profiles",
        ),
        exec: Exec::InProcess,
        touches: Touches::LocalRead,
    },
    residue(
        "phux_agent_list",
        "agent list",
        DETECTOR,
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    residue(
        "phux_agent_show",
        "agent show",
        DETECTOR,
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    residue(
        "phux_agent_explain",
        "agent explain",
        DETECTOR,
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    cli(
        "phux_agent_set",
        "agent set",
        Touches::Wire(&[RESOLVE, METADATA_WRITE]),
    ),
    cli(
        "phux_agent_clear",
        "agent clear",
        Touches::Wire(&[RESOLVE, "DELETE_METADATA"]),
    ),
    residue(
        "phux_agent_wait",
        "agent wait",
        "the result document's `detection` field is the detector projection, which is built in \
         the phux binary, not phux-client",
        Touches::Wire(&[
            RESOLVE,
            "SUBSCRIBE_EVENTS",
            "SUBSCRIBE_METADATA",
            "GET_METADATA",
            "GET_SCREEN",
        ]),
    ),
    residue(
        "phux_agent_send_keys",
        "agent send-keys",
        "key-spec validation, the occupant check, operation-id minting, and the \
         error-code table live in the CLI command",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "APPLY_INPUT"]),
    ),
    residue(
        "phux_agent_prompt",
        "agent prompt",
        "operation-id minting, the fused wait's result document, and the error-code \
         table live in the CLI command",
        Touches::Wire(&[
            RESOLVE,
            "GET_METADATA",
            "APPLY_INPUT",
            "SUBSCRIBE_METADATA",
            "GET_SCREEN",
        ]),
    ),
    residue(
        "phux_agent_answer",
        "agent answer",
        "live-ask correlation and suggestion validation live in the CLI command",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "APPLY_INPUT"]),
    ),
    residue(
        "phux_agent_start",
        "agent start",
        "integration resolution and the detector readiness wait live in the CLI command",
        Touches::Wire(&[
            RESOLVE,
            "GET_METADATA",
            METADATA_WRITE,
            "APPLY_INPUT",
            "SUBSCRIBE_METADATA",
        ]),
    ),
    residue(
        "phux_agent_session_open",
        "agent session open",
        AGENT_SESSION,
        Touches::Wire(&[RESOLVE, "SPAWN_RESOURCE"]),
    ),
    residue(
        "phux_agent_session_close",
        "agent session close",
        AGENT_SESSION,
        Touches::Wire(&[RESOLVE, "KILL_RESOURCE"]),
    ),
    residue(
        "phux_agent_emit",
        "agent emit",
        AGENT_SESSION,
        Touches::Wire(&[RESOLVE, "APPEND_RESOURCE_OUTPUT"]),
    ),
    residue(
        "phux_agent_log",
        "agent log",
        AGENT_SESSION,
        Touches::Wire(&[RESOLVE, "ATTACH_RESOURCE", "DETACH_RESOURCE"]),
    ),
    cli("phux_mail_send", "mail send", Touches::LocalWrite),
    cli("phux_mail_read", "mail read", Touches::LocalWrite),
    cli("phux_mail_ack", "mail ack", Touches::LocalWrite),
    cli("phux_claim_take", "claim take", Touches::LocalWrite),
    cli("phux_claim_renew", "claim renew", Touches::LocalWrite),
    cli("phux_claim_release", "claim release", Touches::LocalWrite),
    cli("phux_claim_list", "claim list", Touches::LocalRead),
    residue(
        "phux_status",
        "status",
        "the status document (peer-credential pid, service state, log paths) is assembled by \
         the CLI command",
        Touches::Wire(&[RESOLVE]),
    ),
    residue(
        "phux_doctor",
        "doctor",
        "the health checks are the CLI's; a second copy could disagree with `phux doctor`",
        Touches::Wire(&[RESOLVE]),
    ),
    residue(
        "phux_whoami",
        "whoami",
        "the `whoami` feature check and the refusal document are the CLI command's",
        Touches::Wire(&[WHOAMI_KEY]),
    ),
    residue(
        "phux_perf",
        "perf",
        "the report document and the dev-mode desktop's socket discovery are the CLI command's",
        // `source: desktop` reads the desktop's dev socket, which is not a
        // catalog method, and sends the server nothing.
        Touches::Wire(&[PERF_READ]),
    ),
    residue(
        "phux_perf_trace",
        "perf trace",
        "the capture window, the trace file, and the merged desktop timeline are the CLI \
         command's",
        // `source: desktop` asks the desktop, which sends `PERF_CONTROL` itself.
        Touches::Wire(&["PERF_CONTROL"]),
    ),
    cli(
        "phux_resource_show",
        "resource show",
        Touches::Wire(&[RESOLVE, "GET_TERMINAL_STATE", "GET_METADATA"]),
    ),
    cli(
        "phux_resource_wait",
        "resource wait",
        Touches::Wire(&[RESOLVE, "SUBSCRIBE_EVENTS"]),
    ),
    cli(
        "phux_resource_methods",
        "resource methods",
        Touches::Wire(&[RESOLVE]),
    ),
    cli(
        "phux_approvals",
        "approvals",
        Touches::Wire(&["LIST_METADATA", "GET_METADATA"]),
    ),
    cli(
        "phux_approve",
        "approve",
        Touches::Wire(&[APPROVAL_DECIDE_KEY_PREFIX, "GET_METADATA"]),
    ),
];

/// The row for `tool`.
pub(crate) fn row(tool: &str) -> Option<&'static Row> {
    TOOLS.iter().find(|row| row.name == tool)
}
