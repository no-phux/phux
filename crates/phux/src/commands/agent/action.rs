//! The `phux agent` grammar: [`AgentAction`], one variant per verb. Long
//! flag sets live in [`super::action_flags`] so each file stays readable.

use std::path::PathBuf;

use super::action_flags::{
    AnswerFlags, EmitFlags, LogFlags, NativeCommandFlags, PromptFlags, SendKeysFlags, SetFlags,
    StartFlags, UsageFlags,
};
use super::resource_session;

#[derive(Debug, usage::Subcommands)]
pub(crate) enum AgentAction {
    /// Internal durable SDK guardian, bootstrapped by the native spawn transaction.
    #[usage(hide)]
    Host,
    /// Send a typed SDK intent to the exact `AgentSession` and activation.
    ///
    /// This never pastes, sends keys, takes another viewer's lease, or
    /// assumes that a queued dialog response was consumed.
    NativeCommand {
        /// `AgentSession` resource @N, not its parent Terminal.
        target: String,
        #[usage(flatten)]
        flags: NativeCommandFlags,
    },
    /// List every pane's detected or declared agent and current state.
    ///
    /// The third column is the pane's `%name` address, `-` when it has no
    /// agent record, or why its record name is listed but not
    /// `%`-addressable (a name outside `[a-z][a-z0-9_-]`, a per-kind constant
    /// like `claude`, or one several panes share). JSON carries `address`
    /// and `address_refusal`.
    #[usage(alias = "ls")]
    List {
        /// Emit machine-readable JSON instead of the table.
        #[usage(long)]
        json: bool,
    },
    /// Show inferred state for one pane.
    Show {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
        /// Emit machine-readable JSON instead of the table.
        #[usage(long)]
        json: bool,
    },
    /// Explain the evidence behind one pane's state.
    ///
    /// With `--file` this runs OFFLINE: it evaluates the compiled detection
    /// manifests against a captured screen and contacts no server at all.
    /// That is the mode for authoring and debugging a manifest, because it
    /// prints the text every region resolved to on that screen — a rule
    /// scoped to a region that comes back empty can never match, and nothing
    /// else makes that visible (the detector fails safe to `idle`, silently).
    ///
    /// The capture is `phux snapshot --json` output or a plain text screen,
    /// one viewport row per line; `-` reads stdin. A JSON capture carries the
    /// pane's OSC title; a text one does not, so pass `--title` to exercise
    /// title-scoped rules against it (or to override the captured title).
    Explain {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane. Not used in offline (`--file`) mode.
        target: Option<String>,
        /// Emit machine-readable JSON instead of the table.
        #[usage(long)]
        json: bool,
        /// Evaluate a captured screen offline instead of querying the
        /// server. `-` reads stdin.
        #[usage(long, value_name = "PATH", conflicts("target"))]
        file: Option<PathBuf>,
        /// Agent kind whose manifest to evaluate, or one of its binary
        /// aliases. Required with `--file`: offline there is no foreground
        /// process group to identify the agent from.
        #[usage(long, value_name = "KIND", requires("--file"))]
        kind: Option<String>,
        /// OSC 0/2 title to evaluate `title`-scoped rules against. Overrides
        /// a JSON capture's own title; defaults to it, else to empty.
        #[usage(long, value_name = "TEXT", requires("--file"))]
        title: Option<String>,
        /// How to read `--file`. `auto` picks JSON when the first
        /// non-whitespace byte is `{`.
        #[usage(long, choices("auto", "json", "text"), requires("--file"))]
        format: Option<String>,
    },
    /// Declare the agent identity and state associated with a pane.
    ///
    /// Writes the whole agent record (last writer wins), then reads
    /// it back. Exits 1 (`agent_record_unconfirmed`) when the read-back shows
    /// the write did not land or the server refused the read; prints the
    /// confirmed record on success.
    Set {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
        #[usage(flatten)]
        flags: SetFlags,
    },
    /// Report hook-sourced lifecycle evidence to the pane's detector.
    ReportState {
        /// Target selector (resolves to one pane).
        target: String,
        /// Lifecycle evidence from the integration hook.
        #[usage(choices("working", "blocked", "done"))]
        state: String,
    },
    /// Block until a pane's agent TRANSITIONS into a lifecycle state.
    ///
    /// Satisfied only by an observed transition, never by a level read of
    /// the current state. That distinction is the point of the verb: `idle`
    /// is normally the detector's fail-safe fallthrough. Claude's OSC 9;4
    /// remove signal can assert it positively, but a level still cannot prove
    /// that the transition happened after this wait's baseline. A gate that
    /// fired on a level would return success on a corpse, instantly, and on
    /// any pane with no manifest at all.
    ///
    /// The consequence is deliberate: a pane already resting in a target
    /// state when the wait begins times out (124) rather than succeeding.
    /// `phux agent show` is the level read; this verb reports transitions.
    ///
    /// Subscribes before reading the baseline, so no transition is lost in
    /// between, and re-reads on the `phux wait` cadence to recover an edge a
    /// dropped notification never delivered. A record that goes away
    /// mid-wait ends it as a departure (exit 1), which is not a completion.
    Wait {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        #[usage(conflicts("--any"))]
        target: Option<String>,
        /// Wait for the first matching transition from any local agent in
        /// the fleet instead of resolving one pane.
        #[usage(long)]
        any: bool,
        /// Lifecycle state to wait for; repeat to OR several. Defaults to
        /// `idle`, `blocked`, `done` — the three ways a turn ends.
        /// `unknown` is not spellable: it is departure, not a state.
        #[usage(
            long,
            value_name = "STATE",
            choices("idle", "working", "blocked", "done")
        )]
        until: Vec<String>,
        /// Give up after this many seconds and exit 124. Unbounded when
        /// omitted, matching `phux wait` — always pass one in a script.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Emit the machine-readable result document instead of a line.
        #[usage(long)]
        json: bool,
    },
    /// Hand an agent a turn's worth of work, with a delivery receipt.
    ///
    /// The prompt text and Enter ride ONE acknowledged, idempotent operation,
    /// so a caller that does not get an answer can ask again under the same
    /// operation id without risking a duplicate turn — the failure
    /// fire-and-forget input cannot avoid, because its only recovery is a
    /// resend. Enter is last, so a partial write can only drop the
    /// submission and leave unsubmitted text, never submit a truncated
    /// prompt.
    ///
    /// The acknowledged path is required, not preferred: an older server or
    /// a satellite target is refused rather than downgraded, because a
    /// success code that means "the bytes are in the kernel queue" on one
    /// host and "accepted, maybe dropped" on another is not branchable.
    ///
    /// An OK is a kernel-queue receipt, not a consumption receipt. If
    /// delivery comes back UNKNOWN, do not resend: read the pane.
    ///
    /// With `--wait` the same process holds one connection across the
    /// submit, so every state change it sees is strictly post-write, and the
    /// gate is satisfied only by an observed TRANSITION — never by a level
    /// read of the current state, which a crashed agent also reads as.
    ///
    /// Acknowledged input is admitted one operation per pane: prompting
    /// different panes in parallel is fine, but two concurrent prompts to the
    /// SAME pane collide, and the later one is refused as busy.
    Prompt {
        /// Target selector (resolves to one pane).
        target: String,
        /// The prompt text. Single-line: a raw newline is refused, because a
        /// pane that has not enabled bracketed paste turns each one into a
        /// separate submission and no client can observe that mode.
        text: String,
        #[usage(flatten)]
        flags: PromptFlags,
    },
    /// Send keys to a pane, but only if it still hosts the expected agent.
    ///
    /// The agent-addressed sibling of top-level `phux send-keys`, and it
    /// differs from it in exactly one way: it re-checks the pane's declared
    /// agent identity immediately before writing and refuses if the occupant
    /// changed. `phux send-keys` addresses a pane and deliberately
    /// checks no identity; use that one when a pane is what you mean.
    ///
    /// Every key is validated before any byte is written, so a typo in the
    /// third key cannot leave the first two delivered — and since the whole
    /// batch now rides ONE acknowledged operation, that all-or-nothing
    /// promise covers delivery as well as validation. A caller that loses
    /// the answer can ask again under the same operation id instead of
    /// guessing whether the keys landed. For prose you want an agent to act
    /// on, `phux agent prompt` is the verb.
    SendKeys {
        /// Target selector (resolves to one pane).
        target: String,
        /// Key specs: named keys (`Enter`, `C-c`, `M-x`, `Up`) or literal
        /// text. A literal run immediately before `Enter` is sent as one
        /// submission-safe paste.
        #[usage(required)]
        keys: Vec<String>,
        #[usage(flatten)]
        flags: SendKeysFlags,
    },
    /// Answer a pane's pending agent question by validated choice.
    ///
    /// The `asked` event carries the question AND the suggestions the asking
    /// agent itself published, so an orchestrator can reply with a string the
    /// agent named instead of a blind keystroke. That is the contract: the
    /// bytes phux types are always one of the agent's own published answers,
    /// unless you pass `--allow-unlisted`.
    ///
    /// `--id` is required, and the pane must still be asking that exact
    /// question. Answering one the agent already moved past would type into
    /// whatever is on screen now, which is the failure this verb exists to
    /// prevent — so a stale id, an unidentified ask, and a pane that is not
    /// asking at all are all refusals with nothing written.
    ///
    /// The answer rides one acknowledged, idempotent input batch: a trusted
    /// paste followed by Enter, written and confirmed as a single operation.
    Answer {
        /// Target selector (resolves to one pane).
        target: String,
        #[usage(flatten)]
        flags: AnswerFlags,
    },
    /// Start an agent INSIDE an existing shell pane, and return when it is
    /// ready for input.
    ///
    /// The layout-free sibling of `phux launch`: it creates, splits, and
    /// moves nothing. `launch` returns a Terminal id ("a pane now exists");
    /// this returns a readiness assertion about a pane that already existed,
    /// which is a different success statement and therefore a different verb.
    /// The launch resolver is shared — the same integration template, the
    /// same argv, the same provider-native session identity — only the
    /// delivery differs: the pane's child is a live shell, so the command is
    /// typed as one quoted line and submitted as one acknowledged
    /// `APPLY_INPUT` batch.
    ///
    /// Ready is the FIRST detector publication after submit, not
    /// `state == idle`. No shipped detection manifest asserts `idle`
    /// positively — it is the fail-safe fallthrough — so a gate built on it
    /// would report ready for a pane where nothing launched. `--json`
    /// therefore reports the provenance of the answer (which rule matched, or
    /// that none did) rather than an opaque word.
    ///
    /// A `--kind` with no detection manifest is refused up front: without one
    /// the readiness contract is unenforceable and the verb could only time
    /// out, after having typed into the pane. `phux launch` and `phux spawn`
    /// keep working for any agent whatsoever, because neither promises
    /// readiness.
    Start {
        /// Human-facing agent name to bind to the pane. Must match
        /// `^[a-z][a-z0-9_-]{0,31}$`.
        name: String,
        #[usage(flatten)]
        flags: StartFlags,
        /// Extra arguments appended to the integration's launch command.
        #[usage(trailing_var_arg, value_name = "ARGS")]
        args: Vec<String>,
    },
    /// Clear a pane's declared agent identity.
    ///
    /// Deletes the agent record, then reads it back. Exits 1
    /// (`agent_record_unconfirmed`) when a record is still there or the
    /// server refused the read.
    Clear {
        /// Target selector (resolves to one pane). Omit for the focused
        /// pane.
        target: Option<String>,
        /// Print the confirmed `{schema_version, action, terminal, record}`
        /// document; failures are one JSON error line on stderr.
        #[usage(long)]
        json: bool,
    },
    /// Open or close a pane's agent session (an `AgentSession` resource).
    ///
    /// An agent session is the server-side, producer-fed event log an agent
    /// harness appends to with `phux agent emit` and anyone reads with
    /// `phux agent log`. It is bound to the pane the agent runs in: closing
    /// the pane closes it, closing it never touches the pane. Needs a server
    /// that advertises `RESOURCE_KINDS` (`phux status --json`).
    Session {
        #[usage(subcommand)]
        action: resource_session::SessionAction,
    },
    /// Append one record to a pane's agent session.
    ///
    /// The record is one `AgentEventsJsonlV1` line: `--type` from the closed
    /// set (`session_start`, `prompt`, `tool_start`, `tool_end`,
    /// `notification`, `ask`, `stop`, `session_end`, `state`, `provider_raw`)
    /// and `--data`, a JSON object. The server stamps `seq` and `ts_ms` and
    /// derives the agent's lifecycle state from the stream (`prompt` /
    /// `tool_start` -> working, `ask` -> blocked, `stop` -> done,
    /// `session_end` -> retract). Any local connection (this machine's
    /// socket or localhost) may append, not only the one that opened the
    /// session; a remote peer is refused with `not_producer`. Nothing is
    /// written on a refusal. Prints nothing on success.
    Emit {
        /// The session: its resource id, the pane hosting it, or `%name`.
        target: String,
        #[usage(flatten)]
        flags: EmitFlags,
    },
    /// Read a pane's agent session stream.
    ///
    /// Attaches to the session as an observer, the way `phux rec` attaches
    /// to a pane: the retained records arrive first and, with `--follow`,
    /// live records after them until Ctrl-C or the session closes. There is
    /// no `--timeout`; run a following log under a child-process deadline
    /// exactly as you would `phux watch`.
    Log {
        /// The session: its resource id, the pane hosting it, or `%name`.
        target: String,
        #[usage(flatten)]
        flags: LogFlags,
    },
    /// Make plain `claude` launch inside phux and declare its identity.
    InstallClaude {
        /// Shell rc file to activate (auto-detected from SHELL).
        #[usage(long, choices("zsh", "bash", "fish"))]
        shell: Option<String>,
        /// Absolute path to the real Claude executable (auto-detected from PATH).
        #[usage(long, value_name = "PATH")]
        real: Option<PathBuf>,
    },
    /// Remove the claude-in-phux shim and shell activation.
    UninstallClaude,
    /// Show the AI plan limits this host can see.
    ///
    /// Reads each signed-in subscription's limit windows from the vendor's
    /// own credentials: Claude Code (its OAuth usage endpoint), Codex (the
    /// rate limits its session ledgers record, or the live chatgpt.com usage
    /// endpoint when they are old) and Grok Build (its credits allowance). Prints
    /// utilization and reset times per plan. Credentials are only read,
    /// never refreshed, and go only to the vendor that issued them;
    /// requests are spaced at least two minutes apart per vendor and back
    /// off on rate limits, with the last reading reused in between.
    ///
    /// `--publish` also writes the result to the server's `phux.usage/v1`
    /// Global metadata key, which every client allowed `observe@global` can
    /// read and watch; the usage plugin runs this from its hooks. A publish
    /// does nothing when another is running or the last ran under ten
    /// seconds ago (unless `--refresh`), so bursts of hooks stay cheap. A
    /// client may set `phux.usage.refresh/v1`; the next hook-driven publish
    /// then skips the two-minute spacing. Does not start a server.
    Usage {
        #[usage(flatten)]
        flags: UsageFlags,
    },
    /// Internal: read one Claude Code hook payload from stdin and print the
    /// fields the generated shim splices into its `phux` calls, as one line
    /// of shell-safe tokens. Hidden because it is the wrapper's JSON reader,
    /// not a promise that phux ships a JSON tool.
    #[usage(name = "hook-payload", hide)]
    HookPayload,
    /// Internal: read one Claude Code hook payload from stdin and print the
    /// `provider_raw` data of its `phux.transcript/v1` entry, or nothing.
    /// Hidden because only the generated shim and the Claude plugin call it.
    #[usage(name = "hook-transcript", hide)]
    HookTranscript,
}
