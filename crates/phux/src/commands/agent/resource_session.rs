//! `phux agent session open|close`, `phux agent emit`, `phux agent log` —
//! the CLI over the `AgentSession` resource (ADR-0103,
//! `docs/consumers/agents.md` §2 and §4.19).
//!
//! Every verb here is a thin surface over [`phux_client::agent_session`]:
//! resolve the target, gate on the server's `RESOURCE_KINDS` bit before any
//! resource is touched, run the library call, render. The one piece of
//! policy that lives here is *which resource a selector names*:
//!
//! - `open` takes the **parent pane**: a Terminal-kind resource, by `@N`, by
//!   `%name` (the named Terminal), or by any pane form. An `AgentSession` id
//!   is `wrong_resource_kind`.
//! - `close`, `emit`, and `log` take the **session**: an `AgentSession` id
//!   names itself; a pane names its unique live child; `%name` names the
//!   session of the agent so named. A pane with no child is
//!   `no_agent_session`, one with several `agent_session_ambiguous`.
//!
//! Exit codes follow the two families the rest of `agent` uses: `open` and
//! `close` address one pane the way `set`/`clear` do and spend `3` on a miss
//! against a partial fleet view; `emit` and `log` keep `1` for a miss. Every
//! refusal with nothing written or read is `2`.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::agent_session::{
    self, AgentEventRecord, AgentSessionError, EmitRecord, LogEnd, LogOptions,
};
use phux_client::attach::connection::Connection;
use phux_client::resource;
use phux_client::selector::Selector;
use phux_client::state::Degradation;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, partial, report_agent_resolve_error};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// Version of the `session open`, `emit`, and `log` documents.
const SCHEMA_VERSION: u8 = 1;

/// `phux agent session <open|close>`.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum SessionAction {
    /// Open an agent session bound to a pane.
    ///
    /// Spawns an `AgentSession` resource whose parent is the Terminal
    /// TARGET resolves to: the server-side, producer-fed event log the
    /// agent's harness appends to with `phux agent emit`, and the thing
    /// `%name`, `phux agent log`, and the sidebar read. Closing the pane
    /// closes the session; closing the session never touches the pane. The
    /// caller becomes the session's producer — only the client that opened a
    /// session may append to it. The server does not deduplicate: a pane
    /// that already has a live session gets a second one.
    ///
    /// Refused with `unsupported_server` on a server that does not advertise
    /// `RESOURCE_KINDS` (see `phux status --json`'s `features`).
    Open {
        /// The parent pane: a selector resolving to one Terminal (`@N`,
        /// `%name`, `session:window.pane`).
        target: String,
        /// Agent provider slug, e.g. `claude`.
        #[arg(long, value_name = "P")]
        provider: String,
        /// The provider's own opaque session id, when it has one.
        #[arg(long, value_name = "ID")]
        native_id: Option<String>,
        /// Emit the machine-readable result document instead of the bare
        /// resource id.
        #[arg(long)]
        json: bool,
    },
    /// Close a pane's agent session; the pane is untouched.
    ///
    /// TARGET is the session (`@N`), the pane hosting it, or `%name`.
    /// Prints `@N<TAB>closed`.
    Close {
        /// The session: its resource id, the pane hosting it, or `%name`.
        target: String,
    },
}

/// `phux agent session open TARGET --provider P [--native-id ID] [--json]`.
pub(super) fn run_session_open(
    target: &str,
    provider: &str,
    native_id: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    if provider.trim().is_empty() {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::RECORD_INVALID,
                "--provider must not be empty",
                "name the harness, e.g. `--provider claude`",
            ),
            EXIT_USAGE,
        );
    }
    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let verb = "agent session open";
        let mut scope = match Scope::connect(&socket_path, verb, json).await {
            Ok(scope) => scope,
            Err(code) => return code,
        };
        let parent = match scope
            .resolve_parent(&socket_path, &selector, target, verb, json)
            .await
        {
            Ok(parent) => parent,
            Err(code) => return code,
        };
        match agent_session::open(&mut scope.conn, &parent, provider, native_id).await {
            Ok(opened) => {
                let resource = crate::selector::format_terminal_id(&opened.resource);
                if json {
                    let document = serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "resource": resource,
                        "parent": crate::selector::format_terminal_id(&opened.parent),
                        "provider": provider,
                        "native_id": native_id,
                    });
                    print_document(&document)
                } else {
                    outln!("{resource}");
                    ExitCode::SUCCESS
                }
            }
            Err(err) => report_session_error(json, &err, &socket_path, verb),
        }
    })
}

