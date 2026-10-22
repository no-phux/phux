//! The `More` verbs, and the hidden plumbing ones.

use crate::commands::{CompletionShell, JsonOpt, RemoteOpt, perf::PerfAction};
use usage::{Args, Subcommands};

/// Inspect this binary's protocol and capabilities
#[derive(Debug, Args)]
pub(crate) struct RuntimeInfoArgs {
    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Report who this connection is to the server
///
/// Prints the principal and credential id (for a paired device), the
/// auth route (the local socket, or a QUIC / WebSocket bearer
/// credential), the peer uid (local socket), the OS user and host the
/// server runs as, and the server's version, one field per line. Read
/// only: a phux server never switches users, so the serving user is also
/// the user every pane runs as. With `--remote HOST` it reports what
/// that dial authenticated as there. Does not start a server.
#[derive(Debug, Args)]
pub(crate) struct WhoamiArgs {
    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,
}

/// Show the server's performance telemetry
///
/// Reads the always-on latency histograms, throughput counters, and
/// process figures the server keeps about itself (`GET_PERF`) and
/// prints them as a table grouped by pipeline stage: `pty.*` (child
/// output arriving), `echo.server` (input to first output on the same
/// pane), `tick.*` and `pump.*` (fan-out to clients), `wire.*` (socket
/// writes), `cmd.*` / `attach.*` (control plane), and `consumer.*`
/// (per-client backpressure). Without `--watch` the numbers cover the
/// server's lifetime; with `--watch SECS` the verb polls and prints
/// each interval on its own, so counters become rates and a stall
/// shows up in the second it happened. Does not start a server.
///
/// `phux perf trace` records a span trace of the server for Perfetto;
/// `phux perf alloc on|off` switches its heap accounting.
///
/// `--desktop` reads a phux desktop running in dev mode (`PHUX_DEV=1`)
/// instead: its frame rate, frame build times, jank, CPU, memory, heap
/// and every desktop histogram. `phux perf trace --desktop` records the
/// desktop and the server onto one timeline; `phux perf hud on|off`
/// shows the desktop's in-window HUD.
#[derive(Debug, Args)]
pub(crate) struct PerfArgs {
    #[usage(subcommand)]
    pub(crate) action: Option<PerfAction>,
    #[usage(flatten)]
    pub(crate) json: JsonOpt,
    /// Read the dev-mode phux desktop serving this socket, not the server.
    #[usage(long)]
    pub(crate) desktop: bool,
    /// Poll every SECS seconds and print each interval as a delta.
    #[usage(long, value_name = "SECS")]
    pub(crate) watch: Option<f64>,
    /// Zero the server's metrics after each snapshot.
    #[usage(long)]
    pub(crate) reset: bool,
}

/// Export sessions to an archive file, or import one
///
/// `export` writes the running server's sessions, split trees,
/// directories, and resumable agent sessions as JSON. `import` recreates
/// an archive's missing sessions, persistent and with their panes as
/// dormant records, without starting any process.
#[derive(Debug, Args)]
pub(crate) struct ArchiveArgs {
    #[usage(subcommand)]
    pub(crate) action: ArchiveAction,
}

/// Open a one-attach QUIC listener on this host and print how to reach it.
///
/// The far end of `phux attach --ssh`: starts the server if none is
/// running, asks it for a listener that admits only a token minted for
/// it, and prints one JSON line naming the port, the certificate
/// fingerprint to pin, and the token.
#[derive(Debug, Args)]
pub(crate) struct BootstrapArgs {
    /// The version of the phux that asked, named in a mismatch report.
    #[usage(long, value_name = "VERSION")]
    pub(crate) client_version: Option<String>,

    /// Inclusive UDP port range to bind from, e.g. `60000-61000`.
    #[usage(long, value_name = "MIN-MAX")]
    pub(crate) port_range: Option<String>,

