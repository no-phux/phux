//! `phux rm`, `relaunch`, `restore`, `mv` into a session, and `rename` of a
//! pane: the organization verbs (ADR-0160, ADR-0161), over
//! [`phux_client::org`].
//!
//! Each verb resolves its target to one session or resource record: a key
//! prefix (`^ab23cd`) against the records, a session name against the
//! session records, and every other selector against a fresh snapshot, then
//! to the record executing as the one Terminal it names. Every verb needs a
//! server that advertises `ORGANIZATION`.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::org::{self, KeyMatch, OrgError, OrgInventory};
use phux_client::selector::{self, Selector};
use phux_protocol::ids::{KEY_MIN_PREFIX, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Committed, OrgFilter, ResourceRecord, SessionRecord};

use crate::commands::confirm;
use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::{ServerSpec, ServerTarget};

/// Schema of every result document these verbs print under `--json`.
const SCHEMA_VERSION: u8 = 1;

/// What a target names.
#[derive(Debug, Clone)]
pub(crate) enum Target {
    /// A session record.
    Session(SessionRecord),
    /// A resource record.
    Resource(ResourceRecord),
}

/// Why a target did not resolve, or a verb failed, ready to print.
#[derive(Debug)]
pub(crate) struct Failure {
    error: CliError,
    exit: u8,
}

impl Failure {
    fn new(
        code: &'static str,
        message: impl Into<String>,
        remedy: impl Into<String>,
        exit: u8,
    ) -> Self {
        Self {
            error: CliError::new(code, message, remedy),
            exit,
        }
    }

    /// An organization request that failed: refusals exit 2, everything
    /// else 1.
    fn org(verb: &str, err: &OrgError) -> Self {
        let exit = match err {
            OrgError::Refused(_) | OrgError::Server { .. } | OrgError::Ambiguous { .. } => 2,
            _ => 1,
        };
        let remedy = match err {
            OrgError::Unsupported => "upgrade the server (`phux upgrade`)",
            OrgError::OutcomeUnknown { .. } => {
                "run `phux ls` to see whether it took effect, then retry"
            }
            OrgError::NotFound(_) => "run `phux ls` to see sessions and panes",
            _ => "run `phux ls` to see the current state, then retry",
        };
        Self::new(err.code(), format!("{verb}: {err}"), remedy, exit)
    }

    fn emit(&self, json: bool) -> ExitCode {
        json_err::emit(json, &self.error, self.exit)
    }
}

/// The six-digit display prefix `phux ls` and `phux kill` print for a key.
pub(crate) fn short_key(text: &str) -> &str {
    text.get(..KEY_MIN_PREFIX).unwrap_or(text)
}

fn key_text(key: &impl std::fmt::Display) -> String {
    format!("^{}", short_key(&key.to_string()))
}

/// Connect to `server`, reporting an unreachable server under `verb`.
async fn connect(server: &ServerTarget, json: bool, verb: &str) -> Result<Connection, ExitCode> {
    server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, verb))
}

/// Read every record, refusing a server without `ORGANIZATION` first.
async fn records(conn: &mut Connection, verb: &str) -> Result<OrgInventory, Failure> {
    if !org::supported(conn) {
        return Err(Failure::org(verb, &OrgError::Unsupported));
    }
    org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(|err| Failure::org(verb, &err))
}

/// Resolve `text` to one session or resource record (see the module docs).
pub(crate) async fn resolve(
    conn: &mut Connection,
    inventory: &OrgInventory,
    text: &str,
    verb: &str,
) -> Result<Target, Failure> {
    let parsed = selector::parse(text).map_err(|err| {
        Failure::new(
            codes::INVALID_SELECTOR,
            format!("invalid target '{text}': {err}"),
            "run `phux help targets` for the selector grammar",
            1,
        )
    })?;
    match &parsed {
        Selector::Key(prefix) => {
            return match inventory.resolve_key_prefix(prefix) {
                Ok(KeyMatch::Session(record)) => Ok(Target::Session(record)),
                Ok(KeyMatch::Resource(record)) => Ok(Target::Resource(record)),
                Err(err) => Err(Failure::org(verb, &err)),
            };
        }
        Selector::Session(name) => {
            if let Some(record) = inventory.session_named(name) {
                return Ok(Target::Session(record.clone()));
            }
        }
        _ => {}
    }
    let (snapshot, _) = phux_client::state::get_state_on(conn)
        .await
        .map_err(|err| {
            Failure::new(
                codes::TRANSPORT,
                format!("{verb}: could not read state: {err}"),
                "run `phux doctor` for a health check",
                1,
            )
        })?
        .into_parts();
    let ids = phux_client::state::resolve_targets_on(conn, &parsed, &snapshot)
        .await
        .map_err(|err| {
            Failure::new(
                codes::NO_SUCH_TARGET,
                format!("{verb}: {err}"),
                "run `phux ls` to see sessions and panes",
                1,
            )
        })?;
    let [id] = ids.as_slice() else {
        if ids.is_empty() {
            return Err(Failure::new(
                codes::NO_SUCH_TARGET,
                format!("no such target: {text}"),
                "run `phux ls` to see sessions and panes",
                1,
            ));
        }
        return Err(Failure::new(
            codes::SELECTOR_NOT_SINGLE,
            format!("'{text}' names {} panes; {verb} takes one", ids.len()),
            "name one pane (`@N`, `name:N.M`, or a key `^ab23cd`)",
            1,
        ));
    };
    inventory
        .resource_by_live(id)
        .cloned()
        .map(Target::Resource)
        .ok_or_else(|| {
            Failure::new(
                codes::NO_SUCH_TARGET,
                format!("{text} has no record on this server"),
                "run `phux ls` to see sessions and panes",
                1,
            )
        })
}