/// `phux agent session close TARGET`.
pub(super) fn run_session_close(target: &str, socket: Option<PathBuf>) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let verb = "agent session close";
        let mut scope = match Scope::connect(&socket_path, verb, false).await {
            Ok(scope) => scope,
            Err(code) => return code,
        };
        let session = match scope
            .resolve_session(
                &socket_path,
                &selector,
                target,
                verb,
                false,
                MissStatus::Partial,
            )
            .await
        {
            Ok(session) => session,
            Err(code) => return code,
        };
        match agent_session::close(&mut scope.conn, &session).await {
            Ok(()) => {
                outln!("{}\tclosed", crate::selector::format_terminal_id(&session));
                ExitCode::SUCCESS
            }
            Err(err) => report_session_error(false, &err, &socket_path, verb),
        }
    })
}

/// `phux agent emit TARGET --type T [--data JSON | -] [--json]`.
pub(super) fn run_emit(
    target: &str,
    event_type: &str,
    data: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let record = match read_data(data).and_then(|data| EmitRecord::new(event_type, data)) {
        Ok(record) => record,
        Err(err) => return report_session_error(json, &err, Path::new(""), "agent emit"),
    };
    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let verb = "agent emit";
        let mut scope = match Scope::connect(&socket_path, verb, json).await {
            Ok(scope) => scope,
            Err(code) => return code,
        };
        let session = match scope
            .resolve_session(
                &socket_path,
                &selector,
                target,
                verb,
                json,
                MissStatus::Keep,
            )
            .await
        {
            Ok(session) => session,
            Err(code) => return code,
        };
        match agent_session::emit(&mut scope.conn, &session, std::slice::from_ref(&record)).await {
            Ok(emitted) => {
                if json {
                    let document = serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "resource": crate::selector::format_terminal_id(&session),
                        "seq": emitted.seq,
                        "ts_ms": emitted.ts_ms,
                        "type": record.kind(),
                    });
                    print_document(&document)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(err) => report_session_error(json, &err, &socket_path, verb),
        }
    })
}

/// `phux agent log TARGET [--follow] [--tail N] [--json]`.
pub(super) fn run_log(
    target: &str,
    follow: bool,
    tail: Option<usize>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let verb = "agent log";
        let mut scope = match Scope::connect(&socket_path, verb, json).await {
            Ok(scope) => scope,
            Err(code) => return code,
        };
        let session = match scope
            .resolve_session(
                &socket_path,
                &selector,
                target,
                verb,
                json,
                MissStatus::Keep,
            )
            .await
        {
            Ok(session) => session,
            Err(code) => return code,
        };
        let info = resource::find(&scope.snapshot, &session).cloned();
        let options = LogOptions { follow, tail };

        // Envelope mode buffers the retained records; the two streaming
        // modes print each record as it arrives.
        let mut buffered: Vec<AgentEventRecord> = Vec::new();
        let outcome = {
            let sink = |record: AgentEventRecord| {
                if json && !follow {
                    buffered.push(record);
                } else if json {
                    match serde_json::to_string(&record) {
                        Ok(line) => outln!("{line}"),
                        Err(err) => eprintln!("phux: could not render record: {err}"),
                    }
                } else {
                    outln!(
                        "{}\t{}\t{}\t{}",
                        record.seq,
                        record.ts_ms,
                        record.kind,
                        serde_json::to_string(&record.data).unwrap_or_default()
                    );
                }
                true
            };
            let read = agent_session::log(&mut scope.conn, &session, options, sink);
            // Ctrl-C is a clean stop, the `watch` contract.
            tokio::select! {
                outcome = read => outcome,
                _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
            }
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(err) => return report_session_error(json, &err, &socket_path, verb),
        };
        if json && !follow {
            let facet = info.as_ref().and_then(|info| info.agent.as_ref());
            let document = serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "resource": crate::selector::format_terminal_id(&session),
                "parent": info
                    .as_ref()
                    .and_then(|info| info.parent.as_ref())
                    .map(crate::selector::format_terminal_id),
                "provider": facet.map(|facet| facet.provider.clone()),
                "native_id": facet.and_then(|facet| facet.native_id.clone()),
                "records": buffered,
            });
            return print_document(&document);
        }
        match outcome.end {
            LogEnd::Retained | LogEnd::SessionClosed | LogEnd::Stopped => ExitCode::SUCCESS,
            LogEnd::Disconnected => {
                eprintln!("phux: server closed the connection while following {target}");
                ExitCode::from(EXIT_FAILURE)
            }
        }
    })
}

