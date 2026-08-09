//! `phux agent start` — start an agent **into an existing shell pane** and
//! return only once it is detected and ready for interactive input.
//!
//! # Why this is a verb and not a flag on `launch`
//!
//! `phux launch` returns a Terminal id: its whole success statement is "a pane
//! now exists". This returns a *readiness assertion about a pane that already
//! existed*, and it creates, splits, and moves nothing — that separation is
//! the point of the verb. Two more divergences make a shared flag wrong rather
//! than merely awkward: `launch` hands an argv vector to `SPAWN_TERMINAL`
//! (structured, no shell, deliberately so per ADR-0042), while the only way
//! into a pane whose child is a live shell is to *type a command line*; and
//! the failure families are disjoint (this verb has no spawn errors and adds
//! an entire precondition family). What *is* shared is the resolver:
//! [`phux_plugin::resolve_launch`] and [`super::prepare_for_launch`] are
//! called unchanged, and only the delivery step diverges.
//!
//! # What "ready" means here, and what it deliberately does not
//!
//! **Not `state == idle`.** Not one of the five shipped detection manifests
//! declares a positive `idle` rule — `idle` is the fail-safe fallthrough, so
//! it is equally true of a half-painted TUI, a crashed agent, and a pane where
//! nothing ever launched. Ready is instead **the first detector publication
//! after submit**, which is an observed *transition* off the `unknown` this
//! verb binds before typing anything. That predicate already has a home:
//! [`phux_client::agent_wait::wait_for_agent_state`] (ADR-0076 point 5)
//! *seeds* its baseline and never evaluates it, so this file composes against
//! it rather than carrying a second copy of the same plumbing.
//!
//! The default target set is every derived state — `idle`, `working`,
//! `blocked`, `done` — because an agent that starts straight into `blocked`
//! (a trust-this-folder prompt) *is* ready for interactive input, which is the
//! verb's actual promise.
//!
//! # Where the readiness phase lives
//!
//! ADR-0046 point 2 is emphatic that the detector is level-triggered and
//! "nothing is remembered but the last published tuple", which is in tension
//! with a phase machine that must remember it is pending. The resolution is
//! that **the phase machine lives in neither the detector nor the server**: it
//! is this process's stack frame, held open across one subscription for the
//! whole operation. `Pending` is "I bound the name and I hold a subscription";
//! `Active` is "a qualifying transition arrived"; `Failed` is a deadline or a
//! departure. A killed `phux agent start` therefore leaves no server-side
//! phase state to garbage-collect — only the bound name, which is inert
//! (ADR-0075 point 7 refuses input to a withdrawn record) and cleared by
//! `phux agent clear`.
//!
//! # Ordering, and why no sequence counter is needed
//!
//! The bind writes `state: "unknown"` and is read back before anything is
//! typed, so the level the wait baselines on is known by construction. No
//! publication about the *new* agent can precede the submit, because the agent
//! process does not exist until the shell execs it — which happens after the
//! Enter this verb sends is consumed, and the detector then owes a full
//! identification plus its startup grace on top. So any derived state the wait
//! observes is strictly post-submit without the CLI needing a sequence field.
//!
//! # The available-shell precondition, and the surface it is missing
//!
//! herdr's gate is three clauses: the pane's foreground pgid equals its child
//! pid, the job holds only that shell, and the name is a known shell. phux has
//! the raw materials for clauses 1 and 3 **server-side only** — the detector
//! already makes both syscalls on its identify path
//! (`phux-server/src/proc_query.rs`) — and no wire surface carries the answer:
//! `TerminalInfo` has no foreground-process field, and there is no other read
//! path. Publishing a server-derived `phux.pane-occupant/v1` L3 key is the
//! right fix and is filed; it is normative spec surface and is not this
//! change.
//!
//! What ships here is the degraded check, stated out loud rather than
//! silently: the pane's `phux.agent/v1` record must not name a live occupant,
//! and the pane must show an OSC-133 semantic mark **on the cursor's row** —
//! positive evidence that the thing holding the screen right now is a shell
//! prompt. Three-valued on purpose (see [`ShellCheck`]), because "shell
//! integration is off" and "the cursor is not at a prompt" are different
//! answers and only the second is a confident refusal. `--force` skips it, and
//! `--json` always reports which check ran.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_client::agent_meta::{
    AgentMetaState, AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record,
};
use phux_client::agent_wait::{AgentWaitError, AgentWaitResult, wait_for_agent_state};
use phux_client::attach::AttachError;
use phux_client::attach::connection::{Answer, Connection};
use phux_core::screen::{ScreenState, SemanticContent};
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{InputOperationId, TerminalId};
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind, Scope};
use phux_server::agent_explain::{self, Explanation};
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::codes;
use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};
use crate::exit_codes::{EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE, EXIT_WAIT_TIMEOUT};

/// Version of the `agent start` result document.
const RESULT_SCHEMA_VERSION: u8 = 1;

/// Poll-floor cadence for the readiness wait's `GET_METADATA` re-read — the
/// same gap `phux wait` and `phux agent wait` use.
const POLL_INTERVAL: Duration = phux_client::wait::DEFAULT_POLL_INTERVAL;

/// Default readiness deadline, in seconds.
///
/// Bounded by default, unlike `phux agent wait`: a verb whose success *is* a
/// readiness claim must not hang forever when the claim cannot be made. The
/// number is sized off the server's own constants rather than taste. After the
/// command line lands, an already-existing pane's detector is past its
/// identify-acquire window and re-identifies on a 5 s cadence; identification
/// then owes a 3 s startup grace before it may publish at all, plus one 300 ms
/// tick. Worst case is therefore ~8.3 s of pure detector latency before the
/// agent's own splash is even considered, and a cold agent binary adds its
/// own. 60 s leaves generous headroom over that without being unbounded.
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// The addressable agent-name grammar (ADR-0075 point 5), as prose for the
/// refusal message.
const NAME_GRAMMAR: &str = "^[a-z][a-z0-9_-]{0,31}$";

/// Longest shell line this verb will type, in bytes.
///
/// One sixteenth of ADR-0053's 65536-byte `APPLY_INPUT` command-body cap, so
/// the batch limit can never be the thing that fails, and comfortably inside
/// every shell's line buffer. A resolved launch argv longer than this is
/// pathological and is refused before anything is written rather than
/// truncated on the way in.
const MAX_SHELL_LINE: usize = 4096;

/// The typed line must sit inside ADR-0053's command-body cap, or the batch
/// limit could be the thing that fails after a name is already bound. Checked
/// at compile time so the two constants cannot drift apart silently.
const _: () = assert!(MAX_SHELL_LINE < phux_protocol::MAX_APPLY_INPUT_COMMAND_BODY);

/// The readiness target set: every *derived* state.
///
/// `unknown` is deliberately absent — it is the level this verb binds, and
/// the wait treats a return to it as departure rather than arrival.
const READY_STATES: &[AgentMetaState] = &[
    AgentMetaState::Idle,
    AgentMetaState::Working,
    AgentMetaState::Blocked,
    AgentMetaState::Done,
];

/// One `agent start` invocation, as parsed.
///
/// A struct rather than a nine-argument function so the call site in
/// `mod.rs` reads as the flag list it is.
#[derive(Debug)]
pub(super) struct StartRequest<'a> {
    /// Human-facing agent name to bind to the pane.
    pub(super) name: &'a str,
    /// Detection-manifest kind slug the started agent must identify as.
    pub(super) kind: &'a str,
    /// Selector for the existing pane to start into.
    pub(super) target: &'a str,
    /// Launch integration id; defaults to `kind`.
    pub(super) integration: Option<&'a str>,
    /// Readiness deadline in seconds.
    pub(super) timeout: Option<u64>,
    /// Submit and return without waiting for readiness.
    pub(super) no_wait: bool,
    /// Skip the available-shell precondition.
    pub(super) force: bool,
    /// Emit the machine-readable result document.
    pub(super) json: bool,
    /// Extra arguments appended to the integration's launch argv.
    pub(super) args: &'a [String],
}

/// A refusal: the error document plus the status to exit with.
#[derive(Debug)]
struct Refusal {
    /// The stable code / message / remedy triple.
    err: json_err::CliError,
    /// The exit status this refusal carries.
    exit: u8,
}

