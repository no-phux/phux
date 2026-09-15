//! The `phux_resource_*` tools (PHA-406): one resource's record, the methods
//! it answers on this server, and a bounded, resumable wait on its exit.
//!
//! In-process over `phux_client::resource`, returning the same documents
//! `phux resource show|methods|wait --json` prints, so the two surfaces
//! cannot drift. `phux_resource_wait` is always bounded (`timeout_secs` is
//! required), and a timeout is a result (`outcome: "timed_out"` with the
//! cursor reached), not a tool error.

use std::path::PathBuf;
use std::time::Duration;

use phux_client::deadline::Deadline;
use phux_client::resource::LookupError;
use phux_client::resource::cursor::Cursor;
use phux_client::selector::{self, Selector};
use phux_client::state;
use phux_protocol::ids::ResourceId;
use serde_json::{Value, json};

use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, resolve_one, str_arg, strict_object};

/// Upper bound on `phux_resource_wait`'s `timeout_secs`, as `phux_run`.
const WAIT_MAX_SECS: u64 = 3600;

/// The three resource tool descriptors.
pub(crate) fn schemas() -> Vec<Value> {
    vec![show_schema(), wait_schema(), methods_schema()]
}

/// Whether `name` is one of this module's tools.
pub(crate) fn owns(name: &str) -> bool {
    matches!(
        name,
        "phux_resource_show" | "phux_resource_wait" | "phux_resource_methods"
    )
}

/// Dispatch one resource tool call.
pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "phux_resource_show" => show(args).await,
        "phux_resource_wait" => wait(args).await,
        "phux_resource_methods" => methods(args).await,
        other => Err(ToolError::new(format!("unknown resource tool: {other}"))),
    }
}

fn target_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 4096,
        "description": "Resource selector. A direct id (`@N`, `host/@N`) is used as given, so a resource that already exited can still be named; any other selector resolves against the live inventory.",
    })
}

fn show_schema() -> Value {
    schema(
        "phux_resource_show",
        "Read one resource: kind, parent, lifecycle, how a retained process exited, process facts (pid, foreground group, cwd, prompt state), input-lease holder, tags, and agent record. Read-only; the document `phux resource show --json` prints.",
        json!({ "target": target_schema(), "socket": string_schema() }),
        &["target"],
    )
}

fn wait_schema() -> Value {
    schema(
        "phux_resource_wait",
        "Wait until a resource's process exits, race-free. Always bounded: `timeout_secs` is required (1..=3600). Returns `outcome` (exited, gone, or timed_out), the exit status or signal, whether the resource is still retained, and a `cursor`; pass the cursor back as `after` to resume without missing an exit. A timeout is a result, not a tool error. The document `phux resource wait --json` prints.",
        json!({
            "target": target_schema(),
            "timeout_secs": { "type": "integer", "minimum": 1, "maximum": WAIT_MAX_SECS },
            "after": {
                "type": "string",
                "minLength": 3,
                "maxLength": 4096,
                "description": "The `cursor` a previous phux_resource_wait (or `phux watch`) returned.",
            },
            "socket": string_schema(),
        }),
        &["target", "timeout_secs"],
    )
}

fn methods_schema() -> Value {
    schema(
        "phux_resource_methods",
        "List the methods a resource answers on this server: each method's verb, whether it changes state, and whether it is available here, or why not (feature_unadvertised, wrong_kind, transport, unimplemented). Listing grants nothing. The document `phux resource methods --json` prints.",
        json!({ "target": target_schema(), "socket": string_schema() }),
        &["target"],
    )
}

async fn show(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &["target"])?;
    let (socket, id) = target(args).await?;
    phux_client::resource::show::show(&socket, &id)
        .await
        .map(|details| details.to_json())
        .map_err(|err| lookup_error(&err))
}

async fn methods(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &["target"])?;
    let (socket, id) = target(args).await?;
    phux_client::resource::methods::methods(&socket, &id)
        .await
        .map(|report| report.to_json())
        .map_err(|err| lookup_error(&err))
}