/// How a selector miss exits: `3` for the verbs that address one pane the
/// way `set`/`clear` do, `1` where the status is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissStatus {
    Partial,
    Keep,
}

/// One connection, gated, plus the snapshot every session verb resolves
/// against.
struct Scope {
    conn: Connection,
    snapshot: SessionSnapshot,
    degradation: Degradation,
}

impl Scope {
    /// Connect, refuse a server without `RESOURCE_KINDS` before anything is
    /// read, then take the snapshot on the same connection.
    async fn connect(socket_path: &Path, verb: &str, json: bool) -> Result<Self, ExitCode> {
        let mut conn = Connection::connect(socket_path)
            .await
            .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))?;
        if let Err(err) = agent_session::require_support(&conn) {
            return Err(report_session_error(json, &err, socket_path, verb));
        }
        let (snapshot, degradation) = phux_client::state::get_state_on(&mut conn)
            .await
            .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))?
            .into_parts();
        Ok(Self {
            conn,
            snapshot,
            degradation,
        })
    }

    /// The resource a non-`%name` selector picks, of any kind.
    async fn pick(&self, socket_path: &Path, selector: &Selector) -> Option<TerminalId> {
        let candidates =
            phux_client::state::resolve_targets(socket_path, selector, &self.snapshot).await;
        crate::selector::pick_target_pane(&candidates, &self.snapshot.focused_pane)
    }

    /// The parent Terminal `open` binds to.
    async fn resolve_parent(
        &self,
        socket_path: &Path,
        selector: &Selector,
        target: &str,
        verb: &str,
        json: bool,
    ) -> Result<TerminalId, ExitCode> {
        let picked = match selector {
            Selector::Agent(name) => {
                phux_client::state::resolve_agent_target(socket_path, name, &self.snapshot, false)
                    .await
                    .map_err(|err| report_agent_resolve_error(json, &err, false))?
                    .terminal
            }
            _ => self.pick(socket_path, selector).await.ok_or_else(|| {
                partial::report_target_miss_for(json, Some(target), &self.degradation)
            })?,
        };
        partial::warn_partial_view(verb, &self.degradation);
        match resource::find(&self.snapshot, &picked) {
            Some(info) if !resource::is_terminal(info) => Err(report_session_error(
                json,
                &AgentSessionError::WrongKind {
                    resource: picked,
                    expected: resource::TERMINAL,
                    actual: resource::kind_name(info.kind).to_owned(),
                },
                socket_path,
                verb,
            )),
            _ => Ok(picked),
        }
    }

    /// The `AgentSession` a session verb acts on.
    async fn resolve_session(
        &self,
        socket_path: &Path,
        selector: &Selector,
        target: &str,
        verb: &str,
        json: bool,
        miss: MissStatus,
    ) -> Result<TerminalId, ExitCode> {
        let session = if let Selector::Agent(name) = selector {
            let resolved =
                phux_client::state::resolve_agent_target(socket_path, name, &self.snapshot, false)
                    .await
                    .map_err(|err| {
                        report_agent_resolve_error(json, &err, miss == MissStatus::Keep)
                    })?;
            resolved.session.ok_or(AgentSessionError::NoSession {
                terminal: resolved.terminal,
            })
        } else {
            let picked = self
                .pick(socket_path, selector)
                .await
                .ok_or_else(|| match miss {
                    MissStatus::Partial => {
                        partial::report_target_miss_for(json, Some(target), &self.degradation)
                    }
                    MissStatus::Keep => partial::report_target_miss_keeping_status_for(
                        json,
                        Some(target),
                        &self.degradation,
                    ),
                })?;
            resource::session_of(&self.snapshot, &picked)
        };
        partial::warn_partial_view(verb, &self.degradation);
        session.map_err(|err| report_session_error(json, &err, socket_path, verb))
    }
}

