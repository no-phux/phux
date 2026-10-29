//! The value types and flattened option groups the verbs share.

use phux_protocol::wire::frame::TerminalSignal;
use usage::{Args, ValueEnum};

use crate::commands::server_target;

/// CLI signal names for `phux signal TARGET SIGNAL` (ADR-0033), mapped to the
/// wire [`TerminalSignal`]. The POSIX spellings a `kill(1)` user types
/// (`TERM`, `SIGTERM`, `INT`, ...) are accepted as hidden aliases, in any
/// case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[usage(ignore_case)]
pub(crate) enum SignalArg {
    /// SIGINT — the Ctrl-C equivalent.
    #[usage(alias = "int", alias = "sigint")]
    Interrupt,
    /// SIGSTOP — pause the process group (reversible via `resume`).
    #[usage(alias = "stop", alias = "sigstop")]
    Freeze,
    /// SIGCONT — resume a frozen process group.
    #[usage(alias = "cont", alias = "sigcont")]
    Resume,
    /// SIGTERM — request graceful termination.
    #[usage(alias = "term", alias = "sigterm")]
    Terminate,
    /// SIGKILL — force termination.
    #[usage(alias = "sigkill")]
    Kill,
}

/// Parsed `--split` spelling; creation verbs name the placement axis, while
/// existing-pane edits name the divider. Keep the per-command help explicit.
/// `h` / `v` are accepted shorthands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SpawnSplit {
    #[usage(visible_alias = "h")]
    Horizontal,
    #[usage(visible_alias = "v")]
    Vertical,
}

/// The shell `phux completion` writes a script for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CompletionShell {
    Bash,
    Elvish,
    Zsh,
    Fish,
    Nu,
    // Spelled as the shell is (`powershell`, `pwsh`); `power-shell`, the
    // derived kebab-case word this once was, stays accepted.
    #[usage(name = "powershell", alias = "power-shell", alias = "pwsh")]
    PowerShell,
}

impl From<CompletionShell> for usage::complete::Shell {
    fn from(shell: CompletionShell) -> Self {
        match shell {
            CompletionShell::Bash => Self::Bash,
            CompletionShell::Elvish => Self::Elvish,
            CompletionShell::Zsh => Self::Zsh,
            CompletionShell::Fish => Self::Fish,
            CompletionShell::Nu => Self::Nu,
            CompletionShell::PowerShell => Self::PowerShell,
        }
    }
}

/// One `KEY=VALUE` assignment for `phux new --env`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvAssignment {
    pub key: String,
    pub value: String,
}

impl std::str::FromStr for EnvAssignment {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_env_assignment(value).map(|(key, value)| Self { key, value })
    }
}

/// `phux snapshot --format`: which libghostty-vt Formatter rendering to
/// request from the server (D9, fallback rung three — below typed
/// commands and semantic streams, above synthetic input,
/// `docs/scripting.md#reading-a-screen`). The server never reimplements extraction
/// (CONTRIBUTING); this only names the two renderings it forwards to the
/// engine's own Formatter. Mutually exclusive with `--cells`
/// (`ScreenState::cells` is a different, sparse per-cell projection) and
/// `--rendered` (the CLIENT's composited multi-pane view — this instead
/// asks the SERVER to render one pane through its own engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SnapshotFormat {
    /// HTML with inline styles.
    Html,
    /// VT escape sequences (colors, styles, hyperlinks).
    Vt,
}

impl SnapshotFormat {
    /// `GET_SCREEN`'s wire byte for this format (D9).
    #[must_use]
    pub(crate) const fn wire_byte(self) -> u8 {
        match self {
            Self::Html => phux_client::snapshot::SCREEN_FORMAT_HTML,
            Self::Vt => phux_client::snapshot::SCREEN_FORMAT_VT,
        }
    }
}

/// Output format for a recording, shared by `--rec-format` on the attach
/// path and `--format` on the `rec` verb.
///
/// `Apng` covers both the `.png` and `.apng` extensions: a recording is an
/// animation, and this surface never produces a still frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RecFormat {
    /// asciinema cast — the archival, re-renderable artifact.
    Cast,
    /// Animated GIF — shareable and embeddable anywhere.
    Gif,
    /// Animated PNG — truecolor, no quantization, larger files.
    Apng,
}

