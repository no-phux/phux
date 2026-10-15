//! `phux_rm`, `phux_relaunch`, and `phux_organize`: the organization edits
//! `phux rm`, `phux relaunch`, and `phux organize` make, over
//! [`phux_client::org`] and [`phux_client::organize`]. Each returns the
//! document the CLI prints under `--json`.
//!
//! A target is a key prefix (`^ab23cd`) resolved against the records, a
//! session name, or any other selector resolved against a fresh snapshot to
//! one Terminal and then to the record executing as it.

use phux_client::attach::connection::Connection;
use phux_client::org::{self, KeyMatch, KeyNames, OrgError, OrgInventory};
use phux_client::selector::Selector;
use phux_protocol::wire::frame::{Committed, OrgFilter, ResourceRecord, SessionRecord};
use serde_json::{Value, json};

use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, parse_selector, socket_arg, strict_object};

pub(crate) fn schemas() -> Vec<Value> {
    vec![rm_schema(), relaunch_schema(), organize_schema()]
}

pub(crate) fn owns(name: &str) -> bool {
    matches!(name, "phux_rm" | "phux_relaunch" | "phux_organize")
}

pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "phux_rm" => rm(args).await,
        "phux_relaunch" => relaunch(args).await,
        "phux_organize" => organize(args).await,
        other => Err(ToolError::new(format!(
            "unknown organization tool: {other}"
        ))),
    }
}

fn rm_schema() -> Value {
    schema(
        "phux_rm",
        "Remove a pane whose process has ended, or delete a session with no members. A pane \
         that still runs, or a session that still has members, is refused: stop it with \
         phux_kill first. Requires confirm=true.",
        json!({
            "target": { "type": "string", "description": "Pane selector, session name, or key `^ab23cd`." },
            "confirm": { "type": "boolean", "const": true, "description": "Required explicit confirmation." },
            "socket": string_schema(),
        }),
        &["target", "confirm"],
    )
}

fn relaunch_schema() -> Value {
    schema(
        "phux_relaunch",
        "Start a stopped pane again, with the command, directory, and environment it was \
         launched with, in the same session. Refused while it runs.",
        json!({
            "target": { "type": "string", "description": "Pane selector or key `^ab23cd`." },
            "socket": string_schema(),
        }),
        &["target"],
    )
}

fn organize_schema() -> Value {
    schema(
        "phux_organize",
        "Put every pane in the session it belongs in: panes join the session for their \
         repository, a repository with no session gets one, and a numbered session holding one \
         repository takes its name. Membership only; every process keeps running. With \
         TYPESAFE_API_KEY set, TypeSafe's Jev judges each pane from its name, title, directory, \
         and repository (never screen contents); otherwise repository matching decides. Use \
         dry_run to see the plan first; `phux organize --undo` reverses a run.",
        json!({
            "session": { "type": "string", "description": "Only this session's panes may move." },
            "dry_run": { "type": "boolean", "description": "Return the plan without changing anything." },
            "offline": { "type": "boolean", "description": "Repository matching only; send nothing off the machine." },
            "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Confidence a judgment needs to move a pane (default 0.6)." },
            "socket": string_schema(),
        }),
        &[],
    )
}

/// `phux_organize`'s arguments, validated before any dial.
struct OrganizeArgs {
    session: Option<String>,
    dry_run: bool,
    offline: bool,
    min_confidence: f64,
}

fn organize_args(args: &Value) -> Result<OrganizeArgs, ToolError> {
    strict_object(
        args,
        &["session", "dry_run", "offline", "min_confidence", "socket"],
        &[],
    )?;
    let min_confidence = match args.get("min_confidence") {
        None => phux_client::organize::DEFAULT_MIN_CONFIDENCE,
        Some(value) => value
            .as_f64()
            .filter(|p| (0.0..=1.0).contains(p))
            .ok_or_else(|| ToolError::new("`min_confidence` must be a number from 0 to 1"))?,
    };
    Ok(OrganizeArgs {
        session: crate::cli_adapter::bounded_string(args, "session", false)?,
        dry_run: bool_arg(args, "dry_run")?,
        offline: bool_arg(args, "offline")?,
        min_confidence,
    })
}

fn bool_arg(args: &Value, name: &str) -> Result<bool, ToolError> {
    match args.get(name) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::new(format!("`{name}` must be a boolean"))),
    }
}

/// `phux_organize`: what `phux organize --json` prints.
async fn organize(args: &Value) -> Result<Value, ToolError> {
    use phux_client::organize::{self, Judge, Options};
    let parsed = organize_args(args)?;
    let socket = socket_arg(args)?;
    let mut conn = Connection::connect(&socket).await?;
    let config = phux_config::loader::load().unwrap_or_default();
    let (judge, key_error) = Judge::configured(&config.organize, parsed.offline).await;
    let options = Options {
        session: parsed.session,
        judge,
        min_confidence: parsed.min_confidence,
        jev_timeout: organize::JEV_TIMEOUT,
    };
    let host = phux_config::instance::local_host_name();
    let mut outcome = organize::run(&mut conn, Some(&host), &options, parsed.dry_run)
        .await
        .map_err(|err| ToolError::new(format!("organize: {err} ({})", err.code())))?;
    drop(conn);
    outcome.judge_error = outcome.judge_error.or(key_error);
    Ok(organize::document(
        &outcome.plan,
        outcome.applied.as_ref(),
        outcome.judge_error.as_deref(),
        parsed.dry_run,
    ))
}