impl Refusal {
    /// Build a refusal from its parts.
    fn new(
        code: &'static str,
        message: impl Into<String>,
        remedy: impl Into<String>,
        exit: u8,
    ) -> Self {
        Self {
            err: json_err::CliError::new(code, message, remedy),
            exit,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure core: name grammar, shell-line construction, the shell precondition.
// Everything here is unit-testable with no server, no socket, and no clock.
// ---------------------------------------------------------------------------

/// Whether `name` is spellable under the addressable agent-name grammar
/// (ADR-0075 point 5).
///
/// Checked locally so a typo fails before any round trip. The record's `name`
/// stays "any non-empty string" per `L3.md` §3.7 — this narrower grammar is
/// what `%name` can address, and binding a name this verb's own success makes
/// addressable would be dishonest.
#[must_use]
pub(super) fn is_addressable_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if name.len() > 32 {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}

/// Quote one word for a POSIX shell, unconditionally.
///
/// Single-quote everything, escaping an embedded `'` as `'\''`. No
/// "it looks safe" fast path: the argv comes from a plugin-authored template
/// with `${PHUX_PLUGIN_ROOT}` already expanded to an absolute path that may
/// contain spaces, and ADR-0068 native session ids are attacker-adjacent data
/// validated only for "non-option-shaped, control-free, <=1024 bytes" —
/// nothing in that validation stops a `$(...)` or a `;`.
#[must_use]
pub(super) fn shell_quote(word: &str) -> String {
    let mut out = String::with_capacity(word.len().saturating_add(2));
    out.push('\'');
    for ch in word.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Why a resolved launch cannot be typed as one shell line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShellLineError {
    /// The integration resolved to no argv at all.
    Empty,
    /// An element carries a control character, so quoting cannot contain it —
    /// a newline in the line is a second command.
    Control {
        /// Zero-based index into the argv (env pairs come first, negatively
        /// indexed conceptually; this names the argv element).
        index: usize,
        /// The offending element, as resolved.
        element: String,
    },
    /// An environment variable name is not a shell-safe identifier.
    EnvName(String),
    /// The assembled line is longer than [`MAX_SHELL_LINE`].
    TooLong {
        /// Length of the assembled line, in bytes.
        bytes: usize,
    },
}

/// Assemble the one shell line that starts the agent.
///
/// Environment goes through `env` rather than a `VAR=v cmd` prefix: that form
/// is not valid fish, and phux does not know which shell the pane is running.
/// Every element — `argv[0]`, every flag, every `-- extra` argument, and each
/// `NAME=value` pair — is quoted by [`shell_quote`].
///
/// This is the one place phux gains a shell-evaluation surface it did not
/// have, which is why the refusals below are absolute rather than best effort.
pub(super) fn shell_line(
    argv: &[String],
    env: &BTreeMap<String, String>,
) -> Result<String, ShellLineError> {
    if argv.is_empty() {
        return Err(ShellLineError::Empty);
    }
    for (index, element) in argv.iter().enumerate() {
        if element.chars().any(char::is_control) {
            return Err(ShellLineError::Control {
                index,
                element: element.clone(),
            });
        }
    }
    let mut words: Vec<String> = Vec::with_capacity(argv.len().saturating_add(env.len() + 1));
    if !env.is_empty() {
        words.push("env".to_owned());
        for (key, value) in env {
            if key.is_empty()
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                || key.starts_with(|ch: char| ch.is_ascii_digit())
            {
                return Err(ShellLineError::EnvName(key.clone()));
            }
            if value.chars().any(char::is_control) {
                return Err(ShellLineError::Control {
                    index: 0,
                    element: format!("{key}=<value>"),
                });
            }
            words.push(shell_quote(&format!("{key}={value}")));
        }
    }
    words.extend(argv.iter().map(|arg| shell_quote(arg)));
    let line = words.join(" ");
    if line.len() > MAX_SHELL_LINE {
        return Err(ShellLineError::TooLong { bytes: line.len() });
    }
    Ok(line)
}

/// What the client-side available-shell check could establish.
///
/// Three-valued because two of the answers are refusals for different reasons
/// and the third is an admission. Collapsing them would make the verb either
/// unusable (refusing every pane without shell integration) or unsafe
/// (typing into `vim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShellCheck {
    /// An OSC-133 `Prompt`/`Input` mark sits on the cursor's row: whatever
    /// holds the screen right now is a shell command line.
    AtPrompt,
    /// The pane carries OSC-133 marks somewhere, but not on the cursor's row.
    /// Shell integration is on and the cursor is somewhere else — positive
    /// evidence that something other than the prompt has the screen.
    NotAtPrompt,
    /// No semantic marks at all, or no resolvable cursor. Shell integration is
    /// probably off, and phux cannot answer the question client-side.
    Unanswerable,
}

impl ShellCheck {
    /// The wire word for `--json`.
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::AtPrompt => "at-prompt",
            Self::NotAtPrompt => "not-at-prompt",
            Self::Unanswerable => "unanswerable",
        }
    }
}

/// Evaluate the available-shell precondition against one screen.
///
/// Deliberately scoped to the **cursor's row** rather than the whole viewport:
/// a prompt mark left further up the screen is equally true while a build runs
/// in the foreground, and that is exactly the case the precondition exists to
/// catch.
#[must_use]
pub(super) fn shell_check(screen: &ScreenState) -> ShellCheck {
    let Some(cells) = screen.cells.as_ref() else {
        return ShellCheck::Unanswerable;
    };
    let marked: Vec<&phux_core::screen::CellInfo> = cells
        .iter()
        .filter(|cell| {
            matches!(
                cell.semantic,
                Some(SemanticContent::Input | SemanticContent::Prompt)
            )
        })
        .collect();
    if marked.is_empty() {
        return ShellCheck::Unanswerable;
    }
    let Some(cursor) = screen.cursor.as_ref() else {
        return ShellCheck::Unanswerable;
    };
    if marked.iter().any(|cell| cell.row == cursor.y) {
        ShellCheck::AtPrompt
    } else {
        ShellCheck::NotAtPrompt
    }
}

/// How the started agent's identity was confirmed at readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KindVerdict {
    /// The published record's `kind` is the requested one — the kernel says
    /// that binary's process group owns the pane's tty.
    Confirmed,
    /// The record carries no `kind`, so nothing confirmed the occupant. Not a
    /// failure: an explicit hook writer may own this record and stand the
    /// detector down (ADR-0046 point 8), in which case no kind was ever
    /// derived. Reported rather than refused.
    Unconfirmed,
    /// The record names a different kind than the one requested. The
    /// observed kind is not carried here: the caller already holds the record
    /// and formats it, and duplicating the string would give two places for
    /// it to be wrong.
    Mismatch,
}

/// Compare the requested kind against the record readiness landed on.
#[must_use]
pub(super) fn kind_verdict(record: Option<&AgentRecord>, requested: &str) -> KindVerdict {
    let Some(kind) = record.and_then(|rec| rec.kind.as_deref()) else {
        return KindVerdict::Unconfirmed;
    };
    if kind.trim().eq_ignore_ascii_case(requested.trim()) {
        KindVerdict::Confirmed
    } else {
        KindVerdict::Mismatch
    }
}

/// Where the state that satisfied readiness came from, in the manifest's own
/// terms.
///
/// The point of publishing this is that `manifest_asserts_idle` is `false` for
/// every shipped manifest, and a caller who reads it learns the thing an
/// opaque status word hides: *this readiness is an absence of contrary
/// evidence, not a positive observation.* A careful orchestrator can then wait
/// for a transition into `working` before trusting the pane.
#[must_use]
fn state_source(explanation: &Explanation) -> &'static str {
    if explanation.freeze {
        return "frozen";
    }
    if explanation.matched_rule.is_some() {
        return "rule";
    }
    if explanation
        .evaluated_rules
        .iter()
        .all(|rule| rule.state.is_none())
    {
        // A manifest that declares binaries but no state-bearing rule would
        // identify, publish `idle` at grace expiry, and satisfy readiness — a
        // genuine false ready. No shipped manifest is shaped that way; a
        // user-supplied one under PHUX_AGENT_RULES_DIR may be.
        return "no-rules";
    }
    "fail-safe"
}