/// `--data`: inline JSON, `-` for stdin, `{}` when omitted.
fn read_data(data: Option<&str>) -> Result<serde_json::Value, AgentSessionError> {
    let text = match data {
        None => return Ok(serde_json::json!({})),
        Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| AgentSessionError::RecordInvalid(format!("--data - : {err}")))?;
            text
        }
        Some(inline) => inline.to_owned(),
    };
    serde_json::from_str(&text)
        .map_err(|err| AgentSessionError::RecordInvalid(format!("--data is not JSON: {err}")))
}

fn print_document(document: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(document) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &json_err::CliError::new(
                json_err::codes::JSON_SERIALIZE,
                format!("could not render the document: {err}"),
                "report this: a document of strings and numbers cannot fail to serialize",
            ),
            EXIT_FAILURE,
        ),
    }
}

/// Map a library failure onto the closed error-code vocabulary and exit.
fn report_session_error(
    json: bool,
    err: &AgentSessionError,
    socket_path: &Path,
    verb: &str,
) -> ExitCode {
    let (code, exit_code, remedy): (&'static str, u8, String) = match err {
        AgentSessionError::Unsupported => (
            json_err::codes::UNSUPPORTED_SERVER,
            EXIT_USAGE,
            "upgrade the server (`phux upgrade`), or keep using `phux agent set` / \
             `phux agent report-state`; `phux status --json` lists the advertised `features`"
                .to_owned(),
        ),
        AgentSessionError::NoSession { terminal } => (
            json_err::codes::NO_AGENT_SESSION,
            EXIT_USAGE,
            format!(
                "open one with `phux agent session open {} --provider <P>`; `phux ls --json` \
                 lists every resource with its kind and parent",
                crate::selector::format_terminal_id(terminal)
            ),
        ),
        AgentSessionError::AmbiguousSession { .. } => (
            json_err::codes::AGENT_SESSION_AMBIGUOUS,
            EXIT_USAGE,
            "address the session directly by its @N".to_owned(),
        ),
        AgentSessionError::WrongKind { expected, .. } => (
            json_err::codes::WRONG_RESOURCE_KIND,
            EXIT_USAGE,
            format!("name a {expected} resource; `phux ls --json` lists every resource's kind"),
        ),
        AgentSessionError::ParentNotFound { .. } => (
            json_err::codes::PARENT_NOT_FOUND,
            EXIT_FAILURE,
            "the pane went away between the lookup and the spawn; re-read `phux ls --json`"
                .to_owned(),
        ),
        AgentSessionError::ParentKindMismatch { .. } => (
            json_err::codes::PARENT_KIND_MISMATCH,
            EXIT_USAGE,
            "name the pane hosting the agent, not another session".to_owned(),
        ),
        AgentSessionError::RecordInvalid(_) => (
            json_err::codes::RECORD_INVALID,
            EXIT_USAGE,
            format!(
                "--type is one of {}; --data is a JSON object under 16 KiB",
                agent_session::EVENT_TYPES.join(", ")
            ),
        ),
        AgentSessionError::NotProducer { .. } => (
            json_err::codes::NOT_PRODUCER,
            EXIT_USAGE,
            "only the client that ran `phux agent session open` may append; open a session of \
             your own"
                .to_owned(),
        ),
        AgentSessionError::Overflow(_) => (
            json_err::codes::OVERFLOW,
            EXIT_USAGE,
            "raise `defaults.agent-log-bytes`, or emit smaller records".to_owned(),
        ),
        AgentSessionError::Refused(_) => (
            json_err::codes::AGENT_SESSION_REFUSED,
            EXIT_FAILURE,
            "run `phux doctor` for a health check".to_owned(),
        ),
        AgentSessionError::Transport(inner) => {
            return json_err::report_no_server(json, inner, socket_path, verb);
        }
    };
    json_err::emit(
        json,
        &json_err::CliError::new(code, format!("{verb}: {err}"), remedy),
        exit_code,
    )
}