/// What a target names.
enum Target {
    Session(SessionRecord),
    Resource(ResourceRecord),
}

fn org_error(verb: &str, err: &OrgError) -> ToolError {
    ToolError::new(format!("{verb}: {err} ({})", err.code()))
}

async fn records(conn: &mut Connection, verb: &str) -> Result<OrgInventory, ToolError> {
    if !org::supported(conn) {
        return Err(org_error(verb, &OrgError::Unsupported));
    }
    org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(|err| org_error(verb, &err))
}

async fn resolve(
    conn: &mut Connection,
    inventory: &OrgInventory,
    text: &str,
    verb: &str,
) -> Result<Target, ToolError> {
    let parsed = parse_selector(text)?;
    match &parsed {
        Selector::Key(prefix) => {
            return match inventory.resolve_key_prefix(prefix) {
                Ok(KeyMatch::Session(record)) => Ok(Target::Session(record)),
                Ok(KeyMatch::Resource(record)) => Ok(Target::Resource(record)),
                Err(err) => Err(org_error(verb, &err)),
            };
        }
        Selector::Session(name) => {
            if let Some(record) = inventory.session_named(name) {
                return Ok(Target::Session(record.clone()));
            }
        }
        _ => {}
    }
    let (snapshot, _) = phux_client::state::get_state_on(conn).await?.into_parts();
    let ids = phux_client::state::resolve_targets_on(conn, &parsed, &snapshot)
        .await
        .map_err(|err| crate::tools::agent_resolve_error(&err))?;
    let [id] = ids.as_slice() else {
        return Err(ToolError::new(if ids.is_empty() {
            format!("no such target: {text}")
        } else {
            format!("'{text}' names {} panes; {verb} takes one", ids.len())
        }));
    };
    inventory
        .resource_by_live(id)
        .cloned()
        .map(Target::Resource)
        .ok_or_else(|| ToolError::new(format!("{text} has no record on this server")))
}

/// `^` and the shortest prefix of `key` no other record of `inventory`
/// shares, the display `phux rm` and `phux relaunch` print ([`KeyNames`]).
fn key_text(inventory: &OrgInventory, key: [u8; 16]) -> String {
    KeyNames::of_inventory(inventory).display(key)
}

fn document(verb: &str, target: &str, key: &str, committed: &Committed) -> Value {
    json!({
        "schema_version": 1,
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

/// `phux_rm`: what `phux rm --json` prints.
async fn rm(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "confirm", "socket"],
        &["target", "confirm"],
    )?;
    let target = crate::cli_adapter::bounded_string(args, "target", true)?.unwrap_or_default();
    let socket = socket_arg(args)?;
    let mut conn = Connection::connect(&socket).await?;
    let inventory = records(&mut conn, "rm").await?;
    let (result, key) = match resolve(&mut conn, &inventory, &target, "rm").await? {
        Target::Session(record) => (
            org::delete_session(&mut conn, record.key).await,
            key_text(&inventory, *record.key.as_bytes()),
        ),
        Target::Resource(record) => (
            org::remove_resource(&mut conn, record.key).await,
            key_text(&inventory, *record.key.as_bytes()),
        ),
    };
    drop(conn);
    let committed = result.map_err(|err| org_error("rm", &err))?;
    Ok(document("rm", &target, &key, &committed))
}

/// `phux_relaunch`: what `phux relaunch --json` prints.
async fn relaunch(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &["target"])?;
    let target = crate::cli_adapter::bounded_string(args, "target", true)?.unwrap_or_default();
    let socket = socket_arg(args)?;
    let mut conn = Connection::connect(&socket).await?;
    let inventory = records(&mut conn, "relaunch").await?;
    let record = match resolve(&mut conn, &inventory, &target, "relaunch").await? {
        Target::Resource(record) => record,
        Target::Session(record) => {
            return Err(ToolError::new(format!(
                "relaunch: {target} is session {:?}; relaunch takes a pane",
                record.name
            )));
        }
    };
    let key = key_text(&inventory, *record.key.as_bytes());
    let result = org::relaunch(&mut conn, record.key).await;
    drop(conn);
    let committed = result.map_err(|err| org_error("relaunch", &err))?;
    Ok(document("relaunch", &target, &key, &committed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn both_tools_refuse_malformed_arguments_before_any_dial() {
        for (name, args) in [
            ("phux_rm", json!({ "target": "@1" })),
            (
                "phux_rm",
                json!({ "target": "@1", "confirm": true, "extra": 1 }),
            ),
            ("phux_relaunch", json!({})),
            ("phux_organize", json!({ "dry_run": "yes" })),
            ("phux_organize", json!({ "min_confidence": 2 })),
            ("phux_organize", json!({ "extra": 1 })),
            ("phux_relaunch", json!({ "target": 3 })),
        ] {
            assert!(call(name, &args).await.is_err(), "{name} accepted {args}");
        }
    }
}