// ---------------------------------------------------------------------------
// The verb.
// ---------------------------------------------------------------------------

/// `phux agent start NAME --kind K --target TARGET [--integration ID]
/// [--timeout SECS] [--no-wait] [--force] [--json] [-- ARGS]`.
pub(super) fn run_agent_start(action: &super::AgentAction, socket: Option<PathBuf>) -> ExitCode {
    let super::AgentAction::Start {
        name,
        kind,
        target,
        integration,
        timeout,
        no_wait,
        force,
        json,
        args,
    } = action
    else {
        // Unreachable: `run_agent` routes only its own variant here.
        return ExitCode::FAILURE;
    };
    start(
        &StartRequest {
            name,
            kind,
            target,
            integration: integration.as_deref(),
            timeout: *timeout,
            no_wait: *no_wait,
            force: *force,
            json: *json,
            args,
        },
        socket,
    )
}

/// The verb proper, over a parsed request.
fn start(req: &StartRequest<'_>, socket: Option<PathBuf>) -> ExitCode {
    // Steps 1-6 are local: every cheap refusal happens before the first byte
    // is written, so the common failures (typo in the name, unsupported kind,
    // no such integration) never leave residue in a pane.
    let plan = match preflight(req) {
        Ok(plan) => plan,
        Err(refusal) => return json_err::emit(req.json, &refusal.err, refusal.exit),
    };
    let selector = match parse_selector(Some(req.target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move { drive(req, &plan, &selector, &socket_path).await })
}

/// Everything resolved before a socket is opened.
#[derive(Debug)]
struct Plan {
    /// Canonical detection-manifest kind slug.
    kind: String,
    /// The one line to type.
    line: String,
    /// Directory the integration wants to run in.
    resolved_cwd: PathBuf,
    /// Integration id that produced the argv, for diagnostics.
    integration_id: String,
    /// Readiness deadline.
    timeout: Duration,
}

/// Resolve the kind, the integration, and the shell line — all locally.
fn preflight(req: &StartRequest<'_>) -> Result<Plan, Refusal> {
    if !is_addressable_name(req.name) {
        return Err(Refusal::new(
            codes::INVALID_AGENT_NAME,
            format!("'{}' is not an addressable agent name", req.name),
            format!(
                "names this verb binds must match {NAME_GRAMMAR} so `%{}` can address the \
                 pane afterwards; `phux agent set` accepts any non-empty name",
                req.name
            ),
            EXIT_USAGE,
        ));
    }

    // The most important refusal in the verb. A kind with no loaded manifest
    // is never identified at all, so the detector publishes nothing, forever
    // — readiness is unreachable and the verb would spend its whole timeout
    // *after* having typed a command into the pane and bound a name to it. The
    // agent starts fine; phux just can never say so. Refusing up front is free
    // and pre-write, and it draws the line in the right place: `launch` and
    // `spawn` keep working for any agent whatsoever, because neither makes a
    // readiness promise. This verb is the one that promises, so it is the one
    // that has to check it can keep the promise.
    let loaded = agent_explain::kinds();
    let kind = if req.no_wait {
        agent_explain::resolve_kind(req.kind).unwrap_or_else(|| req.kind.to_owned())
    } else if loaded.is_empty() {
        return Err(Refusal::new(
            codes::AGENT_DETECTION_UNAVAILABLE,
            "no agent detection manifests are loaded, so readiness can never be observed",
            "PHUX_AGENT_DETECT=0 disables detection entirely, and a platform with no \
             process introspection never identifies an agent; use `phux launch` / `phux \
             spawn`, which make no readiness promise, or `phux agent start --no-wait`",
            EXIT_USAGE,
        ));
    } else {
        agent_explain::resolve_kind(req.kind).ok_or_else(|| {
            Refusal::new(
                codes::UNSUPPORTED_AGENT_KIND,
                format!(
                    "no detection manifest for kind '{}', so this verb has no readiness \
                     contract to enforce",
                    req.kind
                ),
                format!(
                    "loaded kinds: {}; add one under $PHUX_AGENT_RULES_DIR, or use \
                     `--no-wait` to submit without a readiness claim (`phux launch` and \
                     `phux spawn` make none either)",
                    loaded.join(", ")
                ),
                EXIT_USAGE,
            )
        })?
    };

    // The integration id and the manifest kind are different namespaces: a
    // template's own `kind` is a category (`terminal-agent`) and its
    // `[agent_identity] kind` is not surfaced by the config loader, so there
    // is no client-side map from one to the other. Defaulting to the slug
    // covers the templates that name themselves after the agent and the
    // override covers the rest, rather than guessing.
    let integration_id = req.integration.unwrap_or(req.kind).to_owned();
    let workspace_cwd = std::env::current_dir().map_err(|err| {
        Refusal::new(
            json_err::codes::INTERNAL_ERROR,
            format!("could not read the current directory: {err}"),
            "run from a directory that exists",
            EXIT_FAILURE,
        )
    })?;
    let config_path = phux_config::loader::config_path();
    let resolved =
        phux_plugin::resolve_launch(&config_path, &integration_id, req.args, &workspace_cwd)
            .map_err(|err| {
                Refusal::new(
                    codes::UNKNOWN_INTEGRATION,
                    format!("could not resolve integration '{integration_id}': {err}"),
                    "`phux launch --list` enumerates launchable integrations; pass \
                     `--integration ID` when it is not spelled like the kind slug",
                    EXIT_USAGE,
                )
            })?;
    let prepared = super::prepare_for_launch(&resolved).map_err(|err| {
        Refusal::new(
            codes::UNKNOWN_INTEGRATION,
            format!("could not prepare agent session for '{integration_id}': {err}"),
            "check the integration's `[session_identity]` block",
            EXIT_FAILURE,
        )
    })?;
    let argv = prepared
        .as_ref()
        .map_or(&resolved.argv, |session| &session.argv);
    let env = prepared
        .as_ref()
        .map_or_else(BTreeMap::new, |session| session.env.clone());
    let line = shell_line(argv, &env).map_err(|err| shell_line_refusal(&integration_id, &err))?;

    Ok(Plan {
        kind,
        line,
        resolved_cwd: resolved.cwd.clone(),
        integration_id,
        timeout: Duration::from_secs(req.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS)),
    })
}

/// Render a [`ShellLineError`] as its refusal.
fn shell_line_refusal(integration_id: &str, err: &ShellLineError) -> Refusal {
    let (message, remedy) = match err {
        ShellLineError::Empty => (
            format!("integration '{integration_id}' resolved to an empty launch command"),
            "the template's `[launch] command` must name a program".to_owned(),
        ),
        ShellLineError::Control { index, element } => (
            format!(
                "integration '{integration_id}' argv element {index} ('{element}') carries a \
                 control character and cannot be typed as one shell word"
            ),
            "a newline in a typed command line is a second command; fix the template, or \
             use `phux launch`, which spawns argv directly and needs no shell"
                .to_owned(),
        ),
        ShellLineError::EnvName(name) => (
            format!(
                "integration '{integration_id}' would export '{name}', which is not a shell \
                 identifier"
            ),
            "environment names must match [A-Za-z_][A-Za-z0-9_]*".to_owned(),
        ),
        ShellLineError::TooLong { bytes } => (
            format!(
                "integration '{integration_id}' resolves to a {bytes}-byte command line, over \
                 the {MAX_SHELL_LINE}-byte limit"
            ),
            "shorten the template's argv or the trailing `-- ARGS`".to_owned(),
        ),
    };
    Refusal::new(codes::INVALID_LAUNCH_ARGV, message, remedy, EXIT_USAGE)
}

