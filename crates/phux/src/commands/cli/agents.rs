//! The `Agents` verbs.

use crate::commands::{JsonOpt, SignalArg, SpawnSplit, agent, coord, mission::MissionAction};
use usage::Args;

/// Start an agent integration in a new pane
///
/// Resolves INTEGRATION (a `phux launch --list` id) to its `[launch]`
/// command from an enabled plugin's integration template, then creates a
/// pane running it. The integration also gives the pane its agent name
/// and kind automatically, with no alias or per-shell config.
///
/// `--print` resolves and prints the argv without spawning (a server-free
/// dry run). Extra agent arguments follow `--`:
/// `phux launch codex -- --model o3`.
#[derive(Debug, Args)]
pub(crate) struct LaunchArgs {
    /// Integration id to launch (from `phux launch --list`).
    #[usage(value_name = "INTEGRATION", required_unless("--list"))]
    pub(crate) integration: Option<String>,

    /// List launchable integrations from enabled plugins and exit.
    #[usage(long)]
    pub(crate) list: bool,

    /// Resolve and print the launch argv (and cwd) without spawning a
    /// pane — a server-free dry run.
    #[usage(long, alias = "dry-run")]
    pub(crate) print: bool,

    /// Use the integration's declared ACP agent, never terminal input.
    #[usage(long)]
    pub(crate) acp: bool,

    /// Existing session that owns a native agent and its guardian.
    #[usage(long, value_name = "SESSION", requires("--acp"))]
    pub(crate) session: Option<String>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    /// Existing local pane beside which to place the launched pane.
    #[usage(long, value_name = "TARGET", conflicts("--list", "--print", "--acp"))]
    pub(crate) target: Option<String>,

    /// Split axis for explicit placement (requires `--target`).
    #[usage(long, value_enum, default = "horizontal", requires("--target"))]
    pub(crate) split: SpawnSplit,

    /// Fraction of the split retained by TARGET (requires `--target`).
    #[usage(
        long,
        default = "0.5",
        default_value_t = 0.5,
        requires("--target"),
        validate = "float(value) > 0 && float(value) < 1",
        validate_error = "ratio must be finite and strictly between 0 and 1"
    )]
    pub(crate) ratio: f32,

    /// Named projection to place into instead of the shared default
    /// (requires `--target`). Must be
    /// `<prefix>.layout/v1/<session-id>` for TARGET's session.
    #[usage(long, value_name = "KEY", requires("--target"))]
    pub(crate) projection: Option<String>,

    /// Working directory for a `working_directory = "workspace"`
    /// template. Defaults to the current directory.
    #[usage(short = 'c', long = "cwd", value_name = "DIR")]
    pub(crate) cwd: Option<std::path::PathBuf>,

    /// Extra arguments appended to the agent command, after `--`.
    #[usage(trailing_var_arg)]
    pub(crate) extra: Vec<String>,
}

/// Take exclusive input control of a pane
///
/// Seizes exclusive input authority over the resolved pane: while held,
/// only this connection's input reaches the PTY — every other client's
/// keystrokes (and any agent's `send-keys`) are locked out. Use it to
/// grab control of a pane an agent is driving. Release with `phux give`.
/// TARGET is a selector (see the top-level help).
#[derive(Debug, Args)]
pub(crate) struct TakeArgs {
    /// Target selector (resolves to one pane).
    pub(crate) target: String,

    /// Auto-release after this many seconds — the server, not this
    /// process, enforces it, so it survives this command exiting. Omit
    /// to hold the lease until `phux give` or a disconnect, today's
    /// default. The wire's `ttl_ms` is a `u32`, so this caps at
    /// 4294967 (about 49.7 days).
    #[usage(
        long,
        value_name = "SECS",
        validate = "int(value) <= 4294967",
        validate_error = "must be at most 4294967 seconds (ttl_ms is a u32)"
    )]
    pub(crate) ttl: Option<u32>,
}

/// Give back input control taken with `take`
///
/// Releases the input lease taken with `phux take`, returning the pane to
/// open input. A no-op if you do not hold the lease. TARGET is a selector.
#[derive(Debug, Args)]
pub(crate) struct GiveArgs {
    /// Target selector (resolves to one pane).
    pub(crate) target: String,
}

/// Signal a pane's process group.
#[derive(Debug, Args)]
#[usage(
    about = "Send a signal to a pane's process group",
    long_about = "Signal a pane's process group.\n\n\
            Delivers a POSIX signal to the program running in the resolved pane and \
            every subprocess it spawned — distinct from `phux kill`, which destroys \
            the pane. `freeze` (SIGSTOP) pauses the process mid-step; `resume` \
            (SIGCONT) lets it run again — the reversible brake for an agent about to \
            do something rash. TARGET is a selector.\n\n\
            Examples:\n  \
            phux signal build freeze\n  \
            phux signal . kill"
)]
pub(crate) struct SignalArgs {
    /// Target selector (resolves to one pane).
    pub(crate) target: String,

    /// Which signal to deliver.
    #[usage(value_enum)]
    pub(crate) signal: SignalArg,

    /// Make the signal safe to retry: a repeat with the same key (32 hex
    /// digits) answers the first signal's result and delivers nothing.
    /// The server must advertise `keyed_signal`.
    #[usage(long = "idempotency-key", value_name = "HEX32")]
    pub(crate) idempotency_key: Option<String>,
    /// Deliver `interrupt`, `terminate`, or `kill` without asking.
    ///
    /// Without it those three ask on a terminal, and when stdin is not
    /// one they refuse with exit 2 having sent nothing. `freeze` and
    /// `resume`, the reversible brake, never ask.
    #[usage(long)]
    pub(crate) yes: bool,
}