    /// Seconds the listener stays open with nobody connected; `0` asks
    /// for the server default.
    #[usage(long, value_name = "SECS", default = "0", default_value_t = 0)]
    pub(crate) linger: u32,
}

/// Print a shell completion script on stdout.
#[derive(Debug, Args)]
#[usage(
    about = "Print a shell completion script",
    long_about = "Print a shell completion script on stdout.\n\n\
            The script is generated from the binary's own argument parser, so it \
            always matches the verbs this build actually accepts. It contacts no \
            server and reads no config, which is what makes it safe to run from a \
            shell startup file.\n\n\
            Regenerate after upgrading phux; a stale script completes verbs the \
            installed binary no longer has.\n\n\
            Install it the way your shell prefers. Examples:\n  \
            phux completion zsh  > ~/.zfunc/_phux   (~/.zfunc must be on $fpath)\n  \
            phux completion bash > ~/.local/share/bash-completion/completions/phux\n  \
            phux completion fish > ~/.config/fish/completions/phux.fish"
)]
pub(crate) struct CompletionArgs {
    /// Shell dialect to generate for.
    #[usage(value_enum, value_name = "SHELL")]
    pub(crate) shell: CompletionShell,
}

/// Run the bundled MCP stdio adapter
///
/// This is a transparent launcher for the separate MCP companion
/// binary. All arguments are forwarded unchanged. With no arguments it
/// serves MCP over stdin/stdout; discovery modes include `--skill`,
/// `--schema`, `--help`, and `--version`.
#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    /// Arguments forwarded unchanged to the MCP companion.
    #[usage(value_name = "ARGS", trailing_var_arg, allow_hyphen_values)]
    pub(crate) args: Vec<std::ffi::OsString>,
}

/// Print the agent skill this binary ships with, on stdout.
#[derive(Debug, Args)]
#[usage(
    about = "Print the agent skill this binary ships with",
    long_about = "Print the agent skill this binary ships with, on stdout.\n\n\
            The text is compiled into the executable, so it describes the verbs \
            and flags THIS build actually has — it cannot drift from the binary \
            the way a copied file can. It contacts no server and reads no \
            config.\n\n\
            Give it to any agent that needs to drive phux: it teaches the \
            read-act-wait loop, the selector grammar, the difference between a \
            level read and an observed transition, the exit codes, and the \
            safety rules for driving a terminal a human may also be using.\n\n\
            Scopes are `quick` (core loop and safety), `agent` (lifecycle and \
            identity), `terminal` (screen and input mechanics), and `full` \
            (everything, the default). Examples:\n  \
            phux skill\n  \
            phux skill agent\n  \
            phux --skill=terminal\n  \
            phux --skill=quick | pbcopy"
)]
pub(crate) struct SkillArgs {
    /// Amount and subject of guidance to print.
    #[usage(value_enum, default = "full", value_name = "SCOPE")]
    pub(crate) scope: crate::skill::SkillScope,
}

/// Regenerate the repository's generated reference pages (internal).
///
/// Hidden developer tooling behind `just docs-gen`, not part of the
/// user-facing surface: renders the reference pages from this binary's
/// own inventories and writes them into the checkout. A unit test
/// byte-compares the checked-in pages against this generator, so the
/// published reference can never drift from the compiled binary.
#[derive(Debug, Args)]
pub(crate) struct GenReferenceDocsArgs {
    /// Directory to write the pages into. Defaults to the checkout's
    /// generated-reference tree; run from the repository root.
    #[usage(long, value_name = "DIR")]
    pub(crate) out: Option<std::path::PathBuf>,
}

/// `phux archive <action>` — the session archive.
#[derive(Debug, Subcommands)]
pub(crate) enum ArchiveAction {
    /// Write the running server's sessions as a JSON archive.
    Export {
        /// Write the archive to a path instead of stdout.
        #[usage(long, short = 'o', value_name = "PATH")]
        output: Option<std::path::PathBuf>,

        /// Read each session's split tree from this projection key PREFIX
        /// (`<PREFIX>.layout/v1`) instead of the shared default, appending
        /// `/<session-id>` itself for each session archived. Unlike every
        /// other `--projection` flag, this one takes a bare prefix, not a
        /// full key: a full `<prefix>.layout/v1/<id>` key is refused. A
        /// session with nothing stored under its own key falls back to its
        /// bare pane list; when this flag is given, that absence is also
        /// reported on stderr.
        #[usage(long, value_name = "PREFIX")]
        projection: Option<String>,
    },

    /// Recreate an archive's missing sessions without starting them.
    ///
    /// Each missing session becomes a persistent session with its archived
    /// host, project, project link, and directory, and each archived local
    /// pane a dormant record in its arrangement; nothing is launched until
    /// `phux restore`.
    /// A session of the same name already on the server is skipped.
    Import {
        /// JSON archive path, or '-' to read from stdin.
        archive: std::path::PathBuf,
        /// Print the archive's session, host, and directory organization
        /// and do not dial a server.
        #[usage(long)]
        plan: bool,
    },
}