/// The whole server-facing half, once the plan exists.
#[allow(
    clippy::future_not_send,
    reason = "inherited from `wait_for_agent_state`, whose two halves share \
              one EdgeTracker through a RefCell; ADR-0003 binds the CLI to a \
              current-thread runtime, so nothing here is ever polled across \
              threads"
)]
async fn drive(
    req: &StartRequest<'_>,
    plan: &Plan,
    selector: &crate::selector::Selector,
    socket_path: &Path,
) -> ExitCode {
    let terminal = match resolve_target(socket_path, selector, "agent start", req.json).await {
        Ok(id) => id,
        Err(code) => return code,
    };
    // APPLY_INPUT is local-only (ADR-0076 point 1) and `phux.agent/v1` does
    // not federate, so a satellite target would be a write phux cannot make
    // and a readiness claim phux cannot observe.
    if !matches!(terminal, TerminalId::Local { .. }) {
        return emit(
            req.json,
            &Refusal::new(
                json_err::codes::SATELLITE_TARGET,
                format!(
                    "{} is a satellite pane; acknowledged input and agent metadata are \
                     hub-local",
                    crate::selector::format_terminal_id(&terminal)
                ),
                "run `phux agent start` against the satellite's own server",
                EXIT_USAGE,
            ),
        );
    }

    if let Err(refusal) = check_preconditions(req, plan, &terminal, socket_path).await {
        return emit(req.json, &refusal);
    }

    let mut conn = match Connection::connect(socket_path).await {
        Ok(conn) => conn,
        Err(err) => return json_err::report_no_server(req.json, &err, socket_path, "agent start"),
    };
    if let Err(refusal) = acknowledged_input_available(&conn) {
        return emit(req.json, &refusal);
    }
    match recheck_occupant(&mut conn, &terminal).await {
        Ok(()) => {}
        Err(Ok(refusal)) => return emit(req.json, &refusal),
        Err(Err(err)) => {
            return json_err::report_no_server(req.json, &err, socket_path, "agent start");
        }
    }

    // THE BIND. Name only, never `kind`: the server fills `kind` only when the
    // stored record has none, so pre-declaring `--kind claude` on a pane that
    // actually starts `codex` would leave a permanently wrong kind blessed by
    // the detector and undetectable as a mismatch. Binding name only makes the
    // record's `kind` the detector's kernel-derived answer, which is precisely
    // what the readiness check compares against.
    let bound = AgentRecord {
        name: req.name.to_owned(),
        ..AgentRecord::default()
    };
    let bound_bytes = bound.encode();
    if let Err(refusal) = bind_name(&mut conn, &terminal, &bound_bytes).await {
        return emit(req.json, &refusal);
    }

    let started = Instant::now();
    let events = submit_events(&plan.line);
    let operation_id = new_operation_id();
    if let Err(failure) = apply_input(&mut conn, &terminal, operation_id, events).await {
        // Rolling a name back is only safe when we know nothing was typed.
        // Once bytes are on the PTY, deleting the name would leave a running
        // agent with no handle — strictly worse than a name pointing at a pane
        // whose state says `unknown`, which ADR-0075 point 7 already refuses
        // input to.
        if failure.wrote_nothing {
            rollback_bind(&mut conn, &terminal, &bound_bytes).await;
        }
        return emit(req.json, &failure.refusal);
    }

    if req.no_wait {
        return report_submitted(req, plan, &terminal);
    }

    let outcome = wait_for_agent_state(
        socket_path,
        &terminal,
        READY_STATES,
        Some(plan.timeout),
        POLL_INTERVAL,
    )
    .await;
    let latency = started.elapsed();
    match outcome {
        Ok(result) if result.satisfied() => {
            report_ready(req, plan, &terminal, &result, latency, socket_path).await
        }
        Ok(result) => report_timeout(req, plan, &terminal, &result),
        Err(err) => emit(req.json, &wait_refusal(&terminal, err)),
    }
}

/// ADR-0076 point 1: an older server is refused, never downgraded to
/// fire-and-forget `ROUTE_INPUT`. A launch command is not idempotent at the
/// receiver, and its only fire-and-forget recovery is the duplicate.
fn acknowledged_input_available(conn: &Connection) -> Result<(), Refusal> {
    if conn.negotiated_bootstrap().is_some_and(|bootstrap| {
        bootstrap
            .server_features
            .contains(ServerFeature::AcknowledgedInput)
    }) {
        return Ok(());
    }
    Err(Refusal::new(
        json_err::codes::SERVER_TOO_OLD,
        "this server does not advertise ACKNOWLEDGED_INPUT, so the launch line cannot be \
         delivered with a receipt",
        "upgrade the server (`phux update`), or start the agent by hand in the pane",
        EXIT_USAGE,
    ))
}

/// Re-read the pane's occupant on the connection that is about to write.
///
/// The occupancy check in [`check_preconditions`] rode a different connection,
/// so it carries no ordering guarantee against the bind. The server handles one
/// connection's frames in arrival order, so nothing this client sends can
/// interleave between this answer and the `SET_METADATA` that follows. That is
/// the same bound `phux agent send-keys` documents — one server frame-handling
/// turn, not atomicity — and it closes the window that actually bites: the
/// seconds between reading the fleet and typing into a pane.
///
/// `Err(Ok(_))` is a refusal, `Err(Err(_))` a transport failure; the caller
/// reports them on different channels.
async fn recheck_occupant(
    conn: &mut Connection,
    terminal: &TerminalId,
) -> Result<(), Result<Refusal, AttachError>> {
    match read_record(conn, terminal, 90).await {
        Ok(Answer::Ok(Some(occupant))) if occupant.state != AgentMetaState::Unknown => {
            Err(Ok(Refusal::new(
                codes::AGENT_PANE_BUSY,
                format!(
                    "{} began hosting '{}' ({}) while the preconditions were being checked; \
                     nothing was typed",
                    crate::selector::format_terminal_id(terminal),
                    occupant.name,
                    occupant.state.as_str()
                ),
                "re-run against another pane, or clear the record with `phux agent clear`",
                EXIT_USAGE,
            )))
        }
        Ok(_) => Ok(()),
        Err(err) => Err(Err(err)),
    }
}

/// Emit a refusal on the verb's own channels.
fn emit(json: bool, refusal: &Refusal) -> ExitCode {
    json_err::emit(json, &refusal.err, refusal.exit)
}

/// The read-only preconditions: name uniqueness, pane occupancy, cwd
/// agreement, and the available-shell check.
async fn check_preconditions(
    req: &StartRequest<'_>,
    plan: &Plan,
    terminal: &TerminalId,
    socket_path: &Path,
) -> Result<(), Refusal> {
    // Fail CLOSED. A precondition that cannot be evaluated is not a
    // precondition that passed: skipping the block on a read failure would
    // turn a mid-flight disconnect into a launch command typed into an
    // unexamined pane, which is the one outcome this whole section exists to
    // prevent.
    let Ok((snapshot, _degradation)) = super::fetch_snapshot(socket_path, "agent start").await
    else {
        return Err(Refusal::new(
            json_err::codes::TRANSPORT,
            "could not read the session state, so the target pane's preconditions cannot be \
             checked",
            "nothing was written; retry, or run `phux doctor` for a health check",
            EXIT_FAILURE,
        ));
    };
    let index = super::fetch_agent_index(socket_path, &snapshot).await;

    check_name_free(&index, terminal, req.name)?;
    check_pane_free(&index, terminal)?;
    check_cwd_agreement(&snapshot, terminal, plan)?;
    if req.force {
        return Ok(());
    }
    check_shell_available(socket_path, terminal).await
}

/// ADR-0075 point 4: uniqueness is advisory and checked at resolve, but a verb
/// that BINDS a name should refuse to create the ambiguity in the first place.
fn check_name_free(
    index: &std::collections::HashMap<TerminalId, AgentRecord>,
    terminal: &TerminalId,
    name: &str,
) -> Result<(), Refusal> {
    let holders: Vec<String> = index
        .iter()
        .filter(|(id, record)| {
            *id != terminal && record.name.trim().eq_ignore_ascii_case(name.trim())
        })
        .map(|(id, _)| crate::selector::format_terminal_id(id))
        .collect();
    if holders.is_empty() {
        return Ok(());
    }
    Err(Refusal::new(
        codes::AGENT_NAME_TAKEN,
        format!("'{name}' already names {}", holders.join(", ")),
        format!(
            "clear it with `phux agent clear {}`, or choose another name — `%{name}` must \
             resolve to exactly one pane",
            holders.first().map_or("@N", String::as_str),
        ),
        EXIT_USAGE,
    ))
}

