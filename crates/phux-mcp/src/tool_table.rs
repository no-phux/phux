//! The MCP tool table: one row per tool naming the CLI verb it mirrors (or
//! why it has none), how it executes, and what it can touch on the wire.
//!
//! This file is the single source for three consumers and depends only on
//! `phux-protocol`, so each can compile it without linking the others:
//!
//! - `crate::annotations` derives every tool's `readOnlyHint` and
//!   `destructiveHint` from [`Row::touches`] (ADR-0125);
//! - `crate::cli_adapter` refuses to spawn the CLI for a tool whose
//!   [`Row::exec`] is not [`Exec::Cli`] — the residue, each with its reason;
//! - the CLI/MCP parity gate (`crates/phux-mcp/tests/parity.rs`) and the
//!   generated `docs/reference/parity.md` (`crates/phux/src/refdocs/parity.rs`)
//!   include it with `#[path]` and check it against the live tool catalog
//!   and the CLI grammar.
//!
//! A row exists for every catalog tool; the gate fails on a missing or stale
//! one.

#![allow(
    dead_code,
    reason = "compiled into three crates via #[path]; each uses a different subset"
)]

use phux_protocol::ids::ResourceId;
use phux_protocol::kinds::{self, Verb, Verbs};
use phux_protocol::wire::frame::{
    APPROVAL_DECIDE_KEY_PREFIX, FrameKind, RESOURCE_TAGS_KEY, SESSION_CREATE_KEY, SESSION_NAME_KEY,
    Scope, WHOAMI_KEY,
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
/// come from the row the classifier gives one ([`kinds::frame_rule`]).
pub(crate) const METADATA_WRITE: &str = "SET_METADATA (ordinary key)";

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
    cli("phux_ls", "ls", Touches::Wire(&[RESOLVE])),
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
        Touches::Wire(&[RESOLVE, SESSION_CREATE_KEY, METADATA_WRITE, "GET_METADATA"]),
    ),
    cli(
        "phux_kill",
        "kill",
        Touches::Wire(&[RESOLVE, "KILL_RESOURCES", "KILL_RESOURCE"]),
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
    cli("phux_rename", "rename", Touches::Wire(&[SESSION_NAME_KEY])),
    cli(
        "phux_insert_pane",
        "insert-pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    cli(
        "phux_move_pane",
        "move-pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE, "MOVE_RESOURCE"]),
    ),
    cli(
        "phux_swap_pane",
        "swap-pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    residue(
        "phux_workspace",
        "workspace",
        "git worktree inspection and the session archive save/restore live in the CLI command",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE, SESSION_CREATE_KEY]),
    ),
    cli("phux_plugin_action", "config run", Touches::LocalExec),
    Row {
        name: "phux_plugin_workspace",
        surface: Surface::AutomationOnly(
            "lists the workspace profiles configured plugin manifests declare; `phux plugin \
             list` and `phux config plugins` list the manifests, not their workspace profiles",
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
        "key-spec validation, the occupant check, operation-id minting, and the ADR-0076 \
         error-code table live in the CLI command",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "APPLY_INPUT"]),
    ),
    residue(
        "phux_agent_prompt",
        "agent prompt",
        "operation-id minting, the fused wait's result document, and the ADR-0076 error-code \
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

/// CLI verbs in `docs/consumers/agents.md`'s JSON index (its headings and
/// body) that have no MCP tool, each with its reason. The parity gate
/// requires every agent-facing verb there to have a tool or a row here.
pub(crate) const CLI_ONLY: &[(&str, &str)] = &[
    (
        "config agents",
        "local config inventory of declared agent integrations; `phux_agent_list` covers the live agents",
    ),
    (
        "host ls",
        "operator inventory of the host registry, not an agent action",
    ),
    (
        "pair",
        "mints a pairing secret; credential handling stays outside the model-facing set",
    ),
    ("mcp", "launches this adapter itself"),
    (
        "deny",
        "`phux_approve` decides both ways: `decision: deny` is `phux deny` (ADR-0128)",
    ),
    ("resize", "not exposed over MCP yet; a known parity gap"),
    (
        "rec",
        "not exposed over MCP yet; a recording is written to a file on the adapter's host",
    ),
    (
        "play",
        "not exposed over MCP yet; playback replays a file from the adapter's host",
    ),
];

/// The row for `tool`.
pub(crate) fn row(tool: &str) -> Option<&'static Row> {
    TOOLS.iter().find(|row| row.name == tool)
}

/// The two hints one tool carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hints {
    /// No method the tool can send changes server state.
    pub(crate) read_only: bool,
    /// Some method can end a process, eject a client, or type into a PTY.
    pub(crate) destructive: bool,
}

/// What an unmapped tool is annotated as: the most conservative claim.
pub(crate) const UNMAPPED: Hints = Hints {
    read_only: false,
    destructive: true,
};

/// Where the `destructive` column comes from.
pub(crate) const DESTRUCTIVE_SOURCE: &str = "the kind table's `MethodSpec.dangerous` flag \
     (ADR-0128), or a method that needs `INPUT`";

/// Whether a method is destructive: the kind table marks it `dangerous`
/// (it can end a process, eject a client, stop the server, or release a
/// held action; ADR-0128), or it types into a live PTY (`INPUT`). `CREATE`
/// and `BIND` add resources and rewrite bindings: mutating, not destructive.
const fn dangerous(verbs: Verbs, marked: bool) -> bool {
    marked || verbs.contains(Verb::Input)
}

/// The hints `touches` implies, or `None` when it names a method the
/// catalog does not have.
pub(crate) fn hints(touches: Touches) -> Option<Hints> {
    match touches {
        Touches::LocalExec => Some(UNMAPPED),
        Touches::LocalRead => Some(Hints {
            read_only: true,
            destructive: false,
        }),
        Touches::Wire(names) => wire_hints(names),
    }
}

fn wire_hints(names: &[&str]) -> Option<Hints> {
    let mut hints = Hints {
        read_only: true,
        destructive: false,
    };
    for name in names {
        let (verbs, mutating, marked) = method_facts(name)?;
        hints.read_only &= !mutating;
        hints.destructive |= dangerous(verbs, marked);
    }
    Some(hints)
}

/// The verbs `name` can need, whether it can change state
/// ([`kinds::MethodSpec::mutating`]'s conservative rule), and whether the
/// kind table marks it dangerous.
fn method_facts(name: &str) -> Option<(Verbs, bool, bool)> {
    if name == METADATA_WRITE {
        let verbs = kinds::frame_rule(&ordinary_metadata_write()).verb_set();
        // A row that admits no verb is a denial: conservatively a write.
        return Some((verbs, verbs.is_empty() || verbs.mutates(), false));
    }
    let method = kinds::method_named(name)?;
    Some((method.verbs(), method.mutating(), method.dangerous))
}

/// A representative ordinary write: a Terminal's tags.
fn ordinary_metadata_write() -> FrameKind {
    FrameKind::SetMetadata {
        request_id: 0,
        scope: Scope::Resource(ResourceId::local(1)),
        key: RESOURCE_TAGS_KEY.to_owned(),
        value: Vec::new(),
    }
}

/// The hints for `tool`, or `None` with no row or an unknown method.
pub(crate) fn hints_for(tool: &str) -> Option<Hints> {
    row(tool).and_then(|row| hints(row.touches))
}
