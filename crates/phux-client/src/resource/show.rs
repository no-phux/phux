//! `phux resource show` (PHA-406): one resource's inspection record.
//!
//! Everything a caller would otherwise stitch together from four verbs, read
//! on one connection: the inventory entry (kind, parent, lifecycle, the exit
//! facet of a retained resource, the input-lease holder), the Terminal's
//! typed process facet from `GET_TERMINAL_STATE`, its tags, and its
//! declared agent record. Read-only: no attach, no resize, no subscription.

use std::path::Path;

use phux_core::process::TerminalProcessState;
use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, RESOURCE_TAGS_KEY, Scope};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use serde_json::{Value, json};

use super::LookupError;
use crate::agent_meta::AgentRecord;
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::format_terminal_id;
use crate::state::Degradation;

/// `schema_version` of the `resource show --json` document.
pub const SHOW_SCHEMA_VERSION: u32 = 1;

/// One resource's inspection record.
#[derive(Debug, Clone)]
pub struct ResourceDetails {
    /// The inventory entry.
    pub info: ResourceInfo,
    /// The name of the session whose window holds it, for a Terminal.
    pub session: Option<String>,
    /// The Terminal's process facet; `None` for another kind, or when the
    /// server could not answer `GET_TERMINAL_STATE`.
    pub process: Option<TerminalProcessState>,
    /// Its `phux.tags/v1` tags; `None` when the read was refused.
    pub tags: Option<Vec<String>>,
    /// The Terminal's declared `phux.agent/v1` record, if any.
    pub agent: Option<AgentRecord>,
    /// Degradation notices collected along the way.
    pub unreachable: Vec<String>,
}

impl ResourceDetails {
    /// The `resource show --json` document, shared by the CLI and MCP.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let info = &self.info;
        json!({
            "schema_version": SHOW_SCHEMA_VERSION,
            "resource": format_terminal_id(&info.id),
            "kind": super::kind_name(info.kind),
            "parent": info.parent.as_ref().map(format_terminal_id),
            "session": self.session,
            "title": info.title,
            "cwd": info.cwd,
            "lifecycle": super::lifecycle_name(info.lifecycle),
            "exit": info.exit.as_ref().map(super::exit_facet_json),
            "input_holder": info.input_holder.map(ClientId::get),
            "process": self.process,
            "tags": self.tags,
            "agent": self.agent,
            "agent_session": info.agent.as_ref().map(|facet| json!({
                "provider": facet.provider,
                "native_id": facet.native_id,
                "state": facet.state,
            })),
            "unreachable": self.unreachable,
        })
    }
}

/// Read `resource`'s inspection record over a fresh connection.
///
/// # Errors
///
/// [`LookupError::NotFound`] when the resource is not in the inventory, or
/// [`LookupError::Attach`] on a transport failure.
pub async fn show(socket: &Path, resource: &ResourceId) -> Result<ResourceDetails, LookupError> {
    let mut conn = Connection::connect(socket).await?;
    let (info, snapshot, mut unreachable) = super::lookup_on(&mut conn, resource).await?;
    let is_terminal = super::is_terminal(&info);
    let process = if is_terminal {
        terminal_process(&mut conn, resource, &mut unreachable).await?
    } else {
        None
    };
    let tags = read_tags(&mut conn, resource, &mut unreachable).await?;
    let agent = if is_terminal {
        read_agent(&mut conn, resource, &mut unreachable).await?
    } else {
        None
    };
    drop(conn);
    Ok(ResourceDetails {
        session: session_name(&snapshot, &info),
        info,
        process,
        tags,
        agent,
        unreachable,
    })
}

fn session_name(snapshot: &SessionSnapshot, info: &ResourceInfo) -> Option<String> {
    let window = snapshot.windows.iter().find(|w| w.id == info.window_id)?;
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == window.session_id)
        .map(|s| s.name.clone())
}

async fn terminal_process(
    conn: &mut Connection,
    resource: &ResourceId,
    notices: &mut Vec<String>,
) -> Result<Option<TerminalProcessState>, AttachError> {
    let (result, interleaved) = conn
        .request(
            1,
            Command::GetTerminalState {
                terminal_id: resource.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
        )
        .await?
        .into_parts();
    notices.extend_from_slice(Degradation::from_interleaved(&interleaved).notices());
    let CommandResult::OkWith(CommandValue::Json(json)) = result else {
        return Ok(None);
    };
    Ok(parse_process(&json))
}

/// The `process` object of a `GET_TERMINAL_STATE` document, or `None` when
/// it is absent (an older server) or unreadable.
fn parse_process(json: &str) -> Option<TerminalProcessState> {
    let document: Value = serde_json::from_str(json).ok()?;
    serde_json::from_value(document.get("process")?.clone()).ok()
}

async fn read_tags(
    conn: &mut Connection,
    resource: &ResourceId,
    notices: &mut Vec<String>,
) -> Result<Option<Vec<String>>, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            2,
            Scope::Resource(resource.clone()),
            RESOURCE_TAGS_KEY.to_owned(),
        )
        .await?
        .into_parts();
    notices.extend_from_slice(Degradation::from_interleaved(&interleaved).notices());
    Ok(match answer {
        Ok(Some(bytes)) => Some(serde_json::from_slice(&bytes).unwrap_or_default()),
        Ok(None) => Some(Vec::new()),
        Err(_) => None,
    })
}