/// `agent start` starts an agent; it does not adopt one.
fn check_pane_free(
    index: &std::collections::HashMap<TerminalId, AgentRecord>,
    terminal: &TerminalId,
) -> Result<(), Refusal> {
    let Some(occupant) = index.get(terminal) else {
        return Ok(());
    };
    if occupant.state == AgentMetaState::Unknown {
        return Ok(());
    }
    Err(Refusal::new(
        codes::AGENT_PANE_BUSY,
        format!(
            "{} already hosts '{}'{} ({})",
            crate::selector::format_terminal_id(terminal),
            occupant.name,
            occupant
                .kind
                .as_deref()
                .map_or_else(String::new, |kind| format!(" of kind '{kind}'")),
            occupant.state.as_str()
        ),
        "`agent start` starts an agent; it does not adopt one. Clear the record with \
         `phux agent clear`, or pick another pane",
        EXIT_USAGE,
    ))
}

/// The pane's shell is already somewhere, and this verb will not `cd` it.
///
/// Silently moving a human's shell is a side effect that outlives the verb,
/// and a `working_directory = "plugin-root"` template would land it inside a
/// plugin tree after the agent exits.
fn check_cwd_agreement(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    terminal: &TerminalId,
    plan: &Plan,
) -> Result<(), Refusal> {
    let Some(pane_cwd) = snapshot
        .panes
        .iter()
        .find(|pane| pane.id == *terminal)
        .and_then(|pane| pane.cwd.as_deref())
    else {
        return Ok(());
    };
    if Path::new(pane_cwd) == plan.resolved_cwd {
        return Ok(());
    }
    Err(Refusal::new(
        codes::AGENT_CWD_MISMATCH,
        format!(
            "integration '{}' wants to run in {} but {} is in {pane_cwd}",
            plan.integration_id,
            plan.resolved_cwd.display(),
            crate::selector::format_terminal_id(terminal),
        ),
        "`agent start` never `cd`s someone's shell; run it from that directory, or use \
         `phux launch`, which spawns its own pane with the right cwd",
        EXIT_USAGE,
    ))
}

/// The available-shell precondition, degraded to what a client can see.
///
/// Fails CLOSED: a screen phux could not read is [`ShellCheck::Unanswerable`],
/// which refuses, because "I could not look" is not "it was fine".
async fn check_shell_available(socket_path: &Path, terminal: &TerminalId) -> Result<(), Refusal> {
    let screen =
        phux_client::snapshot::get_screen_scrollback(socket_path, terminal.clone(), None, true)
            .await;
    let label = crate::selector::format_terminal_id(terminal);
    match screen
        .as_ref()
        .map_or(ShellCheck::Unanswerable, shell_check)
    {
        ShellCheck::AtPrompt => Ok(()),
        ShellCheck::NotAtPrompt => Err(Refusal::new(
            codes::AGENT_PANE_NOT_AVAILABLE,
            format!(
                "{label} is not sitting at its shell prompt — the cursor is not on a marked \
                 command line, so something else has the screen"
            ),
            "wait for the foreground job to finish, pick another pane, or pass `--force` if \
             you know the pane is free",
            EXIT_USAGE,
        )),
        ShellCheck::Unanswerable => Err(Refusal::new(
            codes::AGENT_PANE_NOT_AVAILABLE,
            format!(
                "cannot establish that {label} is at an idle shell prompt: the pane reports \
                 no OSC-133 shell-integration marks"
            ),
            "phux refuses to type a launch command into a pane it cannot see the state of. \
             Enable your shell's OSC-133 integration, use `phux launch` (which spawns its \
             own pane and needs no precondition), or pass `--force`",
            EXIT_USAGE,
        )),
    }
}

/// Write the `phux.agent/v1` bind and confirm it landed.
///
/// The trailing `GET` is load-bearing exactly as in `phux agent set`:
/// `SET_METADATA` has no reply frame, so without a round trip the process
/// could submit input before the server had even read the write.
async fn bind_name(
    conn: &mut Connection,
    terminal: &TerminalId,
    value: &[u8],
) -> Result<(), Refusal> {
    let transport = |err: &AttachError| {
        Refusal::new(
            json_err::codes::TRANSPORT,
            format!("could not bind the agent name: {err}"),
            "run `phux doctor` for a health check",
            EXIT_FAILURE,
        )
    };
    conn.send(&FrameKind::SetMetadata {
        request_id: 100,
        scope: Scope::Terminal(terminal.clone()),
        key: TERMINAL_AGENT_KEY.to_owned(),
        value: value.to_vec(),
    })
    .await
    .map_err(|err| transport(&err))?;
    match read_record(conn, terminal, 101).await {
        Ok(Answer::Ok(Some(_))) => Ok(()),
        Ok(Answer::Ok(None)) => Err(Refusal::new(
            codes::AGENT_START_FAILED,
            "the agent name did not persist, so nothing was started",
            "nothing was typed into the pane; check the server log and retry",
            EXIT_FAILURE,
        )),
        Ok(Answer::Err(refusal)) => Err(Refusal::new(
            codes::AGENT_START_FAILED,
            format!("the agent name could not be confirmed: {refusal}"),
            "nothing was typed into the pane; the write may or may not have landed — \
             `phux agent show` says which",
            EXIT_FAILURE,
        )),
        Err(err) => Err(transport(&err)),
    }
}

/// Undo the bind, but only after proving nothing else changed it.
///
/// Read-compare-delete rather than a bare delete: L3 has no compare-and-swap
/// and `SET_METADATA` is whole-record last-writer-wins, so different bytes
/// mean either the detector already filled `state` (the agent *did* start and
/// our observation failed, not the start) or a third party wrote. In both
/// cases the right move is to leave them and say so. The race window is one
/// round trip and cannot be closed without a wire change; reporting it beats
/// pretending.
async fn rollback_bind(conn: &mut Connection, terminal: &TerminalId, expected: &[u8]) {
    match read_record(conn, terminal, 200).await {
        Ok(Answer::Ok(Some(record))) if record.encode() == expected => {
            if let Err(err) = conn
                .send(&FrameKind::DeleteMetadata {
                    request_id: 201,
                    scope: Scope::Terminal(terminal.clone()),
                    key: TERMINAL_AGENT_KEY.to_owned(),
                })
                .await
            {
                eprintln!("phux: warning: could not release the bound agent name: {err}");
            }
        }
        Ok(Answer::Ok(Some(_))) => {
            eprintln!(
                "phux: warning: the agent record changed while the launch was being refused, \
                 so the name was left bound; `phux agent show` reads it, `phux agent clear` \
                 releases it"
            );
        }
        Ok(Answer::Ok(None)) => {}
        Ok(Answer::Err(refusal)) => {
            eprintln!("phux: warning: could not read the bound agent name back: {refusal}");
        }
        Err(err) => {
            eprintln!("phux: warning: could not read the bound agent name back: {err}");
        }
    }
}

/// One `GET_METADATA` round trip for the pane's agent record.
async fn read_record(
    conn: &mut Connection,
    terminal: &TerminalId,
    request_id: u32,
) -> Result<Answer<Option<AgentRecord>>, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Terminal(terminal.clone()),
            TERMINAL_AGENT_KEY.to_owned(),
        )
        .await?
        .into_parts();
    for message in phux_client::state::degradation_notices(&interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
    Ok(answer.map(|value| value.as_deref().and_then(parse_agent_record)))
}

/// The one input batch: the command line as a trusted paste, then Enter.
///
/// Built through `phux_client::send_keys::events_for` so the batching rule
/// lives in one place — a literal run immediately before `Enter` becomes one
/// trusted paste plus the real Enter key, which is exactly the ADR-0076
/// point 3 shape. Enter last means a partial delivery drops the *submission*
/// and leaves unsubmitted text on screen, the recoverable failure. No submit
/// delay: the two events are one batch encoded against one mode snapshot.
#[must_use]
fn submit_events(line: &str) -> Vec<InputEvent> {
    phux_client::send_keys::events_for(&[line.to_owned(), "Enter".to_owned()])
}

/// A fresh CSPRNG operation id (ADR-0053).
///
/// `UUIDv4` bytes: 122 bits from the OS entropy source with the version and
/// variant bits set, so the all-zero value the wire reserves is unreachable by
/// construction rather than by a retry loop.
fn new_operation_id() -> Option<InputOperationId> {
    InputOperationId::new(*uuid::Uuid::new_v4().as_bytes())
}

