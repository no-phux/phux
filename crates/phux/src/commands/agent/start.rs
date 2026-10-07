//! `phux agent start` — start an agent **into an existing shell pane** and
//! return only once it is detected and ready for interactive input.
//!
//! Unlike `phux launch` (which spawns argv into a new pane), this types one
//! quoted command line into a pane whose child is a live shell, so it adds a
//! precondition family and shares only the resolver
//! ([`phux_plugin::resolve_launch`], [`super::prepare_for_launch`]).
//!
//! **Ready** is the first detector publication after submit: an observed
//! transition off the `unknown` this verb binds before typing, via
//! [`phux_client::agent_wait::wait_for_agent_state`]. Every derived state
//! counts, because an agent that starts into `blocked` (a trust prompt) is
//! ready for input. The pending phase lives only in this process, so a killed
//! `agent start` leaves just the inert bound name (`phux agent clear`).
//!
//! No sequence counter is needed: the bind is read back before anything is
//! typed, and the agent process cannot exist until the shell consumes the
//! Enter this verb sends, so any derived state observed is post-submit.
//!
//! The available-shell precondition reads the server-published
//! `phux.pane-occupant/v1` record, cross-checked against OSC-133 marks;
//! `--force` skips it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_agent_rules::explain::{self as agent_explain, Explanation};
use phux_client::agent_meta::{
    AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY, ShellAvailability, ShellCheck,
    pane_shell_availability, parse_agent_record,
};
use phux_client::agent_prompt::{ApplyVerdict, Refusal as ApplyRefusal, apply_input_once};
use phux_client::agent_wait::{AgentWaitError, AgentWaitResult, wait_for_agent_state};
use phux_client::attach::AttachError;
use phux_client::attach::connection::{Answer, Connection};
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{FrameKind, Scope};
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::codes;
use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};
use crate::exit_codes::{EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE, EXIT_WAIT_TIMEOUT};

/// Version of the `agent start` result document.
const RESULT_SCHEMA_VERSION: u8 = 1;

/// Poll-floor cadence for the readiness wait's `GET_METADATA` re-read — the
/// same gap `phux wait` and `phux agent wait` use.
const POLL_INTERVAL: Duration = phux_client::wait::DEFAULT_POLL_INTERVAL;

/// Default readiness deadline, in seconds. Bounded (unlike `agent wait`)
/// because success *is* a readiness claim; detector latency alone is ~8.3 s
/// worst case (5 s re-identify + 3 s grace + one tick).
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// The addressable agent-name grammar (ADR-0075 point 5), as prose for the
/// refusal message.
const NAME_GRAMMAR: &str = "^[a-z][a-z0-9_-]{0,31}$";

/// Longest shell line this verb will type, in bytes: well inside ADR-0053's
/// `APPLY_INPUT` command-body cap (asserted below), so the batch limit can
/// never fail after a name is already bound.
const MAX_SHELL_LINE: usize = 4096;

const _: () = assert!(MAX_SHELL_LINE < phux_protocol::MAX_APPLY_INPUT_COMMAND_BODY);

/// The readiness target set: every *derived* state. `unknown` is the level
/// this verb binds, so a return to it is departure, not arrival.
const READY_STATES: &[AgentMetaState] = &[
    AgentMetaState::Idle,
    AgentMetaState::Working,
    AgentMetaState::Blocked,
    AgentMetaState::Done,
];

/// One `agent start` invocation, as parsed.
#[derive(Debug)]
pub(super) struct StartRequest<'a> {
    pub(super) name: &'a str,
    pub(super) kind: &'a str,
    pub(super) target: &'a str,
    pub(super) integration: Option<&'a str>,
    pub(super) timeout: Option<u64>,
    pub(super) no_wait: bool,
    pub(super) force: bool,
    pub(super) json: bool,
    pub(super) args: &'a [String],
}

