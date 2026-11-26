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
use phux_client::org::{self, KeyMatch, KeyNames, OrgError, OrgInventory};
use phux_client::selector::{self, Selector};
use phux_protocol::ids::{ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Committed, OrgFilter, ResourceRecord, SessionRecord};

use crate::commands::confirm;
use crate::commands::json_err::{Refusal, codes};
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

mod lock;
mod raw_relaunch;
mod restore_report;
mod targeting;
pub(crate) use lock::run_lock;
pub(crate) use targeting::resolve;
use targeting::{connect, key_text, records};

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
    run_on_server(ServerSpec::local(socket), verb, json, body)
}

/// Run `body` on a fresh connection to the server `spec` names, local or
/// `--remote`.
fn run_on_server<F>(spec: ServerSpec, verb: &str, json: bool, body: F) -> ExitCode
where
    F: AsyncFnOnce(&ServerTarget, &mut Connection) -> ExitCode,
{
    let (rt, server) = match spec.prepare(verb, json) {
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
    if let Err(code) = confirm::confirmed_for(json, yes, &format!("remove {target}")) {
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
                key_text(&inventory, *record.key.as_bytes()),
                format!("deleted session {:?}", record.name),
            ),
            Target::Resource(record) => (
                org::remove_resource(conn, record.key).await,
                key_text(&inventory, *record.key.as_bytes()),
                format!("removed {target}"),
            ),
        };
        finish(json, "rm", target, &key, result, &line)
    })
}

/// `phux project link SESSION NAME` (`project: Some`) and `phux project
/// unlink SESSION` (`None`): set or clear the session's project link on the
/// server that owns it (`SESSION_SET_PROJECT`, last writer wins). Every
/// client groups sessions across hosts by that link. The name is trimmed
/// first. Exit 0 success, 1 no server or no such session, 2 refusal or a
/// server without `SESSION_PROJECT`.
pub(crate) fn run_set_project(
    target: &str,
    project: Option<&str>,
    json: bool,
    server: ServerSpec,
) -> ExitCode {
    let project = project.map(str::trim);
    let verb = if project.is_some() {
        "project link"
    } else {
        "project unlink"
    };
    run_on_server(server, verb, json, async |_, conn| {
        let inventory = match records(conn, verb).await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let record = match resolve(conn, &inventory, target, verb).await {
            Ok(Target::Session(record)) => record,
            Ok(Target::Resource(_)) => {
                return Refusal::new(
                    codes::WRONG_RESOURCE_KIND,
                    format!("{verb}: {target} is a pane; a project link names a session"),
                    "name the session (`phux ls` lists them)",
                    1,
                )
                .emit(json);
            }
            Err(failure) => return failure.emit(json),
        };
        let key = key_text(&inventory, *record.key.as_bytes());
        match org::set_session_project(conn, record.key, project).await {
            Ok(committed) if json => {
                let mut document = committed_document(verb, target, &key, &committed);
                document["session"] = serde_json::json!(record.name);
                document["project"] = serde_json::json!(project);
                crate::output::json(&document)
            }
            Ok(_) => {
                match project {
                    Some(project) => {
                        outln!("linked session {:?} to project {project:?}", record.name);
                    }
                    None => outln!("unlinked session {:?} from its project", record.name),
                }
                ExitCode::SUCCESS
            }
            Err(err) => Refusal::org(verb, &err).emit(json),
        }
    })
}

