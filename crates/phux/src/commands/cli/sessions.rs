//! The `Sessions` verbs that list, create, and manage sessions.

use crate::commands::{EnvAssignment, JsonOpt, RemoteOpt};
use usage::Args;

/// List sessions
///
/// Queries the running server and prints one line per session: how many
/// terminals run, and only what needs you (an agent waiting, a failure,
/// work a restart lost). Sessions that need you come first; past twenty
/// lines the quiet ones collapse into a count. `SESSION` lists that
/// session's terminals, exceptions first. `-l` shows every session with
/// its keys, lifetime, and windows, and with `SESSION` each terminal's
/// key, revision, and run.
///
/// Does not start a server: with no server running it reports as much
/// and exits non-zero (like `tmux ls`). Pass `--json` for the stable,
/// versioned machine shape, always complete, instead of the human text.
/// `--all` lists every machine at once: this one and each registered
/// host, grouped by machine.
#[derive(Debug, Args)]
pub(crate) struct LsArgs {
    /// List this session's terminals.
    #[usage(conflicts("--json", "--all"))]
    pub(crate) session: Option<String>,

    /// Show keys, lifetimes, windows, and runs; never collapse.
    #[usage(short = 'l', long, conflicts("--json"))]
    pub(crate) detail: bool,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,

    /// List this machine and every registered host (`phux host ls`),
    /// grouped by machine. Hosts are queried at once with a short
    /// deadline; one that does not answer is listed as unreachable
    /// instead of failing the listing. With `--json` the document is
    /// `phux.hosts/v1`, the shape the TUI sidebar's hosts provider reads.
    #[usage(short = 'a', long, conflicts("--remote"))]
    pub(crate) all: bool,
}

/// Create a session and attach to it
///
/// Creates the named session if it does not already exist, then
/// attaches. Auto-starts a server if none is running. A name already
/// in use is an error; omit the name to take the configured
/// `session-name-template`, disambiguated with fresh `${random-name}`
/// picks when the template draws one, then a numeric suffix.
///
/// A session you name is persistent: it stays, empty, after its last
/// process exits, until `phux rm` or `phux kill` deletes it. A session
/// whose name was generated, like the one bare `phux` starts, is
/// ephemeral: it ends with its last process. `--ephemeral` makes a
/// named session ephemeral too.
///
/// With `--json`, creates the session *without* attaching and prints
/// the seed pane's id as JSON instead. This neither attaches nor
/// resizes, and the create is atomic server-side (no attach race).
/// `--json` requires an explicit `-s NAME`, and a name already in use
/// is an error (create-only, never create-or-attach).
// `--json` requires `-s`; a group carries the rule because `json` lives
// on the shared flattened `JsonOpt`.
#[derive(Debug, Args)]
pub(crate) struct NewArgs {
    /// Session name. `phux new work` creates a session named "work".
    /// Omitted ⇒ the `session-name-template` (default: the cwd
    /// basename), redrawn if it uses `${random-name}` and the pick is
    /// taken, then disambiguated with a numeric suffix.
    #[usage(value_name = "NAME")]
    pub(crate) name: Option<String>,

    /// Session name in flag form — equivalent to the positional NAME,
    /// and the form required by `--json`. An error if it conflicts
    /// with NAME.
    #[usage(short = 's', long = "session")]
    pub(crate) session: Option<String>,

    /// Working directory for the seed pane.
    #[usage(short = 'c', long = "cwd")]
    pub(crate) cwd: Option<std::path::PathBuf>,

    /// Emit stable, versioned JSON on stdout instead of the human view.
    /// On failure, stdout stays empty and stderr carries one JSON error
    /// object. Requires an explicit `-s NAME` (a positional NAME is not
    /// enough).
    #[usage(long, requires("--session"))]
    pub(crate) json: bool,