/// A refusal: the error document plus the status to exit with.
#[derive(Debug)]
struct Refusal {
    err: json_err::CliError,
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

/// Whether `name` is spellable under the addressable agent-name grammar
/// (ADR-0075 point 5), checked locally so a typo fails before any round trip.
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

/// Quote one word for a POSIX shell, unconditionally (`'` becomes `'\''`).
/// No "looks safe" fast path: template paths and native session ids may
/// carry spaces, `$(...)`, or `;`.
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
        /// Zero-based index into the argv.
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

/// Assemble the one shell line that starts the agent, every word quoted.
///
/// Environment goes through `env` rather than a `VAR=v cmd` prefix, which is
/// not valid fish. This is a shell-evaluation surface, so the refusals are
/// absolute rather than best effort.
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

/// How the started agent's identity was confirmed at readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KindVerdict {
    /// The published record's `kind` is the requested one — the kernel says
    /// that binary's process group owns the pane's tty.
    Confirmed,
    /// The record carries no `kind` (an explicit hook writer may own it,
    /// ADR-0046 point 8). Reported rather than refused.
    Unconfirmed,
    /// The record names a different kind than the one requested.
    Mismatch,
}

/// Compare the requested kind against the record readiness landed on.
#[must_use]
pub(super) fn kind_verdict(record: Option<&AgentRecord>, requested: &str) -> KindVerdict {
    let Some(kind) = record.and_then(|rec| rec.kind.as_deref()) else {
        return KindVerdict::Unconfirmed;
    };
    if phux_plugin::kind_matches(kind, requested) {
        KindVerdict::Confirmed
    } else {
        KindVerdict::Mismatch
    }
}

/// Where the state that satisfied readiness came from, in the manifest's own
/// terms, so a caller can tell a positive observation from a fail-safe.
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
        // A user manifest with no state-bearing rule publishes `idle` at grace
        // expiry: a false ready worth naming.
        return "no-rules";
    }
    "fail-safe"
}

/// `phux agent start`, over a parsed request.
pub(super) fn start(req: &StartRequest<'_>, socket: Option<PathBuf>) -> ExitCode {
    // Every cheap refusal happens locally, before the first byte is written.
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
    resolved_cwd: PathBuf,
    integration_id: String,
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

    // A kind with no loaded manifest is never identified, so readiness is
    // unreachable; refuse before typing anything rather than time out after.
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

    let config_path = phux_config::loader::config_path();
    let workspace_cwd = std::env::current_dir().map_err(|err| {
        Refusal::new(
            json_err::codes::INTERNAL_ERROR,
            format!("could not read the current directory: {err}"),
            "run from a directory that exists",
            EXIT_FAILURE,
        )
    })?;
    // Pass the canonical kind, never `req.kind` as typed: an unclaimed kind
    // falls back to an integration id spelled like the kind.
    let resolved = phux_plugin::resolve_launch_for_kind(
        &config_path,
        req.integration,
        &kind,
        req.args,
        &workspace_cwd,
    )
    .map_err(launch_refusal)?;
    let integration_id = resolved.integration_id.clone();
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

/// Render a [`phux_plugin::KindLaunchError`] as its refusal.
fn launch_refusal(err: phux_plugin::KindLaunchError) -> Refusal {
    match err {
        phux_plugin::KindLaunchError::Ambiguous { kind, claimants } => Refusal::new(
            codes::AMBIGUOUS_INTEGRATION,
            format!(
                "kind '{kind}' is claimed by more than one enabled integration: {}",
                claimants.join(", ")
            ),
            "pass `--integration ID` to say which one starts this agent",
            EXIT_USAGE,
        ),
        phux_plugin::KindLaunchError::Resolve {
            integration_id,
            source,
        } => Refusal::new(
            codes::UNKNOWN_INTEGRATION,
            format!("could not resolve integration '{integration_id}': {source}"),
            "`phux launch --list` enumerates launchable integrations; pass \
             `--integration ID`, or declare `[agent_identity] kind` in the \
             template so `--kind` resolves it by itself",
            EXIT_USAGE,
        ),
    }
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
    selector: &phux_client::selector::Selector,
    socket_path: &Path,
) -> ExitCode {
    let (terminal, mut conn) = match open_target(req, plan, selector, socket_path).await {
        Ok(opened) => opened,
        Err(code) => return code,
    };

    // Bind the name only, never `kind`: the server fills `kind` only when the
    // record has none, so the readiness check compares against the detector's
    // kernel-derived answer rather than our own claim.
    let bound = AgentRecord {
        name: req.name.to_owned(),
        ..AgentRecord::default()
    };
    let bound_bytes = bound.encode();
    if let Err(refusal) = bind_name(&mut conn, &terminal, &bound_bytes).await {
        return emit(req.json, &refusal);
    }

    let started = Instant::now();
    if let Err(refusal) = submit_line(plan, &mut conn, &terminal, &bound_bytes).await {
        return emit(req.json, &refusal);
    }
    if req.no_wait {
        return report_submitted(req, plan, &terminal);
    }
    await_ready(req, plan, &terminal, socket_path, started).await
}

/// Resolve the target pane and open the connection that will type into it,
/// refusing (as an emitted exit code) a satellite pane, a failed
/// precondition, a server without acknowledged input, or an occupied pane.
#[allow(
    clippy::future_not_send,
    reason = "same current-thread CLI future as `drive`"
)]
async fn open_target(
    req: &StartRequest<'_>,
    plan: &Plan,
    selector: &phux_client::selector::Selector,
    socket_path: &Path,
) -> Result<(ResourceId, Connection), ExitCode> {
    let terminal = resolve_target(socket_path, selector, "agent start", req.json).await?;
    // APPLY_INPUT is local-only and `phux.agent/v1` does not federate.
    if !matches!(terminal, ResourceId::Local { .. }) {
        return Err(emit(req.json, &satellite_refusal(&terminal)));
    }
    check_preconditions(req, plan, &terminal, socket_path)
        .await
        .map_err(|refusal| emit(req.json, &refusal))?;

    let no_server = |err| json_err::report_no_server(req.json, &err, socket_path, "agent start");
    let mut conn = Connection::connect(socket_path).await.map_err(no_server)?;
    acknowledged_input_available(&conn).map_err(|refusal| emit(req.json, &refusal))?;
    match recheck_occupant(&mut conn, &terminal).await {
        Ok(()) => Ok((terminal, conn)),
        Err(Ok(refusal)) => Err(emit(req.json, &refusal)),
        Err(Err(err)) => Err(no_server(err)),
    }
}