async fn wait(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "timeout_secs", "after", "socket"],
        &["target", "timeout_secs"],
    )?;
    let secs = args
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .filter(|secs| (1..=WAIT_MAX_SECS).contains(secs))
        .ok_or_else(|| ToolError::new("`timeout_secs` must be an integer in 1..=3600"))?;
    let after = str_arg(args, "after")
        .map(str::parse::<Cursor>)
        .transpose()
        .map_err(|err| ToolError::new(err.to_string()))?;
    let deadline = Deadline::new(Some(Duration::from_secs(secs)));
    let (socket, id) = target(args).await?;
    phux_client::resource::wait::wait_for_exit(&socket, id, after.as_ref(), deadline)
        .await
        .map(|wait| wait.to_json())
        .map_err(|err| {
            ToolError::new(err.cursor.as_ref().map_or_else(
                || err.to_string(),
                |cursor| format!("{err} (resume with `after`: {cursor})"),
            ))
        })
}

/// The socket and the one resource `target` names. A direct id is taken as
/// given (no lookup), like the CLI, so an exited resource can be named.
async fn target(args: &Value) -> Result<(PathBuf, ResourceId), ToolError> {
    let socket = crate::socket::resolve(str_arg(args, "socket"));
    let raw = str_arg(args, "target")
        .ok_or_else(|| ToolError::new("missing required string `target`"))?;
    let selector = selector::parse(raw)
        .map_err(|err| ToolError::new(format!("invalid target '{raw}': {err}")))?;
    if let Some(id) = direct_id(&selector) {
        return Ok((socket, id));
    }
    let view = state::get_state(&socket).await?;
    let id = resolve_one(&socket, &selector, &view).await?;
    Ok((socket, id))
}

fn direct_id(selector: &Selector) -> Option<ResourceId> {
    match selector {
        Selector::ResourceId(id) => Some(ResourceId::local(*id)),
        Selector::SatelliteResourceId { host, id } => {
            Some(ResourceId::satellite(host.as_str(), *id))
        }
        _ => None,
    }
}

fn lookup_error(err: &LookupError) -> ToolError {
    match err {
        LookupError::NotFound { unreachable, .. } if !unreachable.is_empty() => {
            ToolError::new(format!(
                "{err}; this server's view of the fleet is incomplete ({}), so the \
                 resource may exist on an unreachable satellite",
                unreachable.join("; ")
            ))
        }
        _ => ToolError::new(err.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::frame::ResourceLifecycle;
    use phux_protocol::wire::info::{ExitFacet, ResourceInfo, SessionSnapshot};
    use tokio::net::UnixListener;

    use super::*;

    fn retained() -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(7), WindowId::new(1), 80, 24)
                    .with_lifecycle(ResourceLifecycle::Exited)
                    .with_exit(Some(ExitFacet::new(5, 10).with_exit_status(Some(42)))),
            ])
    }

    #[test]
    fn every_resource_schema_is_strict_and_bounded() {
        for descriptor in schemas() {
            assert!(owns(descriptor["name"].as_str().unwrap()));
            assert_eq!(descriptor["inputSchema"]["additionalProperties"], false);
            assert!(
                descriptor["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("target"))
            );
        }
        let wait = wait_schema();
        assert_eq!(
            wait["inputSchema"]["required"],
            json!(["target", "timeout_secs"]),
            "the wait is always bounded"
        );
    }

    #[tokio::test]
    async fn the_wait_refuses_an_unbounded_or_malformed_call_before_any_connection() {
        for args in [
            json!({ "target": "@7" }),
            json!({ "target": "@7", "timeout_secs": 0 }),
            json!({ "target": "@7", "timeout_secs": 3601 }),
            json!({ "target": "@7", "timeout_secs": 5, "after": "nope" }),
            json!({ "target": "@7", "timeout_secs": 5, "extra": true }),
        ] {
            let socket = "/nonexistent/phux.sock";
            let mut args = args;
            args["socket"] = json!(socket);
            assert!(wait(&args).await.is_err(), "{args} must be refused");
        }
    }

    #[tokio::test]
    async fn the_wait_tool_returns_the_cli_document() {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("mcp-wait.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let spec = ScriptSpec::new().state(retained());
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        let doc = call(
            "phux_resource_wait",
            &json!({ "target": "@7", "timeout_secs": 20, "socket": socket.to_str().unwrap() }),
        )
        .await
        .expect("wait");
        server.await.expect("scripted server");
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["outcome"], "exited");
        assert_eq!(doc["exit"]["status"], 42);
        assert_eq!(doc["retained"], true);
    }
}