/// Every server-side failure maps onto a distinct code; a transport failure
/// keeps the shared no-server family.
#[cfg(test)]
mod tests {
    use super::*;
    use phux_client::attach::AttachError;

    #[test]
    fn data_defaults_to_an_empty_object_and_refuses_non_json() {
        assert_eq!(read_data(None).unwrap(), serde_json::json!({}));
        assert_eq!(
            read_data(Some(r#"{"chars":4}"#)).unwrap(),
            serde_json::json!({ "chars": 4 })
        );
        assert!(matches!(
            read_data(Some("not json")),
            Err(AgentSessionError::RecordInvalid(_))
        ));
        // A JSON value that is not an object is the record validator's
        // refusal, not the parser's.
        let parsed = read_data(Some("[1]")).unwrap();
        assert!(matches!(
            EmitRecord::new("stop", parsed),
            Err(AgentSessionError::RecordInvalid(_))
        ));
    }

    #[test]
    fn every_library_error_lands_on_its_registered_code_and_exit() {
        let socket = Path::new("/tmp/unused.sock");
        let at = TerminalId::local(7);
        for (err, code, exit) in [
            (
                AgentSessionError::Unsupported,
                json_err::codes::UNSUPPORTED_SERVER,
                2,
            ),
            (
                AgentSessionError::NoSession {
                    terminal: at.clone(),
                },
                json_err::codes::NO_AGENT_SESSION,
                2,
            ),
            (
                AgentSessionError::AmbiguousSession {
                    terminal: at.clone(),
                    candidates: vec![TerminalId::local(9), TerminalId::local(10)],
                },
                json_err::codes::AGENT_SESSION_AMBIGUOUS,
                2,
            ),
            (
                AgentSessionError::WrongKind {
                    resource: at.clone(),
                    expected: resource::AGENT_SESSION,
                    actual: resource::TERMINAL.to_owned(),
                },
                json_err::codes::WRONG_RESOURCE_KIND,
                2,
            ),
            (
                AgentSessionError::ParentNotFound { parent: at.clone() },
                json_err::codes::PARENT_NOT_FOUND,
                1,
            ),
            (
                AgentSessionError::ParentKindMismatch { parent: at.clone() },
                json_err::codes::PARENT_KIND_MISMATCH,
                2,
            ),
            (
                AgentSessionError::RecordInvalid("x".to_owned()),
                json_err::codes::RECORD_INVALID,
                2,
            ),
            (
                AgentSessionError::NotProducer { resource: at },
                json_err::codes::NOT_PRODUCER,
                2,
            ),
            (
                AgentSessionError::Overflow("x".to_owned()),
                json_err::codes::OVERFLOW,
                2,
            ),
            (
                AgentSessionError::Refused("x".to_owned()),
                json_err::codes::AGENT_SESSION_REFUSED,
                1,
            ),
        ] {
            let status = report_session_error(false, &err, socket, "agent emit");
            assert_eq!(status, ExitCode::from(exit), "{code}: {err}");
        }
        // A transport failure keeps the shared no-server family (exit 1).
        let err = AgentSessionError::Transport(AttachError::Disconnected);
        assert_eq!(
            report_session_error(false, &err, socket, "agent log"),
            ExitCode::from(1)
        );
    }
}