/// `phux relaunch TARGET`: start a new run of a stopped resource.
pub(crate) fn run_relaunch(
    target: &str,
    approve_raw: bool,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run_on(socket, "relaunch", json, async |_, conn| {
        let inventory = match records(conn, "relaunch").await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let record = match resolve(conn, &inventory, target, "relaunch").await {
            Ok(Target::Resource(record)) => record,
            Ok(Target::Session(record)) => {
                return Refusal::new(
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
        let key = key_text(&inventory, *record.key.as_bytes());
        if approve_raw {
            return raw_relaunch::run(conn, &record, target, json).await;
        }
        let result = org::relaunch(conn, record.key).await;
        let line = result
            .as_ref()
            .ok()
            .and_then(|committed| committed.target.as_ref())
            .map_or_else(
                || format!("relaunched {target}"),
                |live| {
                    format!(
                        "relaunched {target} as {}",
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
        Err(err) => raw_relaunch::refusal(verb, target, &err).emit(json),
    }
}

/// What `phux restore` relaunches ([`OrgInventory::restorable`]).
fn restore_targets(inventory: &OrgInventory, session: Option<&SessionKey>) -> Vec<ResourceKey> {
    inventory
        .restorable(session)
        .into_iter()
        .map(|record| record.key)
        .collect()
}

/// `phux restore [SESSION]`: relaunch every resource a server restart ended
/// and every Terminal never started ([`restore_targets`]): one summary line,
/// plus one per refusal (`verbose`: one per resource). Exit 0 when every
/// relaunch committed (or there was nothing to relaunch), 2 when any was
/// refused.
pub(crate) fn run_restore(
    session: Option<&str>,
    verbose: bool,
    dry_run: bool,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
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
                    return Refusal::new(
                        codes::NO_SUCH_TARGET,
                        format!("no such session: {name}"),
                        "run `phux ls` to see sessions",
                        1,
                    )
                    .emit(json);
                }
            },
        };
        let targets = restore_targets(&inventory, key.as_ref());
        if dry_run {
            let resumes = super::program_resume::fetch(conn).await;
            return restore_report::render_dry_run(
                &KeyNames::of_inventory(&inventory),
                &targets,
                &resumes,
                json,
            );
        }
        let mut results = Vec::new();
        for target in targets {
            let result = org::relaunch(conn, target).await;
            results.push((target, result));
        }
        if json {
            return restore_report::restore_document(&KeyNames::of_inventory(&inventory), &results);
        }
        restore_report::render_restore(
            &KeyNames::of_inventory(&inventory),
            &results,
            session,
            verbose,
        )
    })
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
    let socket_path = socket.map_or_else(phux_config::socket::default_socket_path, PathBuf::from);
    let Ok(rt) = crate::commands::cli_runtime() else {
        return MoveDest::Pane;
    };
    rt.block_on(async {
        let Ok(mut conn) = Connection::connect(&socket_path).await else {
            return MoveDest::Pane;
        };
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
                return Refusal::new(
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
                return Refusal::new(
                    codes::WRONG_RESOURCE_KIND,
                    format!("mv: {source} is session {:?}; mv moves a pane", record.name),
                    "name one pane (`@N`, `name:N.M`, or a key `^ab23cd`)",
                    1,
                )
                .emit(json);
            }
            Err(failure) => return failure.emit(json),
        };
        let key = key_text(&inventory, *record.key.as_bytes());
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
pub(crate) fn run_rename_resource(
    target: &str,
    new_name: &str,
    json: bool,
    server: ServerSpec,
) -> ExitCode {
    let (rt, server) = match server.prepare("rename", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(async {
        let mut conn = match connect(&server, json, "rename").await {
            Ok(conn) => conn,
            Err(code) => return code,
        };
        let inventory = match records(&mut conn, "rename").await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let record = match resolve(&mut conn, &inventory, target, "rename").await {
            Ok(Target::Resource(record)) => record,
            Ok(Target::Session(record)) => {
                // A key prefix can name a session: rename it as one.
                drop(conn);
                return crate::commands::rename::run_rename_on(
                    &server,
                    &record.name,
                    new_name,
                    json,
                )
                .await;
            }
            Err(failure) => return failure.emit(json),
        };
        let key = key_text(&inventory, *record.key.as_bytes());
        let result = org::rename_resource(&mut conn, record.key, Some(new_name)).await;
        drop(conn);
        finish(
            json,
            "rename",
            target,
            &key,
            result,
            &format!("renamed {key} to {new_name:?}"),
        )
    })
}

#[cfg(test)]
mod tests;
