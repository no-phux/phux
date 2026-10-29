//! The `Maintain` verbs that inspect and update this install.

use crate::commands::{JsonOpt, report::ReportAction, update};
use usage::Args;

/// Report the running server: pid, uptime, clients
///
/// One glance at the server behind the socket: whether it is running
/// and as which pid, since when, the protocol version it speaks, how
/// many clients are attached, the sessions it holds, and where its
/// logs live. Does not start a server: with no server running it
/// reports as much and exits non-zero. Pass `--json` for the stable,
/// versioned machine shape instead of the human text; with no server
/// that shape is `{"running": false, ...}` on stdout, still exiting
/// non-zero.
// `status` reports "no server" as a stdout answer, not an error, so its
// `--json` help differs from the shared `JsonOpt` wording.
#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Update phux to the latest stable or next release, keeping sessions alive.
#[derive(Debug, Args)]
#[usage(
    about = "Update phux, keeping sessions alive",
    long_about = "Update phux to the latest stable or next release, keeping sessions alive.\n\n\
            Checks the published release, downloads the archive for this platform, \
            verifies it against the checksum published beside it, replaces the \
            binaries atomically, and asks a running server to re-exec so live panes \
            survive. `--channel next` follows green `main` instead of the latest \
            `vX.Y.Z`; `--channel latest` is the numbered releases. `phux channel` \
            is the same switch without the flag. A server, its local clients, its \
            satellites, and its relays must \
            all run the same release, so this is the command that moves a whole \
            deployment in one step.\n\n\
            phux updates only installs it maintains: a release archive unpacked into \
            $PHUX_INSTALL_DIR, ~/.local/bin, ~/bin, /usr/local/bin, or /opt/phux/bin. \
            A Homebrew, Cargo, or Nix install is never modified — the exact native \
            command is printed instead — and an unrecognized location is refused \
            rather than overwritten.\n\n\
            The previous binaries are kept beside the new ones; `--rollback` puts \
            them back.\n\n\
            Examples:\n  \
            phux update --check\n  \
            phux update --check --json\n  \
            phux update\n  \
            phux update --channel next\n  \
            phux update --channel latest\n  \
            phux update --dry-run --version v1.2.3\n  \
            phux update --rollback"
)]
pub(crate) struct UpdateArgs {
    /// Update options.
    #[usage(flatten)]
    pub(crate) opts: update::UpdateOpts,
}

/// Show or switch the release channel.
#[derive(Debug, Args)]
#[usage(
    about = "Show or switch the release channel",
    long_about = "Show or switch the release channel.\n\n\
            Bare `phux channel` reports the rail this install follows and what \
            is published there. `phux channel next` follows green `main`; \
            `phux channel latest` (also `stable`) follows the numbered GitHub \
            releases. Switching persists the choice and runs the same update \
            path as `phux update --channel`, so live panes survive.\n\n\
            Examples:\n  \
            phux channel\n  \
            phux channel next\n  \
            phux channel latest"
)]
pub(crate) struct ChannelArgs {
    /// Channel to follow. Omit to report the current rail without changing it.
    #[usage(value_enum, value_name = "CHANNEL")]
    pub(crate) channel: Option<update::channel::ChannelArg>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Hot-swap the running server to the installed binary
///
/// Asks the server to snapshot every pane, re-exec the on-disk binary, and
/// re-adopt the live PTYs, so the shells / editors / agents in every
/// session survive a binary update (e.g. after `cargo install` /
/// `brew upgrade`). Clients briefly disconnect and reconnect. This is the
/// low-level primitive: it re-execs whatever is already on disk and
/// downloads nothing. `phux update` is the command that puts a new binary
/// there first.
///
/// A refusal the server marks as a passing race (its tree changed, a pane
/// is still being reaped, the handoff deadline elapsed) is retried a few
/// times over about five seconds. A server older than agent-session-safe
/// handoff refuses while any agent session exists; on a terminal this
/// offers to close them and retry, otherwise it prints the commands.
#[derive(Debug, Args)]
pub(crate) struct UpgradeArgs {
    /// When an older server refuses because agent sessions exist, close
    /// them (`phux agent session close`) and retry without asking.
    /// Panes and agent processes keep running; only the closed sessions'
    /// event logs end.
    #[usage(long)]
    pub(crate) close_agent_sessions: bool,
}

/// Diagnose the install: config, socket, server
///
/// Composes the checks that already exist as separate verbs and reports
/// one verdict, because knowing which four commands to run and how to
/// read each one is exactly what someone debugging phux does not have.
///
/// Read-only. Exits 1 if any check failed; warnings alone exit 0,
/// since a stopped server is a normal state and not a broken install.
#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Emit a stable JSON document instead of human text.
    #[usage(long)]
    pub(crate) json: bool,
}

/// Show where the logs live, or tail one
///
/// Bare `phux logs` prints the inventory: the canonical server log
/// (every spawn path writes it), the per-pid client logs, and the state
/// dir that holds them — with existence, size, and age, so a fresh
/// machine reads "not created yet" instead of an error. `--server`
/// tails the server log, `--client` the newest client log (`--pid`
/// picks a specific one);
/// `-f` follows and `-n` sets the tail length. `--json` emits the
/// inventory as a stable document.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent CLI flag per bool, as the verb declares them"
)]
#[derive(Debug, Args)]
pub(crate) struct LogsArgs {
    /// Tail the canonical server log.
    #[usage(long, group = "which")]
    pub(crate) server: bool,

    /// Tail the newest per-pid client log (or the one `--pid` names).
    #[usage(long, group = "which")]
    pub(crate) client: bool,

    /// With --client: the client pid whose log to tail, instead of the
    /// newest.
    #[usage(long, value_name = "PID", requires("--client"))]
    pub(crate) pid: Option<u32>,

    /// Follow the tailed log as it grows (needs --server or --client).
    #[usage(short, long)]
    pub(crate) follow: bool,

    /// How many trailing lines to show; 200 when omitted (needs
    /// --server or --client).
    #[usage(short = 'n', long, value_name = "NUM")]
    pub(crate) lines: Option<u32>,

    /// Emit the path inventory as a stable JSON document instead of
    /// human text. Inventory only — it cannot combine with a tail.
    #[usage(
        long,
        conflicts("--server", "--client", "--pid", "--follow", "--lines")
    )]
    pub(crate) json: bool,
}

/// Capture or list local bug reports
///
/// Bare `phux report` lists bundles under the profile state directory
/// (newest first; `latest` is printed first). `phux report show [ID]`
/// prints one `report.md` (omit ID for the newest). `phux report new`
/// writes a logs-and-version bundle from a shell; prefer the TUI action
/// `report-bug` (`C-a B`) while attached so the live session, pane, and
/// screen are included. An agent given a report path can `cat` it or
/// run `phux report show`.
#[derive(Debug, Args)]
pub(crate) struct ReportArgs {
    #[usage(subcommand)]
    pub(crate) action: Option<ReportAction>,
    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}