/// The `--rec` / `--rec-format` pair, declared on exactly the two paths that
/// raise a TUI: the root command (naked `phux`) and `phux attach`.
///
/// Shared through `#[usage(flatten)]` rather than a `global` arg on
/// the root: a global would parse — and advertise itself in `--help` — on
/// every verb, including the headless ones that can never tee a composited
/// frame. Scoping it here makes `phux ls --help` honest by construction
/// instead of by a runtime rejection. Headless capture is `phux rec`.
#[derive(Debug, Args)]
pub(crate) struct RecOpts {
    /// Record this session (.cast, .gif, or .apng)
    // Headless capture of one pane is `phux rec`, which is where the
    // format prose lives; the naked attach and `phux attach` keep one line.
    #[usage(long = "rec", value_name = "PATH")]
    pub(crate) rec: Option<std::path::PathBuf>,

    /// Recording format, overriding PATH's extension
    #[usage(long = "rec-format", value_enum, value_name = "FMT", requires("--rec"))]
    pub(crate) rec_format: Option<RecFormat>,
}

/// The shared `--json` declaration for the core server-talking verbs
/// (ADR-0065 §3, phux-i0e8.8.2).
///
/// `--json` stays verb-scoped rather than global so help stays honest by
/// construction (a global would advertise itself on verbs with no JSON
/// projection); this one flattened struct is the single declaration those
/// verbs share, so the doc string and the error contract cannot drift
/// per-verb. Deliberately **no `-j` short flag**: considered and rejected in
/// ADR-0065 §7 (`--json` is typed by scripts and agents, where explicitness
/// beats two saved characters), cross-referenced in
/// `docs/scripting.md#json-output-and-errors`.
#[derive(Debug, Args)]
pub(crate) struct JsonOpt {
    /// Emit stable, versioned JSON on stdout instead of the human view.
    /// On failure, stdout stays empty and stderr carries one JSON error
    /// object.
    #[usage(long)]
    pub(crate) json: bool,
}

/// The `--remote` target the headless session verbs share (phux-c2td.2),
/// flattened like [`JsonOpt`] so the help text and the resolution rule
/// cannot drift per verb. Resolution lives in [`server_target`].
#[derive(Debug, Args)]
pub(crate) struct RemoteOpt {
    /// Run against the phux server on another machine instead of the local
    /// socket, ssh-style: `--remote me@mini`. Same target and resolution as
    /// `phux attach --remote`: a registered host is dialed directly over
    /// QUIC or WSS, and an unregistered one is paired over your ssh trust
    /// first and remembered (except `upload`, `download`, and calls with
    /// `--json`: these never pair and refuse unregistered hosts, naming
    /// remedies). PORT
    /// defaults to 8788. Cannot combine with `--socket`.
    #[usage(long, value_name = "[USER@]HOST[:PORT]")]
    pub(crate) remote: Option<String>,
}

impl RemoteOpt {
    /// Pair this verb's `--remote` with the global `--socket`.
    pub(crate) fn with_socket(
        self,
        socket: Option<std::path::PathBuf>,
    ) -> server_target::ServerSpec {
        server_target::ServerSpec {
            socket,
            remote: self.remote,
        }
    }
}

/// Parse one environment assignment without imposing shell-variable naming
/// rules: `execve` only forbids an empty key, `=`, and NUL.
fn parse_env_assignment(value: &str) -> Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| "environment assignment must be KEY=VALUE".to_owned())?;
    if key.is_empty() {
        return Err("environment key must not be empty".to_owned());
    }
    if key.contains('\0') || value.contains('\0') {
        return Err("environment assignment must not contain NUL".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

impl From<SignalArg> for TerminalSignal {
    fn from(arg: SignalArg) -> Self {
        match arg {
            SignalArg::Interrupt => Self::Interrupt,
            SignalArg::Freeze => Self::Freeze,
            SignalArg::Resume => Self::Resume,
            SignalArg::Terminate => Self::Terminate,
            SignalArg::Kill => Self::Kill,
        }
    }
}
