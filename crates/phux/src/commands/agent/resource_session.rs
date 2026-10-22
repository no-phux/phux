//! `phux agent session open|close`, `phux agent emit`, `phux agent log` —
//! the CLI over the `AgentSession` resource (ADR-0103,
//! `docs/agents.md#agents-without-an-integration`).
//!
//! Thin surfaces over [`phux_client::agent_session`], gated on the server's
//! `RESOURCE_KINDS` bit. The policy here is which resource a selector names:
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
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, partial, report_agent_resolve_error};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// Version of the `session open`, `emit`, and `log` documents.
const SCHEMA_VERSION: u8 = 1;

mod report;
use report::{print_document, read_data, report_session_error};

/// `phux agent session <open|close>`.
#[derive(Debug, usage::Subcommands)]
pub(crate) enum SessionAction {
    /// Open an agent session bound to a pane.
    ///
    /// Spawns an `AgentSession` resource whose parent is the Terminal
    /// TARGET resolves to: the server-side, producer-fed event log the
    /// agent's harness appends to with `phux agent emit`, and the thing
    /// `%name`, `phux agent log`, and the sidebar read. Closing the pane
    /// closes the session; closing the session never touches the pane. Any
    /// local connection (this machine's socket or localhost) may append to
    /// it, not only the one that opened it; a remote peer never may. The
    /// server does not deduplicate: a pane that already has a live session
    /// gets a second one.
    ///
    /// Refused with `unsupported_server` on a server that does not advertise
    /// `RESOURCE_KINDS` (see `phux status --json`'s `features`).
    Open {
        /// The parent pane: a selector resolving to one Terminal (`@N`,
        /// `%name`, `session:group.pane`).
        target: String,
        /// Agent provider slug, e.g. `claude`.
        #[usage(long, value_name = "P")]
        provider: String,
        /// The provider's own opaque session id, when it has one.
        #[usage(long, value_name = "ID")]
        native_id: Option<String>,
        /// Emit the machine-readable result document instead of the bare
        /// resource id.
        #[usage(long)]
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
        let outcome = agent_session::open(&mut scope.conn, &parent, provider, native_id).await;
        drop(scope);
        match outcome {
            Ok(opened) => {
                let resource = phux_client::selector::format_terminal_id(&opened.resource);
                if json {
                    let document = serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "resource": resource,
                        "parent": phux_client::selector::format_terminal_id(&opened.parent),
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
        let outcome = agent_session::close(&mut scope.conn, &session).await;
        drop(scope);
        match outcome {
            Ok(()) => {
                outln!(
                    "{}\tclosed",
                    phux_client::selector::format_terminal_id(&session)
                );
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
        let outcome =
            agent_session::emit(&mut scope.conn, &session, std::slice::from_ref(&record)).await;
        drop(scope);
        match outcome {
            Ok(emitted) => {
                if json {
                    let document = serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "resource": phux_client::selector::format_terminal_id(&session),
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
                        Err(err) => error!("could not render record: {err}"),
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
        drop(scope);
        if json && !follow {
            let facet = info.as_ref().and_then(|info| info.agent.as_ref());
            let document = serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "resource": phux_client::selector::format_terminal_id(&session),
                "parent": info
                    .as_ref()
                    .and_then(|info| info.parent.as_ref())
                    .map(phux_client::selector::format_terminal_id),
                "provider": facet.map(|facet| facet.provider.clone()),
                "native_id": facet.and_then(|facet| facet.native_id.clone()),
                "records": buffered,
            });
            return print_document(&document);
        }
        match outcome.end {
            LogEnd::Retained | LogEnd::SessionClosed | LogEnd::Stopped => ExitCode::SUCCESS,
            LogEnd::Disconnected => {
                error!("server closed the connection while following {target}");
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

    /// The resource a non-`%name` selector picks, of any kind. Callers branch
    /// `%name` first, since a session verb needs the resolved session too.
    async fn pick(&self, socket_path: &Path, selector: &Selector) -> Option<ResourceId> {
        let candidates = phux_client::state::resolve_targets(socket_path, selector, &self.snapshot)
            .await
            .ok()?;
        phux_client::selector::pick_target_pane(&candidates, &self.snapshot.focused_resource)
    }

    /// The parent Terminal `open` binds to.
    async fn resolve_parent(
        &self,
        socket_path: &Path,
        selector: &Selector,
        target: &str,
        verb: &str,
        json: bool,
    ) -> Result<ResourceId, ExitCode> {
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
    ) -> Result<ResourceId, ExitCode> {
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
            resource::agent_session_of(&self.snapshot, &picked)
        };
        partial::warn_partial_view(verb, &self.degradation);
        session.map_err(|err| report_session_error(json, &err, socket_path, verb))
    }
}

#[cfg(test)]
mod tests;