/// One finished organization edit as `--json` prints it.
fn committed_document(
    verb: &str,
    target: &str,
    key: &str,
    committed: &Committed,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "verb": verb,
        "target": target,
        "key": key,
        "revision": committed.revision.get(),
        "terminal_id": committed
            .target
            .as_ref()
            .map(|live| phux_client::selector::format_terminal_id(&live.resource)),
    })
}

/// Run `body` on a fresh connection to the server `socket` names.
fn run_on<F>(socket: Option<PathBuf>, verb: &str, json: bool, body: F) -> ExitCode
where
    F: AsyncFnOnce(&ServerTarget, &mut Connection) -> ExitCode,
{
    let (rt, server) = match ServerSpec::local(socket).prepare(verb, json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(async {
        let mut conn = match connect(&server, json, verb).await {
            Ok(conn) => conn,
            Err(code) => return code,
        };
        body(&server, &mut conn).await
    })
}

/// `phux rm TARGET`: remove a resource whose execution has ended, or delete
/// a session with no members. Asks first unless `yes`.
pub(crate) fn run_rm(target: &str, yes: bool, json: bool, socket: Option<PathBuf>) -> ExitCode {
    if let Err(code) = confirm::confirmed(yes, &format!("remove {target}")) {
        return code;
    }
    run_on(socket, "rm", json, async |_, conn| {
        let inventory = match records(conn, "rm").await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let resolved = match resolve(conn, &inventory, target, "rm").await {
            Ok(resolved) => resolved,
            Err(failure) => return failure.emit(json),
        };
        let (result, key, line) = match &resolved {
            Target::Session(record) => (
                org::delete_session(conn, record.key).await,
                key_text(&record.key),
                format!("deleted session {:?}", record.name),
            ),
            Target::Resource(record) => (
                org::remove_resource(conn, record.key).await,
                key_text(&record.key),
                format!("removed {}", key_text(&record.key)),
            ),
        };
        finish(json, "rm", target, &key, result, &line)
    })
}

/// `phux relaunch TARGET`: start a new run of a stopped resource.
pub(crate) fn run_relaunch(target: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
    run_on(socket, "relaunch", json, async |_, conn| {
        let inventory = match records(conn, "relaunch").await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let record = match resolve(conn, &inventory, target, "relaunch").await {
            Ok(Target::Resource(record)) => record,
            Ok(Target::Session(record)) => {
                return Failure::new(
                    codes::WRONG_RESOURCE_KIND,
                    format!(
                        "relaunch: {target} is session {:?}; relaunch takes a pane",
                        record.name
                    ),
                    format!(
                        "relaunch every pane a restart ended with `phux restore {}`",
                        record.name
                    ),
                    1,
                )
                .emit(json);
            }
            Err(failure) => return failure.emit(json),
        };
        let key = key_text(&record.key);
        let result = org::relaunch(conn, record.key).await;
        let line = result
            .as_ref()
            .ok()
            .and_then(|committed| committed.target.as_ref())
            .map_or_else(
                || format!("relaunched {key}"),
                |live| {
                    format!(
                        "relaunched {key} as {}",
                        phux_client::selector::format_terminal_id(&live.resource)
                    )
                },
            );
        finish(json, "relaunch", target, &key, result, &line)
    })
}

/// Print one edit's result: the line (or `--json` document) on success, the
/// failure on error.
fn finish(
    json: bool,
    verb: &str,
    target: &str,
    key: &str,
    result: Result<Committed, OrgError>,
    line: &str,
) -> ExitCode {
    match result {
        Ok(committed) if json => {
            crate::output::json(&committed_document(verb, target, key, &committed))
        }
        Ok(_) => {
            outln!("{line}");
            ExitCode::SUCCESS
        }
        Err(err) => Failure::org(verb, &err).emit(json),
    }
}

/// What `phux restore` relaunches, in record order: every resource a server
/// restart ended ([`OrgInventory::ended_by_restart`]) and every Terminal
/// that was never started (an imported archive's panes), optionally only in
/// `session`.
fn restore_targets(inventory: &OrgInventory, session: Option<&SessionKey>) -> Vec<ResourceKey> {
    let ended: Vec<ResourceKey> = inventory
        .ended_by_restart(session)
        .into_iter()
        .map(|record| record.key)
        .collect();
    inventory
        .resources
        .iter()
        .filter(|record| {
            ended.contains(&record.key)
                || (record.activation.is_none()
                    && record.kind == phux_protocol::ids::ResourceKind::Terminal
                    && session.is_none_or(|key| record.session == *key))
        })
        .map(|record| record.key)
        .collect()
}

/// `phux restore [SESSION]`: relaunch every resource a server restart ended
/// and every Terminal never started ([`restore_targets`]), one result per
/// resource. Exit 0 when every relaunch committed (or there was nothing to
/// relaunch), 2 when any was refused.
pub(crate) fn run_restore(session: Option<&str>, json: bool, socket: Option<PathBuf>) -> ExitCode {
    run_on(socket, "restore", json, async |_, conn| {
        let inventory = match records(conn, "restore").await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let key = match session {
            None => None,
            Some(name) => match inventory.session_named(name) {
                Some(record) => Some(record.key),
                None => {
                    return Failure::new(
                        codes::NO_SUCH_TARGET,
                        format!("no such session: {name}"),
                        "run `phux ls` to see sessions",
                        1,
                    )
                    .emit(json);
                }
            },
        };
        let mut results = Vec::new();
        for target in restore_targets(&inventory, key.as_ref()) {
            let result = org::relaunch(conn, target).await;
            results.push((target, result));
        }
        render_restore(json, &results)
    })
}

/// The `phux restore` output: one line or document entry per resource.
fn render_restore(json: bool, results: &[(ResourceKey, Result<Committed, OrgError>)]) -> ExitCode {
    let failed = results.iter().any(|(_, result)| result.is_err());
    if json {
        let entries: Vec<serde_json::Value> = results
            .iter()
            .map(|(key, result)| match result {
                Ok(committed) => serde_json::json!({
                    "key": key_text(key),
                    "result": "relaunched",
                    "terminal_id": committed
                        .target
                        .as_ref()
                        .map(|live| phux_client::selector::format_terminal_id(&live.resource)),
                }),
                Err(err) => serde_json::json!({
                    "key": key_text(key),
                    "result": "refused",
                    "code": err.code(),
                    "message": err.to_string(),
                }),
            })
            .collect();
        let code = crate::output::json(&serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "restored": entries,
        }));
        return if failed { ExitCode::from(2) } else { code };
    }
    if results.is_empty() {
        outln!("Nothing to restore: no pane was ended by a server restart.");
        return ExitCode::SUCCESS;
    }
    for line in restore_lines(results) {
        outln!("{line}");
    }
    if failed {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn restore_lines(results: &[(ResourceKey, Result<Committed, OrgError>)]) -> Vec<String> {
    results
        .iter()
        .map(|(key, result)| match result {
            Ok(committed) => committed.target.as_ref().map_or_else(
                || format!("{}  relaunched", key_text(key)),
                |live| {
                    format!(
                        "{}  relaunched as {}",
                        key_text(key),
                        phux_client::selector::format_terminal_id(&live.resource)
                    )
                },
            ),
            Err(err) => format!("{}  refused: {err}", key_text(key)),
        })
        .collect()
}

/// What `phux mv`'s destination names.
pub(crate) enum MoveDest {
    /// A session: a membership move ([`run_move_into_session`]).
    Session,
    /// Anything else: a placement beside a pane.
    Pane,
}

/// Whether `dest` names a session `phux mv` moves into, rather than a pane
/// to place beside. A server without `ORGANIZATION` has no membership move,
/// so every destination is a pane there.
pub(crate) fn classify_dest(dest: &str, socket: Option<&std::path::Path>) -> MoveDest {
    let Ok(parsed) = selector::parse(dest) else {
        return MoveDest::Pane;
    };
    if !matches!(parsed, Selector::Session(_) | Selector::Key(_)) {
        return MoveDest::Pane;
    }
    let socket_path = socket.map_or_else(phux_server::runtime::default_socket_path, PathBuf::from);
    let Ok(rt) = crate::commands::cli_runtime() else {
        return MoveDest::Pane;
    };
    rt.block_on(async {
        let Ok(mut conn) = Connection::connect(&socket_path).await else {
            return MoveDest::Pane;
        };
        if !org::supported(&conn) {
            return MoveDest::Pane;
        }
        let Ok(inventory) = org::fetch(&mut conn, OrgFilter::All, 1).await else {
            return MoveDest::Pane;
        };
        let names_session = match &parsed {
            Selector::Session(name) => inventory.session_named(name).is_some(),
            Selector::Key(prefix) => matches!(
                inventory.resolve_key_prefix(prefix),
                Ok(KeyMatch::Session(_))
            ),
            _ => false,
        };
        if names_session {
            MoveDest::Session
        } else {
            MoveDest::Pane
        }
    })
}

/// `phux mv SOURCE SESSION`: make SOURCE a member of SESSION. Membership
/// only: its process survives and it is not placed in SESSION's layout.
pub(crate) fn run_move_into_session(
    source: &str,
    dest: &str,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run_on(socket, "mv", json, async |_, conn| {
        let inventory = match records(conn, "mv").await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let to: SessionKey = match resolve(conn, &inventory, dest, "mv").await {
            Ok(Target::Session(record)) => record.key,
            Ok(Target::Resource(_)) => {
                return Failure::new(
                    codes::NO_SUCH_TARGET,
                    format!("mv: {dest} is not a session"),
                    "name the destination session, or a pane to place beside",
                    1,
                )
                .emit(json);
            }
            Err(failure) => return failure.emit(json),
        };
        let record = match resolve(conn, &inventory, source, "mv").await {
            Ok(Target::Resource(record)) => record,
            Ok(Target::Session(record)) => {
                return Failure::new(
                    codes::WRONG_RESOURCE_KIND,
                    format!("mv: {source} is session {:?}; mv moves a pane", record.name),
                    "name one pane (`@N`, `name:N.M`, or a key `^ab23cd`)",
                    1,
                )
                .emit(json);
            }
            Err(failure) => return failure.emit(json),
        };
        let key = key_text(&record.key);
        let result = org::move_resource(conn, record.key, to).await;
        let line = format!("moved {key} into session {dest:?}");
        finish(json, "mv", source, &key, result, &line)
    })
}

/// Whether `target` names a pane, so `phux rename` labels a resource rather
/// than renaming a session: any selector but a bare session name.
pub(crate) fn names_resource(target: &str) -> bool {
    !matches!(selector::parse(target), Ok(Selector::Session(_)) | Err(_))
}

/// `phux rename PANE NEW_NAME`: set a resource's label.
pub(crate) fn run_rename_resource(target: &str, new_name: &str, server: ServerSpec) -> ExitCode {
    let (rt, server) = match server.prepare("rename", false) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(async {
        let mut conn = match connect(&server, false, "rename").await {
            Ok(conn) => conn,
            Err(code) => return code,
        };
        let inventory = match records(&mut conn, "rename").await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(false),
        };
        let record = match resolve(&mut conn, &inventory, target, "rename").await {
            Ok(Target::Resource(record)) => record,
            Ok(Target::Session(record)) => {
                // A key prefix can name a session: rename it as one.
                drop(conn);
                return crate::commands::rename::run_rename_on(&server, &record.name, new_name)
                    .await;
            }
            Err(failure) => return failure.emit(false),
        };
        let key = key_text(&record.key);
        let result = org::rename_resource(&mut conn, record.key, Some(new_name)).await;
        drop(conn);
        finish(
            false,
            "rename",
            target,
            &key,
            result,
            &format!("renamed {key} to {new_name:?}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_print_as_their_six_digit_prefix() {
        assert_eq!(short_key("abcdefghij"), "abcdef");
        assert_eq!(short_key("abc"), "abc");
        let key = ResourceKey::new([7; 16]).expect("non-zero");
        let text = key_text(&key);
        assert!(text.starts_with('^'));
        assert_eq!(text.len(), 1 + KEY_MIN_PREFIX);
    }

    /// A bare session name renames the session; every pane form labels a
    /// resource.
    #[test]
    fn rename_tells_a_pane_from_a_session() {
        assert!(!names_resource("work"));
        assert!(names_resource("@3"));
        assert!(names_resource("work:0.1"));
        assert!(names_resource("^ab23cd"));
        assert!(!names_resource(""));
    }

    #[test]
    fn restore_prints_one_line_per_pane() {
        use phux_protocol::ids::OperationId;
        use phux_protocol::wire::frame::Refusal;
        let ok = ResourceKey::new([1; 16]).expect("non-zero");
        let refused = ResourceKey::new([2; 16]).expect("non-zero");
        let operation = OperationId::new([3; 16]).expect("non-zero");
        let results = vec![
            (
                ok,
                Ok(Committed::new(
                    operation,
                    phux_protocol::ids::Revision::new(2),
                )),
            ),
            (refused, Err(OrgError::Refused(Refusal::ExecutionUncertain))),
        ];
        let lines = restore_lines(&results);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("relaunched"), "{lines:?}");
        assert!(lines[1].contains("refused"), "{lines:?}");
    }
}