/// A failed submit, plus whether the pane is provably untouched.
#[derive(Debug)]
struct SubmitFailure {
    /// What to report.
    refusal: Refusal,
    /// `true` only for refusals the server makes *before* handing bytes to the
    /// PTY, which are therefore safe to unwind.
    wrote_nothing: bool,
}

/// Submit the batch under `operation_id` and map the reply onto ADR-0076
/// point 2's readings.
async fn apply_input(
    conn: &mut Connection,
    terminal: &TerminalId,
    operation_id: Option<InputOperationId>,
    events: Vec<InputEvent>,
) -> Result<(), SubmitFailure> {
    let Some(operation_id) = operation_id else {
        return Err(SubmitFailure {
            refusal: Refusal::new(
                json_err::codes::INTERNAL_ERROR,
                "could not generate an input operation id",
                "report this: a UUIDv4 always has non-zero version bits",
                EXIT_FAILURE,
            ),
            wrote_nothing: true,
        });
    };
    let reply = conn
        .request(
            110,
            Command::ApplyInput {
                operation_id,
                terminal_id: terminal.clone(),
                events,
            },
        )
        .await
        .map_err(|err| SubmitFailure {
            refusal: Refusal::new(
                json_err::codes::TRANSPORT,
                format!("could not submit the launch command: {err}"),
                "run `phux doctor` for a health check",
                EXIT_FAILURE,
            ),
            // A transport failure on the request itself may have delivered the
            // frame; treat the pane as touched rather than guessing.
            wrote_nothing: false,
        })?;
    match reply.into_result_ignoring_interleaved() {
        CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
        CommandResult::Error { code, message } => Err(submit_error(code, &message)),
        other => Err(SubmitFailure {
            refusal: Refusal::new(
                json_err::codes::TRANSPORT,
                format!("unexpected reply to APPLY_INPUT: {other:?}"),
                "run `phux doctor` for a health check",
                EXIT_FAILURE,
            ),
            wrote_nothing: false,
        }),
    }
}

/// Map one `APPLY_INPUT` refusal onto its code, exit status, and unwind
/// safety (ADR-0076 point 2).
fn submit_error(code: ErrorCode, message: &str) -> SubmitFailure {
    match code {
        // Terminal, and NOT exit 3: a same-id retry replays the cached
        // unknown and a new-id retry is the duplicate the acknowledged lane
        // exists to prevent, so "retry is correct" — the published meaning of
        // 3 — would be a lie.
        ErrorCode::InputDeliveryUnknown => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_UNKNOWN,
                format!("whether the launch command reached the pane is unknown: {message}"),
                "do NOT retry under a new operation id — that is the duplicate this path \
                 prevents. Inspect the pane (`phux snapshot`), then `phux agent clear` the \
                 name if nothing started",
                EXIT_FAILURE,
            ),
            wrote_nothing: false,
        },
        ErrorCode::InputLeaseHeld => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("another client holds the input lease on this pane: {message}"),
                "nothing was typed; retry once the lease is released",
                EXIT_USAGE,
            ),
            wrote_nothing: true,
        },
        ErrorCode::CanonicalLimitExceeded => SubmitFailure {
            refusal: Refusal::new(
                codes::INVALID_LAUNCH_ARGV,
                format!("the launch command exceeds the acknowledged-input limits: {message}"),
                "nothing was typed; shorten the integration's argv or its `-- ARGS`",
                EXIT_USAGE,
            ),
            wrote_nothing: true,
        },
        ErrorCode::ResourceExhausted => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("the acknowledged input lane is busy: {message}"),
                "nothing was typed; the lane is server-wide and single — back off and \
                 retry, unchanged, under the same operation id",
                EXIT_FAILURE,
            ),
            wrote_nothing: true,
        },
        _ => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("the launch command was refused: {message}"),
                "run `phux doctor` for a health check",
                EXIT_FAILURE,
            ),
            wrote_nothing: false,
        },
    }
}

/// Turn a wait error into its refusal.
fn wait_refusal(terminal: &TerminalId, err: AgentWaitError) -> Refusal {
    let label = crate::selector::format_terminal_id(terminal);
    match err {
        // Unreachable in practice: this verb binds the record itself before
        // subscribing. It stays an error rather than an unwrap.
        AgentWaitError::NoRecord => Refusal::new(
            codes::AGENT_START_FAILED,
            format!("{label}'s agent record vanished between the bind and the wait"),
            "the launch command was typed; inspect the pane with `phux snapshot`",
            EXIT_FAILURE,
        ),
        AgentWaitError::Departed { from, reason, .. } => Refusal::new(
            codes::AGENT_DEPARTED,
            format!(
                "{label}'s agent record went away while starting (from '{}': {})",
                from.as_str(),
                reason.as_str()
            ),
            "a departure is not a start; the command was typed, so inspect the pane before \
             retrying",
            EXIT_FAILURE,
        ),
        AgentWaitError::Transport(err) => Refusal::new(
            json_err::codes::TRANSPORT,
            format!("could not observe readiness: {err}"),
            "the launch command was typed; `phux agent show` reads the pane's current state",
            EXIT_FAILURE,
        ),
    }
}

/// `--no-wait`: the honest escape hatch. Submitted, readiness unclaimed.
fn report_submitted(req: &StartRequest<'_>, plan: &Plan, terminal: &TerminalId) -> ExitCode {
    let label = crate::selector::format_terminal_id(terminal);
    if req.json {
        let document = serde_json::json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "terminal": label,
            "name": req.name,
            "kind": plan.kind,
            "integration": plan.integration_id,
            "started": true,
            "ready": false,
            "readiness": serde_json::Value::Null,
        });
        return render(&document);
    }
    outln!("{label}\t{}\t{}\tsubmitted", req.name, plan.kind);
    ExitCode::from(EXIT_SUCCESS)
}

/// The success path: a qualifying publication arrived.
async fn report_ready(
    req: &StartRequest<'_>,
    plan: &Plan,
    terminal: &TerminalId,
    result: &AgentWaitResult,
    latency: Duration,
    socket_path: &Path,
) -> ExitCode {
    let label = crate::selector::format_terminal_id(terminal);
    let record = result.record.as_ref();
    let observed_kind = record.and_then(|rec| rec.kind.as_deref()).unwrap_or("");
    match kind_verdict(record, &plan.kind) {
        KindVerdict::Mismatch => {
            return emit(
                req.json,
                &Refusal::new(
                    codes::AGENT_KIND_MISMATCH,
                    format!(
                        "{label} started '{observed_kind}', not '{}' — the kernel says a \
                         different binary's process group owns the pane",
                        plan.kind
                    ),
                    format!(
                        "the name stays bound to the pane that is actually running; \
                         `phux agent explain {label}` shows the evidence, `phux agent clear \
                         {label}` releases the name"
                    ),
                    EXIT_FAILURE,
                ),
            );
        }
        KindVerdict::Unconfirmed if !req.json => {
            eprintln!(
                "phux: warning: {label}'s record carries no kind, so nothing confirmed the \
                 occupant is '{}' (an explicit hook writer may own this record and stand \
                 the detector down)",
                plan.kind
            );
        }
        KindVerdict::Confirmed | KindVerdict::Unconfirmed => {}
    }

    let readiness = provenance(socket_path, terminal, &plan.kind).await;
    if req.json {
        let document = serde_json::json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "terminal": label,
            "name": req.name,
            "kind": plan.kind,
            "integration": plan.integration_id,
            "started": true,
            "ready": true,
            "state": result.last.as_str(),
            "shell_check": if req.force { "skipped" } else { ShellCheck::AtPrompt.as_str() },
            "readiness": readiness_document(
                result,
                readiness.as_ref(),
                kind_verdict(record, &plan.kind),
                latency,
            ),
        });
        return render(&document);
    }
    outln!(
        "{label}\t{}\t{}\t{}\tready in {}ms",
        req.name,
        plan.kind,
        result.last.as_str(),
        latency.as_millis()
    );
    ExitCode::from(EXIT_SUCCESS)
}

