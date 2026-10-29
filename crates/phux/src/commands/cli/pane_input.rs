//! The `Panes` verbs that write into a pane: `send-keys`, `upload`, `paste`, and `run`.

use crate::commands::{JsonOpt, RemoteOpt};
use usage::Args;

/// Send keys to a pane.
#[derive(Debug, Args)]
#[usage(
    about = "Send keys to a pane",
    long_about = "Send keys to a pane.\n\n\
            tmux-shaped: each KEY is a named key (`Enter`, `Tab`, `Escape`, \
            `Up`, `C-c`, `M-x`, …) or a literal string. Literals normally type \
            character by character; a literal run immediately before `Enter` is \
            delivered as a submission-safe paste followed by the real key, honoring \
            the pane's live bracketed-paste mode. TARGET is resolved client-side to \
            one pane, so the live pane is neither attached nor resized.\n\n\
            Flags (`--json`, `--socket`) MUST precede TARGET: KEYS is a trailing \
            var-arg, so anything after TARGET is taken as a key to send.\n\n\
            `--json` prints how many input events the server acknowledged. A \
            failure partway carries `error.recovery`: part of the keys reached \
            the pane, so read it before sending again.\n\n\
            Examples:\n  \
            phux send-keys demo \"echo hi\" Enter\n  \
            phux send-keys work:1.0 C-c"
)]
pub(crate) struct SendKeysArgs {
    /// Target selector: session, session:group, session:group.pane,
    /// @id, or `.` (focused). `=` is unsupported by headless commands.
    pub(crate) target: String,

    /// Keys to send: named keys and/or literal strings, in order.
    #[usage(trailing_var_arg, required)]
    pub(crate) keys: Vec<String>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Upload a local file without attaching or submitting input
#[derive(Debug, Args)]
#[usage(
    long_about = "Upload a file from this client's filesystem, not the pane cwd.

Returns the authoritative path on the terminal-owning host (including satellites). Does not attach, resize, paste, or submit input. A wire path does not imply that an agent provider understands video or other media.

Upload dials configured remote transports headlessly; it never pairs over SSH. The timeout covers resolution, reading, and upload. Completed prints the path; --json prints the receipt for completed, refused, or unknown outcomes. Refused and unknown exit nonzero: do not blindly retry."
)]
pub(crate) struct UploadArgs {
    /// Target selector: session, session:group.pane, @id, or `.`.
    pub(crate) target: String,
    /// File on this client's filesystem.
    pub(crate) file: std::path::PathBuf,
    /// Override the inferred extension (1–16 ASCII alphanumeric characters).
    #[usage(long)]
    pub(crate) extension: Option<String>,
    /// Overall deadline in seconds, bounded to 1..=3600.
    #[usage(long, default = "60", value_name = "SECS")]
    pub(crate) timeout: u64,
    /// Print the structured receipt on stdout, including non-success outcomes.
    #[usage(long)]
    pub(crate) json: bool,
    #[usage(flatten)]
    pub(crate) remote: RemoteOpt,
}

/// Paste text into a pane.
#[derive(Debug, Args)]
#[usage(
    about = "Paste text into a pane",
    long_about = "Paste text into a pane.\n\n\
            Delivers the payload as ONE paste event to the resolved pane \
            (`ROUTE_INPUT`), so the live pane is neither attached nor resized. \
            When the pane's program has bracketed paste (DEC mode 2004) switched \
            on, the server wraps the payload in paste markers and the program \
            receives it as a single block — auto-indent stays off and multiline \
            text arrives intact. Without the mode, the raw bytes are delivered as \
            if typed.\n\n\
            A paste INSERTS; it does not SUBMIT. Paste-aware shells and REPLs \
            buffer the block until a real Enter — follow with \
            `phux send-keys TARGET Enter` to run what you pasted.\n\n\
            TEXT is the payload; omit it to read the payload from stdin. \
            Payloads are trusted by default (you vouch for content you \
            composed); `--untrusted` opts into the server's safety gate.\n\n\
            Examples:\n  \
            phux paste demo 'SELECT count(*) FROM users;'\n  \
            git diff | phux paste review"
)]
pub(crate) struct PasteArgs {
    /// Target selector: session, session:group, session:group.pane,
    /// @id, or `.` (focused). `=` is unsupported by headless commands.
    pub(crate) target: String,

    /// Text to paste. Omit to read the payload from stdin.
    pub(crate) text: Option<String>,

    /// Mark the payload untrusted: the server classifies it and the
    /// pane's untrusted-paste policy (reject by default) may silently
    /// drop an unsafe payload — e.g. anything multiline. Without this
    /// flag the paste is trusted and forwarded verbatim.
    #[usage(long)]
    pub(crate) untrusted: bool,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Run a command in a pane and capture its exit code.
#[derive(Debug, Args)]
#[usage(
    about = "Run a command in a pane and capture its exit code",
    long_about = "Run a command in a pane and capture its exit code.\n\n\
            Reports the command's exit code, output, and duration. \
            Brackets the command with sentinels to capture `$?`, so it \
            assumes a POSIX shell (sh/bash/zsh) — and refuses (exit 2) when a \
            shell is not what is reading the pane, since against `vim` or \
            `less` the same bytes are keystrokes, not a command; `--force` \
            skips that precondition. The process exit code mirrors \
            the command's — and is 125 when `phux` gives up on `--timeout` — so \
            `phux run … && next` composes like a shell. The timeout is one \
            budget for the whole run — connecting, target resolution, input \
            submission, and every screen read — so a server that stops \
            answering still ends the run on time. Input is never started after \
            the timeout, and once started it gets up to 2 more seconds to \
            finish, so the pane is not left holding a half-typed line; the \
            diagnostic says whether nothing, all, or possibly part of the input \
            was delivered. Giving up does not stop the command or retract input \
            already delivered. TARGET is a selector \
            (see the top-level help), resolved client-side to one pane; the \
            command routes to it by id (no attach, no resize).\n\n\
            Flags (`--timeout`, `--force`, `--json`, `--socket`) MUST precede \
            TARGET, or they are swallowed into the trailing command.\n\n\
            Examples:\n  \
            phux run build \"cargo test\"\n  \
            phux run --timeout 30 work:1.0 \"cargo test\""
)]
pub(crate) struct RunArgs {
    /// Target selector: session, session:group, session:group.pane,
    /// @id, or `.` (focused). `=` is unsupported by headless commands.
    pub(crate) target: String,

    /// The command line: all trailing args, joined with spaces.
    #[usage(trailing_var_arg, required)]
    pub(crate) command: Vec<String>,

    /// Give up after this many seconds (exit 125), counted from the start
    /// of the command: connecting, resolving TARGET, submitting the
    /// command, and every screen read share the one budget; input that
    /// has started gets up to 2s more to finish. Default: 600s. Pass 0 to
    /// wait indefinitely.
    #[usage(long, value_name = "SECS")]
    pub(crate) timeout: Option<u64>,

    /// Skip the available-shell precondition. Types the command line
    /// into the pane whatever is running there.
    #[usage(long)]
    pub(crate) force: bool,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}