fn satellite_refusal(terminal: &ResourceId) -> Refusal {
    Refusal::new(
        json_err::codes::SATELLITE_TARGET,
        format!(
            "{} is a satellite pane; acknowledged input and agent metadata are hub-local",
            phux_client::selector::format_terminal_id(terminal)
        ),
        "run `phux agent start` against the satellite's own server",
        EXIT_USAGE,
    )
}

/// Type the launch line with acknowledged input. On failure the name bind
/// is rolled back only when provably nothing was typed; otherwise a running
/// agent would be left without a handle.
async fn submit_line(
    plan: &Plan,
    conn: &mut Connection,
    terminal: &ResourceId,
    bound_bytes: &[u8],
) -> Result<(), Refusal> {
    let events = submit_events(&plan.line);
    let operation_id = new_operation_id();
    let Err(failure) = apply_input(conn, terminal, operation_id, events).await else {
        return Ok(());
    };
    if failure.wrote_nothing {
        rollback_bind(conn, terminal, bound_bytes).await;
    }
    Err(failure.refusal)
}

/// Wait for the agent to reach a ready state and report the outcome.
#[allow(
    clippy::future_not_send,
    reason = "inherited from `wait_for_agent_state`; see `drive`"
)]
async fn await_ready(
    req: &StartRequest<'_>,
    plan: &Plan,
    terminal: &ResourceId,
    socket_path: &Path,
    started: Instant,
) -> ExitCode {
    let outcome = wait_for_agent_state(
        socket_path,
        terminal,
        READY_STATES,
        Some(plan.timeout),
        POLL_INTERVAL,
    )
    .await;
    let latency = started.elapsed();
    match outcome {
        Ok(result) if result.satisfied() => {
            report_ready(req, plan, terminal, &result, latency, socket_path).await
        }
        Ok(result) => report_timeout(req, plan, terminal, &result),
        Err(err) => emit(req.json, &wait_refusal(terminal, err)),
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

/// Re-read the pane's occupant on the connection that is about to write, so
/// the check and the bind are ordered within one connection.
///
/// `Err(Ok(_))` is a refusal, `Err(Err(_))` a transport failure.
async fn recheck_occupant(
    conn: &mut Connection,
    terminal: &ResourceId,
) -> Result<(), Result<Refusal, AttachError>> {
    match read_record(conn, terminal, 90).await {
        Ok(Answer::Ok(Some(occupant))) if occupant.state != AgentMetaState::Unknown => {
            Err(Ok(Refusal::new(
                codes::AGENT_PANE_BUSY,
                format!(
                    "{} began hosting '{}' ({}) while the preconditions were being checked; \
                     nothing was typed",
                    phux_client::selector::format_terminal_id(terminal),
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
    terminal: &ResourceId,
    socket_path: &Path,
) -> Result<(), Refusal> {
    // Fail closed: a precondition that cannot be evaluated has not passed.
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
    index: &std::collections::HashMap<ResourceId, AgentRecord>,
    terminal: &ResourceId,
    name: &str,
) -> Result<(), Refusal> {
    let holders: Vec<String> = index
        .iter()
        .filter(|(id, record)| {
            *id != terminal && record.name.trim().eq_ignore_ascii_case(name.trim())
        })
        .map(|(id, _)| phux_client::selector::format_terminal_id(id))
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
    index: &std::collections::HashMap<ResourceId, AgentRecord>,
    terminal: &ResourceId,
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
            phux_client::selector::format_terminal_id(terminal),
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

/// The pane's shell is already somewhere, and this verb will not `cd` it:
/// that side effect would outlive the verb.
fn check_cwd_agreement(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    terminal: &ResourceId,
    plan: &Plan,
) -> Result<(), Refusal> {
    let Some(pane_cwd) = snapshot
        .resources
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
            phux_client::selector::format_terminal_id(terminal),
        ),
        "`agent start` never `cd`s someone's shell; run it from that directory, or use \
         `phux launch`, which spawns its own pane with the right cwd",
        EXIT_USAGE,
    ))
}

/// The available-shell precondition (shared verdict with `phux run`),
/// rendered as this verb's refusals.
async fn check_shell_available(socket_path: &Path, terminal: &ResourceId) -> Result<(), Refusal> {
    let label = phux_client::selector::format_terminal_id(terminal);
    match pane_shell_availability(socket_path, terminal).await {
        ShellAvailability::Available => Ok(()),
        ShellAvailability::BusyProcess(foreground) => Err(Refusal::new(
            codes::AGENT_PANE_NOT_AVAILABLE,
            format!("{label} is running '{foreground}' in the foreground, not its pane shell"),
            "wait for the foreground job to finish, pick another pane, or pass `--force` if \
             you know the pane is free",
            EXIT_USAGE,
        )),
        ShellAvailability::BusyScreen => Err(Refusal::new(
            codes::AGENT_PANE_NOT_AVAILABLE,
            format!(
                "{label} is not sitting at its shell prompt — the cursor is not on a marked \
                 command line, so something else has the screen"
            ),
            "wait for the foreground job to finish, pick another pane, or pass `--force` if \
             you know the pane is free",
            EXIT_USAGE,
        )),
        ShellAvailability::Unanswerable => Err(Refusal::new(
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

/// Write the `phux.agent/v1` bind and confirm it landed. `SET_METADATA` has
/// no reply, so the read-back orders the write before any input.
async fn bind_name(
    conn: &mut Connection,
    terminal: &ResourceId,
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
        scope: Scope::Resource(terminal.clone()),
        key: RESOURCE_AGENT_KEY.to_owned(),
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

/// Undo the bind, but only after proving nothing else changed it
/// (read-compare-delete; L3 has no compare-and-swap). Different bytes mean
/// the detector or a third party wrote, so leave them and say so.
async fn rollback_bind(conn: &mut Connection, terminal: &ResourceId, expected: &[u8]) {
    match read_record(conn, terminal, 200).await {
        Ok(Answer::Ok(Some(record))) if record.encode() == expected => {
            if let Err(err) = conn
                .send(&FrameKind::DeleteMetadata {
                    request_id: 201,
                    scope: Scope::Resource(terminal.clone()),
                    key: RESOURCE_AGENT_KEY.to_owned(),
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
    terminal: &ResourceId,
    request_id: u32,
) -> Result<Answer<Option<AgentRecord>>, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Resource(terminal.clone()),
            RESOURCE_AGENT_KEY.to_owned(),
        )
        .await?
        .into_parts();
    for message in phux_client::state::degradation_notices(&interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
    Ok(answer.map(|value| value.as_deref().and_then(parse_agent_record)))
}

/// The one input batch: the command line as a trusted paste, then Enter
/// (ADR-0076 point 3). Enter last means a partial delivery leaves unsubmitted
/// text on screen, the recoverable failure.
#[must_use]
fn submit_events(line: &str) -> Vec<InputEvent> {
    phux_client::send_keys::events_for(&[line.to_owned(), "Enter".to_owned()])
}

/// A fresh CSPRNG operation id (ADR-0053): `UUIDv4` bytes, never all-zero.
fn new_operation_id() -> Option<InputOperationId> {
    InputOperationId::new(*uuid::Uuid::new_v4().as_bytes())
}

/// A failed submit, plus whether the pane is provably untouched.
#[derive(Debug)]
struct SubmitFailure {
    refusal: Refusal,
    /// `true` only when the server provably handed no bytes to the PTY.
    wrote_nothing: bool,
}

/// Submit the batch under `operation_id` through the shared acknowledged-input
/// classifier and map its verdict onto this verb's diagnostics.
async fn apply_input(
    conn: &mut Connection,
    terminal: &ResourceId,
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
    let (verdict, interleaved) = apply_input_once(conn, terminal, operation_id, events, 110)
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
    debug_assert!(
        interleaved.is_empty(),
        "unsubscribed connection received {interleaved:?} ahead of the APPLY_INPUT ack",
    );
    match verdict {
        ApplyVerdict::Acked => Ok(()),
        other => Err(submit_verdict(other)),
    }
}

/// Map the shared acknowledged-input verdict onto this verb's exit contract
/// and bind-unwind safety.
fn submit_verdict(verdict: ApplyVerdict) -> SubmitFailure {
    match verdict {
        // NOT exit 3: any retry either replays the unknown or duplicates.
        ApplyVerdict::Unknown(message) => SubmitFailure {
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
        ApplyVerdict::Busy(message) => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("the acknowledged input lane is busy: {message}"),
                "nothing was typed; the lane is server-wide and single — back off and retry",
                EXIT_FAILURE,
            ),
            wrote_nothing: true,
        },
        ApplyVerdict::NotWritten(message) => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("the launch command was not written: {message}"),
                "nothing reached the pane; inspect its health, then retry — even a fresh \
                 operation id cannot duplicate this attempt",
                EXIT_FAILURE,
            ),
            wrote_nothing: true,
        },
        ApplyVerdict::NotFound(message) => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("the target pane disappeared before input handoff: {message}"),
                "nothing was typed; choose a live shell pane and retry",
                EXIT_FAILURE,
            ),
            wrote_nothing: true,
        },
        ApplyVerdict::Refused(ApplyRefusal::InputLeaseHeld(message)) => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("another client holds the input lease on this pane: {message}"),
                "nothing was typed; retry once the lease is released",
                EXIT_USAGE,
            ),
            wrote_nothing: true,
        },
        ApplyVerdict::Refused(ApplyRefusal::CanonicalLimitExceeded(message)) => SubmitFailure {
            refusal: Refusal::new(
                codes::INVALID_LAUNCH_ARGV,
                format!("the launch command exceeds the acknowledged-input limits: {message}"),
                "nothing was typed; shorten the integration's argv or its `-- ARGS`",
                EXIT_USAGE,
            ),
            wrote_nothing: true,
        },
        ApplyVerdict::Refused(reason) => SubmitFailure {
            refusal: Refusal::new(
                codes::AGENT_START_FAILED,
                format!("the launch command was refused: {reason}"),
                "nothing was typed; correct the refusal before retrying",
                EXIT_USAGE,
            ),
            wrote_nothing: true,
        },
        // Handled by the caller; retained so this mapping is total.
        ApplyVerdict::Acked => SubmitFailure {
            refusal: Refusal::new(
                json_err::codes::INTERNAL_ERROR,
                "an acknowledged launch command was mapped as a failure",
                "report this",
                EXIT_FAILURE,
            ),
            wrote_nothing: false,
        },
    }
}

/// Turn a wait error into its refusal.
fn wait_refusal(terminal: &ResourceId, err: AgentWaitError) -> Refusal {
    let label = phux_client::selector::format_terminal_id(terminal);
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
fn report_submitted(req: &StartRequest<'_>, plan: &Plan, terminal: &ResourceId) -> ExitCode {
    let label = phux_client::selector::format_terminal_id(terminal);
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
    terminal: &ResourceId,
    result: &AgentWaitResult,
    latency: Duration,
    socket_path: &Path,
) -> ExitCode {
    let label = phux_client::selector::format_terminal_id(terminal);
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
/// `manifest_asserts_idle: false` tells the caller an `idle` readiness is an
/// absence of contrary evidence. Evaluated client-side, so it can disagree
/// with the server's own trace.
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

/// Re-run the manifest client-side so the answer carries evidence. Best
/// effort: a failure costs one optional sub-document, not the result.
async fn provenance(socket_path: &Path, terminal: &ResourceId, kind: &str) -> Option<Explanation> {
    let screen =
        phux_client::snapshot::get_screen_scrollback(socket_path, terminal.clone(), None, false)
            .await
            .ok()?;
    agent_explain::explain(
        kind,
        &agent_explain::Capture {
            // The live OSC title, the one the server's detector evaluates.
            title: screen.title.unwrap_or_default(),
            lines: screen.lines,
        },
    )
}

/// The deadline expired with no qualifying publication.
fn report_timeout(
    req: &StartRequest<'_>,
    plan: &Plan,
    terminal: &ResourceId,
    result: &AgentWaitResult,
) -> ExitCode {
    let label = phux_client::selector::format_terminal_id(terminal);
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

    use phux_protocol::input::paste::PasteTrust;

    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    /// The grammar this verb binds is the one reserved for the proposed
    /// `%name` selector.
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

    /// Env rides `env NAME=value` (a `VAR=v cmd` prefix is not valid fish);
    /// control characters, bad env names, empty and oversized lines are refused.
    #[test]
    fn shell_line_quotes_every_word_and_refuses_what_quoting_cannot_contain() {
        let env = |key: &str, value: &str| BTreeMap::from([(key.to_owned(), value.to_owned())]);
        assert_eq!(
            shell_line(
                &argv(&["/abs/claude", "--resume", "abc-123"]),
                &env("PHUX_CLAUDE_SESSION_ID", "abc-123")
            ),
            Ok(
                "env 'PHUX_CLAUDE_SESSION_ID=abc-123' '/abs/claude' '--resume' 'abc-123'"
                    .to_owned()
            )
        );
        assert_eq!(
            shell_line(&argv(&["codex"]), &BTreeMap::new()),
            Ok("'codex'".to_owned())
        );
        assert_eq!(
            shell_line(&argv(&["claude", "--flag\nrm -rf /"]), &BTreeMap::new()),
            Err(ShellLineError::Control {
                index: 1,
                element: "--flag\nrm -rf /".to_owned(),
            })
        );
        assert!(shell_line(&argv(&["cl\u{0}aude"]), &BTreeMap::new()).is_err());
        assert_eq!(
            shell_line(&[], &BTreeMap::new()),
            Err(ShellLineError::Empty)
        );
        assert!(matches!(
            shell_line(
                &argv(&["claude", &"x".repeat(MAX_SHELL_LINE)]),
                &BTreeMap::new()
            ),
            Err(ShellLineError::TooLong { .. })
        ));
        assert_eq!(
            shell_line(&argv(&["claude"]), &env("NOT AN IDENT", "x")),
            Err(ShellLineError::EnvName("NOT AN IDENT".to_owned()))
        );
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

    /// The shared verdicts retain their proof boundary when projected onto
    /// start-specific errors. `NotWritten` must roll the provisional name
    /// back; `Unknown` must retain it.
    #[test]
    fn submit_verdicts_map_to_their_unwind_safety_and_exit() {
        let unknown = submit_verdict(ApplyVerdict::Unknown("lost".to_owned()));
        assert!(!unknown.wrote_nothing);
        assert_eq!(unknown.refusal.err.code, codes::AGENT_START_UNKNOWN);
        // NOT exit 3: 3 means "retry is correct", and retrying this under a
        // new id produces the duplicate the acknowledged lane prevents.
        assert_eq!(unknown.refusal.exit, EXIT_FAILURE);

        for verdict in [
            ApplyVerdict::Busy("no".to_owned()),
            ApplyVerdict::NotWritten("no PTY".to_owned()),
            ApplyVerdict::NotFound("gone".to_owned()),
            ApplyVerdict::Refused(ApplyRefusal::InputLeaseHeld("no".to_owned())),
            ApplyVerdict::Refused(ApplyRefusal::CanonicalLimitExceeded("no".to_owned())),
        ] {
            let failure = submit_verdict(verdict);
            assert!(
                failure.wrote_nothing,
                "a proven non-delivery must be unwindable: {failure:?}"
            );
        }
        assert_eq!(
            submit_verdict(ApplyVerdict::Refused(ApplyRefusal::InputLeaseHeld(
                "no".to_owned()
            )))
            .refusal
            .exit,
            EXIT_USAGE
        );
        assert_eq!(
            submit_verdict(ApplyVerdict::Busy("no".to_owned()))
                .refusal
                .exit,
            EXIT_FAILURE
        );
    }

    fn request<'a>(name: &'a str, kind: &'a str) -> StartRequest<'a> {
        StartRequest {
            name,
            kind,
            target: "@7",
            integration: None,
            timeout: None,
            no_wait: false,
            force: false,
            json: false,
            args: &[],
        }
    }

    /// A bad name is refused before the kind is resolved, and a kind with no
    /// manifest is refused before anything is typed, naming a way forward.
    #[test]
    fn an_unmanifested_kind_is_refused_before_any_write() {
        let refusal = preflight(&request("Build Bot", "no-such-agent-kind"))
            .expect_err("a bad name must be refused");
        assert_eq!(refusal.err.code, codes::INVALID_AGENT_NAME);

        let refusal = preflight(&request("build", "no-such-agent-kind"))
            .expect_err("an unmanifested kind must be refused");
        assert_eq!(refusal.err.code, codes::UNSUPPORTED_AGENT_KIND);
        assert_eq!(refusal.exit, EXIT_USAGE);
        assert!(
            refusal.err.remedy.contains("--no-wait"),
            "the refusal must not be a dead end: {}",
            refusal.err.remedy
        );
    }

    /// An unclaimed `--kind CLAUDE` falls back to the integration id of the
    /// canonical kind (`claude`), never the string as typed.
    #[test]
    fn an_unclaimed_kind_falls_back_to_the_canonical_kind_not_the_typed_one() {
        if agent_explain::resolve_kind("CLAUDE").as_deref() != Some("claude") {
            // No shipped manifest to canonicalize through (PHUX_AGENT_DETECT=0);
            // `the_shipped_manifests_resolve_as_kinds` covers that separately.
            return;
        }
        match preflight(&request("canonical-kind", "CLAUDE")) {
            Ok(plan) => {
                assert_eq!(plan.kind, "claude");
                assert!(
                    !plan.integration_id.contains("CLAUDE"),
                    "the resolved id must come from the canonical kind: {}",
                    plan.integration_id
                );
            }
            Err(refusal) => assert!(
                !refusal.err.message.contains("CLAUDE"),
                "the refusal must name the canonical id, not the typed kind: {}",
                refusal.err.message
            ),
        }
    }

    /// Every shipped detection manifest is a legitimate `--kind`.
    /// This is the roster the refusal above prints.
    #[test]
    fn the_shipped_manifests_resolve_as_kinds() {
        let kinds = agent_explain::kinds();
        for kind in [
            "claude",
            "codex",
            "opencode",
            "pi",
            "omp",
            "grok",
            "amp",
            "cursor-agent",
            "gemini",
            "goose",
            "aider",
        ] {
            assert!(
                kinds.iter().any(|loaded| loaded == kind),
                "{kind} must ship a detection manifest; loaded: {kinds:?}"
            );
            assert_eq!(agent_explain::resolve_kind(kind).as_deref(), Some(kind));
        }
    }
}