/// The `readiness` sub-document: provenance, not a word.
///
/// Every field says something a caller can act on. `manifest_asserts_idle` is
/// the load-bearing one: it is `false` for all five shipped manifests, which
/// means a `state: "idle"` readiness is *absence of contrary evidence*, not a
/// positive observation — and the caller can decide whether that is good
/// enough. The evaluation is client-side, against a screen read a moment after
/// the server's, so it can legitimately disagree with the server's own trace.
fn readiness_document(
    result: &AgentWaitResult,
    explanation: Option<&Explanation>,
    verdict: KindVerdict,
    latency: Duration,
) -> serde_json::Value {
    serde_json::json!({
        "identity": match verdict {
            KindVerdict::Confirmed => "kernel-foreground-pgid",
            KindVerdict::Unconfirmed => "unconfirmed",
            KindVerdict::Mismatch => "mismatch",
        },
        "transition": result.edge.map(|edge| serde_json::json!({
            "from": edge.from.as_str(),
            "to": edge.to.as_str(),
            "via": edge.via.as_str(),
        })),
        "state_source": explanation.map(state_source),
        "matched_rule": explanation.and_then(|ex| ex.matched_rule.clone()),
        "fallback_reason": explanation.and_then(|ex| ex.fallback_reason.clone()),
        "manifest_asserts_idle": explanation.map(|ex| {
            ex.evaluated_rules.iter().any(|rule| rule.visible_idle)
        }),
        "evaluated_client_side": explanation.is_some(),
        "latency_ms": u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
        "observations": {
            "edges": result.edges,
            "pushes": result.pushes,
            "polls": result.polls,
        },
    })
}

/// Re-run the same manifest the server just ran, client-side, so the answer
/// carries evidence rather than a word.
///
/// Best effort: this runs *after* readiness has already been established, so a
/// failure here costs one optional sub-document rather than the result.
async fn provenance(socket_path: &Path, terminal: &TerminalId, kind: &str) -> Option<Explanation> {
    let screen =
        phux_client::snapshot::get_screen_scrollback(socket_path, terminal.clone(), None, false)
            .await
            .ok()?;
    let title = super::fetch_snapshot(socket_path, "agent start")
        .await
        .ok()
        .and_then(|(snapshot, _)| {
            snapshot
                .panes
                .iter()
                .find(|pane| pane.id == *terminal)
                .and_then(|pane| pane.title.clone())
        })
        .unwrap_or_default();
    agent_explain::explain(
        kind,
        &agent_explain::Capture {
            title,
            lines: screen.lines,
        },
    )
}

/// The deadline expired with no qualifying publication.
fn report_timeout(
    req: &StartRequest<'_>,
    plan: &Plan,
    terminal: &TerminalId,
    result: &AgentWaitResult,
) -> ExitCode {
    let label = crate::selector::format_terminal_id(terminal);
    emit(
        req.json,
        &Refusal::new(
            codes::AGENT_START_TIMEOUT,
            format!(
                "{label} never published a derived agent state after the launch command was \
                 typed (last seen '{}', {} pushes, {} polls)",
                result.last.as_str(),
                result.pushes,
                result.polls
            ),
            format!(
                "the command WAS typed and the name IS bound — this is a failure to observe, \
                 not proof nothing started. `phux agent explain {label}` shows what the '{}' \
                 manifest makes of the screen; raise `--timeout`, or release the name with \
                 `phux agent clear {label}`",
                plan.kind
            ),
            EXIT_WAIT_TIMEOUT,
        ),
    )
}