async fn read_agent(
    conn: &mut Connection,
    resource: &ResourceId,
    notices: &mut Vec<String>,
) -> Result<Option<AgentRecord>, AttachError> {
    let (answer, degradation) = crate::agent_record::get_record(conn, resource, 3).await?;
    notices.extend_from_slice(degradation.notices());
    Ok(answer.ok().flatten())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_protocol::ids::{ClientId, SessionId, WindowId};
    use phux_protocol::wire::frame::{CloseReason, ResourceLifecycle};
    use phux_protocol::wire::info::{ExitFacet, SessionInfo, WindowInfo};
    use tokio::net::UnixListener;

    use super::*;
    use crate::agent_meta::RESOURCE_AGENT_KEY;
    use crate::testkit::{ScriptSpec, ScriptedServer};

    fn fixture() -> SessionSnapshot {
        let session = SessionId::new(1);
        let window = WindowId::new(2);
        SessionSnapshot::new(session, window, ResourceId::local(7))
            .with_sessions(vec![SessionInfo::new(session, "work")])
            .with_windows(vec![WindowInfo::new(window, session, "one")])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(7), window, 80, 24)
                    .with_lifecycle(ResourceLifecycle::Exited)
                    .with_exit(Some(
                        ExitFacet::new(10, 20)
                            .with_signal(Some(9))
                            .with_reason(CloseReason::Exited),
                    ))
                    .with_input_holder(Some(ClientId::new(4))),
            ])
    }

    async fn show_against(
        spec: ScriptSpec,
        id: ResourceId,
    ) -> Result<ResourceDetails, LookupError> {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("show.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        let result = show(&socket, &id).await;
        server.await.expect("scripted server");
        result
    }

    #[tokio::test]
    async fn resource_show_reads_lifecycle_process_tags_and_agent() {
        let spec = ScriptSpec::new()
            .state(fixture())
            .terminal_state(&json!({
                "schema_version": 1,
                "process": {
                    "child": { "pid": 42, "start_ms": 7 },
                    "foreground": null,
                    "cwd": "/repo",
                    "prompt": { "state": "unknown", "last_exit_code": null },
                    "exit": { "status": null, "signal": 9, "reason": "exited", "exited_at_ms": 10 }
                }
            }))
            .metadata(|_, key| match key {
                RESOURCE_TAGS_KEY => Some(br#"["build"]"#.to_vec()),
                RESOURCE_AGENT_KEY => Some(br#"{"name":"reviewer","state":"idle"}"#.to_vec()),
                _ => None,
            });
        let details = show_against(spec, ResourceId::local(7))
            .await
            .expect("show");
        let doc = details.to_json();
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["resource"], "@7");
        assert_eq!(doc["kind"], "terminal");
        assert_eq!(doc["session"], "work");
        assert_eq!(doc["lifecycle"], "exited");
        assert_eq!(doc["exit"]["signal"], 9);
        assert_eq!(doc["exit"]["reason"], "exited");
        assert_eq!(doc["exit"]["retained_until_ms"], 20);
        assert_eq!(doc["input_holder"], 4);
        assert_eq!(doc["process"]["child"]["pid"], 42);
        assert_eq!(doc["process"]["cwd"], "/repo");
        assert_eq!(doc["tags"], json!(["build"]));
        assert_eq!(doc["agent"]["name"], "reviewer");
        assert!(doc["agent_session"].is_null());
    }

    #[tokio::test]
    async fn an_older_server_without_a_process_facet_reads_as_null() {
        let spec = ScriptSpec::new().state(fixture());
        let details = show_against(spec, ResourceId::local(7))
            .await
            .expect("show");
        assert!(details.process.is_none());
        assert!(details.to_json()["process"].is_null());
        assert_eq!(details.to_json()["tags"], json!([]));
    }

    #[tokio::test]
    async fn an_absent_resource_is_not_found() {
        let spec = ScriptSpec::new().state(fixture());
        let err = show_against(spec, ResourceId::local(99))
            .await
            .expect_err("absent");
        assert!(matches!(err, LookupError::NotFound { .. }), "{err:?}");
        assert_eq!(err.to_string(), "no such resource: @99");
    }
}