/// Track tasks in the local work store
///
/// `list`, `show`, `edit`, `archive`, `restore`, and `keeper` call the
/// work store's keeper, which `phux mission` starts on demand. The keeper
/// watches the phux server and projects each bound agent's state into
/// the task. `start` and `add --start` dial the phux server to create
/// the worktree session and launch the agent. With no subcommand, prints
/// this help.
///
/// `--json` prints a document whose `schema` is `phux.mission/v1`.
/// Exit status: 0 success; 1 the task is missing, the revision is stale,
/// the operation id was reused for a different write, the task cannot be
/// started, or the store failed; 2 the command line was refused, or a
/// failed start left its worktree because stdin is not a terminal.
#[derive(Debug, Args)]
pub(crate) struct MissionArgs {
    #[usage(subcommand)]
    pub(crate) action: Option<MissionAction>,
}

/// Send, read and acknowledge mail between agents
///
/// Mail lives in this host's work store beside `phux mission` tasks and
/// survives restarts. An agent is addressed by its pane here, `@749`,
/// optionally labelled, `pi@749`; anything outside a pane uses a name.
/// `send` stores the message and nothing more: nothing is typed into a
/// recipient's terminal. The recipient's harness integration takes it
/// (`wait`, `hook-context`) and puts it in the agent's conversation; an
/// agent idle under a hook-only harness sees it at its next hook. You
/// speak as `--as`, else `$PHUX_AGENT_ADDRESS`, else your pane, else
/// `$USER`.
///
/// `--json` prints a document whose `schema` is `phux.coord/v1`. Exit
/// status: 0 success; 1 the message is unknown or the store failed; 2
/// the command line was refused.
#[derive(Debug, Args)]
pub(crate) struct MailArgs {
    #[usage(subcommand)]
    pub(crate) action: coord::MailAction,
}

/// Claim paths so other agents leave them alone
///
/// A claim covers its path and everything under it, lives in the work
/// store, and lapses at its deadline. Every atomic acquisition receives
/// one fence. Taking again never adopts a live lease, even from the same
/// pane; `renew FENCE` and `release FENCE` require its exact receipt.
/// Exclusive acquisitions conflict on overlapping paths; shared holders
/// can coexist. Process exit does not free a lease. Claims are advisory:
/// they tell agents who is working where, and nothing stops a write.
///
/// `--json` prints a document whose `schema` is `phux.coord/v1`. Exit
/// status: 0 success; 1 a live acquisition conflicts, and nothing was
/// claimed, or the store failed; 2 the command line was refused.
#[derive(Debug, Args)]
pub(crate) struct ClaimArgs {
    #[usage(subcommand)]
    pub(crate) action: coord::ClaimAction,
}

/// List actions held for approval
///
/// A workload whose grant holds `?signal` has its kills, signals, and
/// forced detaches held by the server until someone holding un-held
/// `signal` on the same subject approves or denies them. This lists what
/// is waiting: the id, who asked, the held method, its subjects, and the
/// time left before it expires.
#[derive(Debug, Args)]
pub(crate) struct ApprovalsArgs {
    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Approve a held action
///
/// Releases the held action ID once: the server runs it as the workload
/// that asked, under that workload's own grant, and answers that
/// workload. Asks on a terminal; pass `--yes` when stdin is not one.
#[derive(Debug, Args)]
pub(crate) struct ApproveArgs {
    /// The approval id `phux approvals` lists.
    pub(crate) id: String,

    /// Approve without asking.
    #[usage(long)]
    pub(crate) yes: bool,
}

/// Deny a held action
///
/// Refuses the held action ID: the workload that asked gets a
/// permission-denied answer and nothing runs.
#[derive(Debug, Args)]
pub(crate) struct DenyArgs {
    /// The approval id `phux approvals` lists.
    pub(crate) id: String,
}

/// Report that an agent in a pane is waiting on a human answer.
#[derive(Debug, Args)]
#[usage(
    about = "Report that an agent is waiting on a human",
    long_about = "Report that an agent in a pane is waiting on a human answer.\n\n\
            This is the opt-in hook contract for configured integrations: it emits \
            the same `asked` event as the `phux-ask` title sentinel without writing \
            escape sequences into the target terminal. TARGET is resolved \
            client-side and the command neither attaches nor resizes the pane.\n\n\
            Examples:\n  \
            phux ask work:1.0 --id deploy --suggest Yes --suggest No \"Deploy?\"\n  \
            phux ask @3 --json \"Need approval\""
)]
pub(crate) struct AskArgs {
    /// Target selector: session, session:group, session:group.pane,
    /// @id, or `.` (focused). `=` is unsupported by headless commands.
    pub(crate) target: String,

    /// Stable question id for answer correlation.
    #[usage(long, default = "")]
    pub(crate) id: String,

    /// Suggested answer. Repeat to preserve display order.
    #[usage(long = "suggest", value_name = "TEXT")]
    pub(crate) suggestions: Vec<String>,

    /// Seconds the agent has already been waiting.
    #[usage(long, value_name = "SECS")]
    pub(crate) elapsed_seconds: Option<u64>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    /// Human-facing question text.
    pub(crate) question: String,
}

/// See and drive the agents running in panes
///
/// Inference (`list`/`show`/`explain`) reports the agent phux infers is
/// running in each pane. `set`/`clear` write and delete an explicit
/// per-pane agent identity that overrides inference.
#[derive(Debug, Args)]
pub(crate) struct AgentArgs {
    #[usage(subcommand)]
    pub(crate) action: agent::AgentAction,
}