    /// Environment assignment for the seed process. Repeat for multiple
    /// variables. Headless `--json` mode only.
    #[usage(
        short = 'e',
        long = "env",
        value_name = "KEY=VALUE",
        requires("--json")
    )]
    pub(crate) env: Vec<EnvAssignment>,

    /// Make the create safe to retry: a repeat with the same key (32 hex
    /// digits) answers the first create's result instead of failing on
    /// the name. Headless `--json` mode only.
    #[usage(
        long = "idempotency-key",
        value_name = "HEX32",
        requires("--json"),
        conflicts("--empty")
    )]
    pub(crate) idempotency_key: Option<String>,

    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,

    /// Create the session with no process. It is persistent, so it
    /// stays until `phux rm` or `phux kill` deletes it. Without `--json`
    /// the new session is attached and shows an empty state; open a
    /// window from there.
    #[usage(long, conflicts("--command", "--env", "--cwd", "--ephemeral"))]
    pub(crate) empty: bool,

    /// Make the session ephemeral: it ends when its last process exits,
    /// even though you named it.
    #[usage(long)]
    pub(crate) ephemeral: bool,

    /// Command (and arguments) to run in the seed pane instead of the
    /// default shell. Must follow `--`: `phux new work -- htop`.
    #[usage(double_dash = "required")]
    pub(crate) command: Vec<String>,
}

/// Kill a session, group, pane, or the server
///
/// `TARGET` uses the selector grammar (`phux help targets`):
/// `name`, `name:N`, `name:N.M`, `name:tag`, `@N`, `^key`, `#tag`, `.`.
/// A selector naming a whole session kills the session; any other is
/// resolved client-side to a set of Terminals, and the server kills
/// them together.
///
/// Killing a whole session ends every member, work added meanwhile
/// included, deletes the session, and prints one summary line.
///
/// `--server` stops the server process instead, ending every session on
/// it. Local socket only: the server accepts that stop on its local
/// socket alone, so `--server` cannot combine with `--remote`.
#[derive(Debug, Args)]
pub(crate) struct KillArgs {
    /// What to kill (selector).
    #[usage(group = "kill_what", required_unless = "--server")]
    pub(crate) target: Option<String>,
    /// Stop the running server, ending every session it holds.
    ///
    /// The server exits cleanly, so a supervised one stays stopped rather
    /// than being restarted. Note that the next `phux attach`/`new` will
    /// auto-spawn a fresh server: this stops the current one, it does not
    /// disable phux.
    #[usage(long, group = "kill_what")]
    pub(crate) server: bool,

    /// Make the kill safe to retry: a repeat with the same key (32 hex
    /// digits) answers the first kill's result and kills nothing. An
    /// `@N` target is sent as written, so a retry after the pane is gone
    /// still gets the first answer. The server must advertise
    /// `keyed_signal`.
    #[usage(long = "idempotency-key", value_name = "HEX32", conflicts("--server"))]
    pub(crate) idempotency_key: Option<String>,
    /// Kill without asking.
    ///
    /// Without it, `phux kill TARGET` asks on a terminal, and when stdin
    /// is not one it refuses with exit 2 having sent nothing. `--server`
    /// never asks.
    #[usage(long)]
    pub(crate) yes: bool,
    /// Emit a schema-versioned JSON result or error: `target`, and
    /// `killed`, the members a session kill ended (`null` for a pane kill).
    #[usage(long, conflicts("--server"))]
    pub(crate) json: bool,

    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,
}

/// Rename a session or a pane
///
/// Reassigns `TARGET`'s human-readable name to `NEW_NAME` in one
/// round-trip. `TARGET` is a session name, or a pane selector (`@N`,
/// `name:N.M`, a key `^ab23cd`) whose label to set. The server is
/// authoritative; attached clients pick up the new name on their next
/// snapshot. An unknown `TARGET`, or a session `NEW_NAME` already in
/// use, is an error.
#[derive(Debug, Args)]
pub(crate) struct RenameArgs {
    /// Current session name, or a pane selector.
    pub(crate) target: String,