/// Render one result document on stdout.
fn render(document: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(document) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::from(EXIT_SUCCESS)
        }
        Err(err) => json_err::emit(
            true,
            &json_err::CliError::new(
                json_err::codes::JSON_SERIALIZE,
                format!("could not render agent start JSON: {err}"),
                "report this: a document of strings and numbers cannot fail to serialize",
            ),
            EXIT_FAILURE,
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]

    use phux_core::screen::{CellInfo, CellStyle, CursorState};
    use phux_protocol::input::paste::PasteTrust;

    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn cell(row: u16, col: u16, semantic: Option<SemanticContent>) -> CellInfo {
        CellInfo {
            col,
            row,
            semantic,
            style: CellStyle::default(),
        }
    }

    fn cursor_at(y: u16) -> CursorState {
        CursorState {
            x: 2,
            y,
            visible: true,
        }
    }

    fn screen(cursor: Option<CursorState>, cells: Option<Vec<CellInfo>>) -> ScreenState {
        ScreenState {
            schema_version: 1,
            pane: 7,
            cols: 80,
            rows: 24,
            cursor,
            lines: vec![String::new(); 24],
            scrollback: Vec::new(),
            cells,
            soft_wrap: None,
            truncated: false,
            truncated_reason: None,
            title: None,
        }
    }

    /// The grammar this verb binds is exactly the one `%name` can address:
    /// binding a name its own success cannot address would be dishonest.
    #[test]
    fn bound_names_are_addressable_names() {
        for name in ["build", "a", "rev-2", "with_underscore", &"x".repeat(32)] {
            assert!(is_addressable_name(name), "{name} must be addressable");
        }
        for name in ["", "Build", "2fast", "-lead", "has space", &"x".repeat(33)] {
            assert!(!is_addressable_name(name), "{name} must be refused");
        }
    }

    /// Every word is quoted, and an embedded single quote is escaped rather
    /// than terminating the quoting.
    #[test]
    fn shell_quote_is_unconditional_and_closes_over_single_quotes() {
        assert_eq!(shell_quote("claude"), "'claude'");
        assert_eq!(
            shell_quote("/opt/my agent/claude"),
            "'/opt/my agent/claude'"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        // The injection this exists to stop: nothing escapes the quotes.
        assert_eq!(shell_quote("; rm -rf /"), "'; rm -rf /'");
        assert_eq!(shell_quote("$(whoami)"), "'$(whoami)'");
    }

    /// Env rides `env NAME=value` rather than a `VAR=v cmd` prefix, because
    /// that prefix is not valid fish and phux does not know the pane's shell.
    #[test]
    fn env_is_delivered_through_env_not_a_prefix() {
        let mut env = BTreeMap::new();
        env.insert("PHUX_CLAUDE_SESSION_ID".to_owned(), "abc-123".to_owned());
        let line = shell_line(&argv(&["/abs/claude", "--resume", "abc-123"]), &env)
            .expect("a clean argv must assemble");
        assert_eq!(
            line,
            "env 'PHUX_CLAUDE_SESSION_ID=abc-123' '/abs/claude' '--resume' 'abc-123'"
        );
        assert!(!line.starts_with("PHUX_"), "no VAR=v prefix form: {line}");
    }

    /// With no env there is no `env` word at all — the line is just the
    /// quoted argv.
    #[test]
    fn an_empty_env_adds_no_env_word() {
        let line =
            shell_line(&argv(&["codex"]), &BTreeMap::new()).expect("a bare argv must assemble");
        assert_eq!(line, "'codex'");
    }

    /// A newline in an argv element would be a second command, and quoting
    /// cannot contain it. Refused before anything is typed.
    #[test]
    fn a_control_character_in_argv_is_refused_not_quoted() {
        let err = shell_line(&argv(&["claude", "--flag\nrm -rf /"]), &BTreeMap::new())
            .expect_err("a newline must be refused");
        assert_eq!(
            err,
            ShellLineError::Control {
                index: 1,
                element: "--flag\nrm -rf /".to_owned(),
            }
        );
        assert!(shell_line(&argv(&["cl\u{0}aude"]), &BTreeMap::new()).is_err());
    }

    /// An empty argv is a template bug, not a launch.
    #[test]
    fn an_empty_argv_is_refused() {
        assert_eq!(
            shell_line(&[], &BTreeMap::new()).expect_err("empty argv"),
            ShellLineError::Empty
        );
    }

    /// The line is bounded well inside ADR-0053's command-body cap, so the
    /// batch limit can never be the thing that fails.
    #[test]
    fn an_oversized_line_is_refused_below_the_apply_input_cap() {
        // The bound-vs-cap relation is a compile-time `const _` assertion at
        // the top of this module; this pins the runtime refusal.
        let long = "x".repeat(MAX_SHELL_LINE);
        let err = shell_line(&argv(&["claude", &long]), &BTreeMap::new())
            .expect_err("an oversized line must be refused");
        assert!(matches!(err, ShellLineError::TooLong { .. }), "{err:?}");
    }

    /// An environment name that is not a shell identifier is refused rather
    /// than emitted into a line the shell would reinterpret.
    #[test]
    fn a_non_identifier_env_name_is_refused() {
        let mut env = BTreeMap::new();
        env.insert("NOT AN IDENT".to_owned(), "x".to_owned());
        assert_eq!(
            shell_line(&argv(&["claude"]), &env).expect_err("bad env name"),
            ShellLineError::EnvName("NOT AN IDENT".to_owned())
        );
    }

    /// The precondition reads the CURSOR'S row, not the whole viewport: a
    /// prompt mark left further up the screen is equally true while a build
    /// runs in the foreground, which is the case this check exists to catch.
    #[test]
    fn the_shell_check_reads_the_cursor_row_not_the_whole_screen() {
        let at_prompt = screen(
            Some(cursor_at(10)),
            Some(vec![cell(10, 0, Some(SemanticContent::Prompt))]),
        );
        assert_eq!(shell_check(&at_prompt), ShellCheck::AtPrompt);

        // Same marks, cursor elsewhere: something else has the screen.
        let busy = screen(
            Some(cursor_at(10)),
            Some(vec![cell(3, 0, Some(SemanticContent::Prompt))]),
        );
        assert_eq!(shell_check(&busy), ShellCheck::NotAtPrompt);
    }

    /// No marks at all is an ADMISSION, not a refusal reason of the same
    /// kind: shell integration is probably off and phux cannot answer. The
    /// two must stay distinguishable, because only one of them is evidence.
    #[test]
    fn a_screen_without_semantic_marks_is_unanswerable() {
        assert_eq!(
            shell_check(&screen(Some(cursor_at(0)), None)),
            ShellCheck::Unanswerable
        );
        assert_eq!(
            shell_check(&screen(Some(cursor_at(0)), Some(Vec::new()))),
            ShellCheck::Unanswerable
        );
        // Styled cells with no semantic mark carry no prompt evidence either.
        assert_eq!(
            shell_check(&screen(Some(cursor_at(0)), Some(vec![cell(0, 0, None)]))),
            ShellCheck::Unanswerable
        );
        // Marks but no resolvable cursor: the question has no anchor.
        assert_eq!(
            shell_check(&screen(
                None,
                Some(vec![cell(0, 0, Some(SemanticContent::Input))])
            )),
            ShellCheck::Unanswerable
        );
    }

    /// Readiness is every DERIVED state, not `idle`. An agent that starts
    /// straight into `blocked` (a trust-this-folder prompt) is ready for
    /// interactive input, which is what the verb promises; `unknown` is the
    /// level the bind writes and is never a target.
    #[test]
    fn readiness_accepts_every_derived_state_and_never_unknown() {
        assert_eq!(
            READY_STATES,
            &[
                AgentMetaState::Idle,
                AgentMetaState::Working,
                AgentMetaState::Blocked,
                AgentMetaState::Done
            ]
        );
        assert!(!READY_STATES.contains(&AgentMetaState::Unknown));
    }

    /// The kind comparison is what turns "something started" into "the thing
    /// I asked for started". An absent kind is reported, not failed: an
    /// explicit hook writer may own the record and stand the detector down.
    #[test]
    fn kind_verdict_separates_confirmed_absent_and_wrong() {
        let with = |kind: Option<&str>| AgentRecord {
            name: "build".to_owned(),
            kind: kind.map(str::to_owned),
            ..AgentRecord::default()
        };
        assert_eq!(
            kind_verdict(Some(&with(Some("claude"))), "claude"),
            KindVerdict::Confirmed
        );
        assert_eq!(
            kind_verdict(Some(&with(Some("CLAUDE"))), " claude "),
            KindVerdict::Confirmed
        );
        assert_eq!(
            kind_verdict(Some(&with(None)), "claude"),
            KindVerdict::Unconfirmed
        );
        assert_eq!(kind_verdict(None, "claude"), KindVerdict::Unconfirmed);
        assert_eq!(
            kind_verdict(Some(&with(Some("codex"))), "claude"),
            KindVerdict::Mismatch
        );
    }

    /// The submitted batch is exactly ADR-0076 point 3's shape: one trusted
    /// paste carrying the whole line, then the real Enter key, in that order.
    /// Enter last means a partial delivery loses the submission rather than
    /// truncating the command.
    #[test]
    fn the_submit_batch_is_one_trusted_paste_then_enter() {
        let events = submit_events("'claude' '--resume' 'x'");
        assert_eq!(events.len(), 2, "{events:?}");
        match &events[0] {
            InputEvent::Paste(paste) => {
                assert_eq!(paste.trust, PasteTrust::Trusted);
                assert_eq!(paste.data, b"'claude' '--resume' 'x'");
            }
            other => panic!("first event must be the paste, got {other:?}"),
        }
        assert!(
            matches!(events[1], InputEvent::Key(_)),
            "second event must be Enter, got {:?}",
            events[1]
        );
    }

    /// ADR-0076 point 2's readings, pinned. The distinction that matters is
    /// `wrote_nothing`: it is the only thing that makes rolling the bound
    /// name back safe, and it is false for `INPUT_DELIVERY_UNKNOWN` by
    /// definition.
    #[test]
    fn submit_errors_map_to_their_unwind_safety_and_exit() {
        let unknown = submit_error(ErrorCode::InputDeliveryUnknown, "lost");
        assert!(!unknown.wrote_nothing);
        assert_eq!(unknown.refusal.err.code, codes::AGENT_START_UNKNOWN);
        // NOT exit 3: 3 means "retry is correct", and retrying this under a
        // new id produces the duplicate the acknowledged lane prevents.
        assert_eq!(unknown.refusal.exit, EXIT_FAILURE);

        for code in [
            ErrorCode::InputLeaseHeld,
            ErrorCode::CanonicalLimitExceeded,
            ErrorCode::ResourceExhausted,
        ] {
            let failure = submit_error(code, "no");
            assert!(
                failure.wrote_nothing,
                "{code:?} is a pre-handoff refusal and must be unwindable"
            );
        }
        assert_eq!(
            submit_error(ErrorCode::InputLeaseHeld, "no").refusal.exit,
            EXIT_USAGE
        );
        assert_eq!(
            submit_error(ErrorCode::ResourceExhausted, "no")
                .refusal
                .exit,
            EXIT_FAILURE
        );
    }

    /// A kind with no compiled manifest is refused BEFORE anything is typed,
    /// and the refusal names the roster — a silent timeout just makes the
    /// verb look broken, while a visible roster is the pressure that grows it.
    #[test]
    fn an_unmanifested_kind_is_refused_before_any_write() {
        let request = StartRequest {
            name: "build",
            kind: "no-such-agent-kind",
            target: "@7",
            integration: None,
            timeout: None,
            no_wait: false,
            force: false,
            json: false,
            args: &[],
        };
        let refusal = preflight(&request).expect_err("an unmanifested kind must be refused");
        assert_eq!(refusal.err.code, codes::UNSUPPORTED_AGENT_KIND);
        assert_eq!(refusal.exit, EXIT_USAGE);
        assert!(
            refusal.err.remedy.contains("--no-wait"),
            "the refusal must not be a dead end: {}",
            refusal.err.remedy
        );
    }

    /// The name is checked before the kind, before the integration, and
    /// before anything opens a socket.
    #[test]
    fn a_bad_name_refuses_before_the_kind_is_even_resolved() {
        let request = StartRequest {
            name: "Build Bot",
            kind: "no-such-agent-kind",
            target: "@7",
            integration: None,
            timeout: None,
            no_wait: false,
            force: false,
            json: false,
            args: &[],
        };
        let refusal = preflight(&request).expect_err("a bad name must be refused");
        assert_eq!(refusal.err.code, codes::INVALID_AGENT_NAME);
    }

    /// Five manifests ship and every one of them is a legitimate `--kind`.
    /// This is the roster the refusal above prints.
    #[test]
    fn the_shipped_manifests_resolve_as_kinds() {
        let kinds = agent_explain::kinds();
        for kind in ["claude", "codex", "opencode", "pi", "omp"] {
            assert!(
                kinds.iter().any(|loaded| loaded == kind),
                "{kind} must ship a detection manifest; loaded: {kinds:?}"
            );
            assert_eq!(agent_explain::resolve_kind(kind).as_deref(), Some(kind));
        }
    }
}