    /// New name.
    pub(crate) new_name: String,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,
}

/// Remove a stopped pane, or delete an empty session
///
/// `TARGET` is a pane selector (`@N`, `name:N.M`, a key `^ab23cd`) whose
/// process has ended, or a session name with no members. A pane that
/// still runs, or a session that still has members, is refused: stop it
/// with `phux kill` first.
#[derive(Debug, Args)]
pub(crate) struct RmArgs {
    /// The pane or session to remove.
    pub(crate) target: String,
    /// Remove without asking.
    ///
    /// Without it, `phux rm` asks on a terminal, and when stdin is not
    /// one it refuses with exit 2 having sent nothing.
    #[usage(long)]
    pub(crate) yes: bool,
    /// Emit a schema-versioned JSON result or error.
    #[usage(long)]
    pub(crate) json: bool,
}

/// Start a stopped pane again
///
/// Starts a new run of `TARGET` (a pane selector or key `^ab23cd`) with
/// the command, directory, and environment it was launched with, in the
/// same session. Refused while it still runs, or while a run that a
/// server restart lost track of may still be running.
#[derive(Debug, Args)]
pub(crate) struct RelaunchArgs {
    /// The pane to start again.
    pub(crate) target: String,
    /// Emit a schema-versioned JSON result or error.
    #[usage(long)]
    pub(crate) json: bool,
}

/// Relaunch every pane a server restart ended
///
/// Starts each pane whose process ended because the server stopped while
/// it ran, and each pane imported but never started, in its session, with
/// its directory and agent resume. With `SESSION`, only that session's
/// panes. Prints one summary line plus one line per pane that was
/// refused (`-v`: every pane).
#[derive(Debug, Args)]
pub(crate) struct RestoreArgs {
    /// Only this session's panes.
    pub(crate) session: Option<String>,
    /// Print one line per pane, not a summary.
    #[usage(short = 'v', long)]
    pub(crate) verbose: bool,
    /// Emit a schema-versioned JSON result or error.
    #[usage(long)]
    pub(crate) json: bool,
}

/// Put every pane in the session it belongs in
///
/// Groups panes into sessions by repository and task: panes in Intake
/// or a catch-all session join the session for their repository, a
/// repository with no session gets one, and a numbered session holding
/// one repository takes its name. With `SESSION`, only that session's
/// panes move. The Jev model judges each pane when `TYPESAFE_API_KEY`
/// is set or `organize.jev-key-command` prints a key (only names,
/// titles, directories, and repositories are sent, never screen
/// contents); otherwise, or with `--offline`, repository matching
/// decides. A pane already in its repository's session never
/// leaves it, and a judgment below `--min-confidence` moves nothing.
/// Moves change membership only; every process keeps running.
/// `--undo` reverses the last run.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent CLI flag per bool, as the verb declares them"
)]
#[derive(Debug, Args)]
pub(crate) struct OrganizeArgs {
    /// Only this session's panes may move.
    pub(crate) session: Option<String>,
    /// Print the plan without changing anything.
    #[usage(short = 'n', long)]
    pub(crate) dry_run: bool,
    /// Decide by repository matching only; send nothing off the machine.
    #[usage(long)]
    pub(crate) offline: bool,
    /// Confidence (0 to 1) a judgment needs to move a pane.
    #[usage(
        long,
        value_name = "P",
        default = "0.6",
        default_value_t = 0.6,
        validate = "float(value) >= 0 && float(value) <= 1",
        validate_error = "confidence must be between 0 and 1"
    )]
    pub(crate) min_confidence: f64,
    /// Reverse the last organize run.
    #[usage(long, conflicts("--dry-run", "--offline"))]
    pub(crate) undo: bool,
    /// Emit a schema-versioned JSON result or error.
    #[usage(long)]
    pub(crate) json: bool,
}
