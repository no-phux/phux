//! `phux.agent-session/v1` provenance and the spawn-with-provenance wire work.
//!
//! The record is inert, terminal-scoped provenance (`plugin_id`,
//! `integration_id`, `native_id`) that `workspace save` archives and
//! `workspace restore` replays by re-resolving the integration's argv.
//!
//! Not `phux.agent/v1` ([`crate::agent_record`]) and not the `AgentSession`
//! resource kind ([`crate::agent_session`]).

use std::collections::HashMap;
use std::path::Path;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, MAX_AGENT_SESSION_RECORD_BYTES, RESOURCE_AGENT_SESSION_KEY,
    Scope, SpawnError, SpawnResult,
};
use phux_protocol::wire::info::SessionSnapshot;
use serde::{Deserialize, Serialize};

use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};

const MAX_PROVENANCE_ID_BYTES: usize = 120;

/// Inert provenance for one provider-native agent session; executable paths
/// and arguments are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the versioned L3 schema names each provenance field explicitly"
)]
pub struct AgentSessionRecord {
    /// The plugin that declared the launched integration.
    pub plugin_id: String,
    /// The integration template that was launched.
    pub integration_id: String,
    /// The provider's own native session identifier.
    pub native_id: String,
}

impl AgentSessionRecord {
    /// Build a record, rejecting an untrusted or oversized identity.
    ///
    /// # Errors
    ///
    /// A field that is empty, oversized, or carries control characters / a
    /// leading `-` (rejected as a would-be flag by
    /// [`phux_config::integration::validate_native_session_id`]).
    pub fn new(plugin_id: &str, integration_id: &str, native_id: &str) -> Result<Self, String> {
        validate_provenance("plugin_id", plugin_id)?;
        validate_provenance("integration_id", integration_id)?;
        phux_config::integration::validate_native_session_id(native_id)
            .map_err(|err| err.to_string())?;
        Ok(Self {
            plugin_id: plugin_id.to_owned(),
            integration_id: integration_id.to_owned(),
            native_id: native_id.to_owned(),
        })
    }

    /// Encode as the `phux.agent-session/v1` wire value.
    ///
    /// # Errors
    ///
    /// Not expected for this shape; surfaced rather than unwrapped.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self)
            .map_err(|err| format!("could not encode agent session record: {err}"))
    }

    /// Parse and re-validate a `phux.agent-session/v1` value, never trusting
    /// the wire bytes as already-checked.
    ///
    /// # Errors
    ///
    /// Empty or oversized bytes, malformed JSON, an unknown field, or a
    /// field that fails [`Self::new`]'s validation.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_AGENT_SESSION_RECORD_BYTES {
            return Err(format!(
                "invalid {RESOURCE_AGENT_SESSION_KEY}: encoded record must contain 1..={MAX_AGENT_SESSION_RECORD_BYTES} bytes"
            ));
        }
        let raw: Self = serde_json::from_slice(bytes)
            .map_err(|err| format!("invalid {RESOURCE_AGENT_SESSION_KEY} JSON: {err}"))?;
        Self::new(&raw.plugin_id, &raw.integration_id, &raw.native_id)
    }
}

fn validate_provenance(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_PROVENANCE_ID_BYTES {
        return Err(format!(
            "agent session {field} must contain 1..={MAX_PROVENANCE_ID_BYTES} UTF-8 bytes"
        ));
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(format!(
            "agent session {field} must be trimmed and control-free"
        ));
    }
    Ok(())
}

/// Print interleaved degradation notices inline, in encounter order.
#[allow(
    clippy::print_stderr,
    reason = "callers (`phux spawn`/`launch`, `workspace restore`) rely on these \
              notices printing inline, in order with the spawn's own"
)]
fn warn_partial<'a>(notices: impl IntoIterator<Item = &'a String>) {
    for message in notices {
        eprintln!("phux: warning: partial results — {message}");
    }
}

/// One `GET_METADATA` of `terminal`'s agent-session record.
async fn read_stored(
    conn: &mut Connection,
    terminal: &ResourceId,
    request_id: u32,
) -> Result<Answer<Option<Vec<u8>>>, String> {
    let (answer, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Resource(terminal.clone()),
            RESOURCE_AGENT_SESSION_KEY.to_owned(),
        )
        .await
        .map_err(|err| err.to_string())?
        .into_parts();
    warn_partial(&crate::state::degradation_notices(&interleaved));
    Ok(answer)
}

/// GET-confirm an atomically installed record, falling back to SET for an
/// older server that ignored the additive spawn field.
///
/// # Errors
///
/// A malformed encode, a transport failure, a refused write, or a
/// server-returned record that does not match `record`.
pub async fn persist_record(
    conn: &mut Connection,
    terminal: &ResourceId,
    record: &AgentSessionRecord,
    request_id: u32,
) -> Result<(), String> {
    if !matches!(terminal, ResourceId::Local { .. }) {
        return Err("agent session records are local-terminal only".to_owned());
    }
    let value = record.encode()?;
    let confirm = |answer: Answer<Option<Vec<u8>>>| match answer {
        Answer::Ok(Some(stored)) if stored == value => Ok(true),
        Answer::Ok(Some(_)) => Err("server returned a different agent session record".to_owned()),
        Answer::Ok(None) => Ok(false),
        Answer::Err(refusal) => Err(format!("agent session record was refused: {refusal}")),
    };
    if confirm(read_stored(conn, terminal, request_id).await?)? {
        return Ok(());
    }
    conn.send(&FrameKind::SetMetadata {
        request_id: request_id.wrapping_add(1),
        scope: Scope::Resource(terminal.clone()),
        key: RESOURCE_AGENT_SESSION_KEY.to_owned(),
        value: value.clone(),
    })
    .await
    .map_err(|err| err.to_string())?;
    if confirm(read_stored(conn, terminal, request_id.wrapping_add(2)).await?)? {
        Ok(())
    } else {
        Err("agent session record did not persist".to_owned())
    }
}

/// Fetch every valid live resume record by its exact local Terminal id.
/// Not best effort: a dropped record would restore a blank shell instead of
/// the saved conversation.
///
/// # Errors
///
/// A connect or transport failure, a refused read, or a stored record that
/// fails [`AgentSessionRecord::parse`].
pub async fn fetch_record_index(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
) -> Result<HashMap<ResourceId, AgentSessionRecord>, String> {
    let mut index = HashMap::new();
    let local = snapshot
        .resources
        .iter()
        .filter(|pane| matches!(pane.id, ResourceId::Local { .. }));
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| err.to_string())?;
    for (offset, pane) in local.enumerate() {
        let request_id = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
        match read_stored(&mut conn, &pane.id, request_id).await? {
            Answer::Ok(Some(bytes)) => {
                index.insert(pane.id.clone(), AgentSessionRecord::parse(&bytes)?);
            }
            Answer::Ok(None) => {}
            Answer::Err(refusal) => {
                return Err(format!(
                    "could not read agent session record for {}: {refusal}",
                    pane.id
                ));
            }
        }
    }
    drop(conn);
    Ok(index)
}

/// Kill `terminal` after a failed provenance write and describe the outcome.
async fn roll_back(
    conn: &mut Connection,
    terminal: &ResourceId,
    request_id: u32,
    removed: &str,
) -> String {
    let command = Command::KillResource {
        terminal_id: terminal.clone(),
        operation_id: None,
    };
    match conn.request(request_id, command).await {
        Ok(reply) => match reply.into_parts().0 {
            CommandResult::Ok => removed.to_owned(),
            other => format!("cleanup returned {other:?}"),
        },
        Err(cleanup_err) => format!("cleanup failed: {cleanup_err}"),
    }
}

/// [`crate::spawn::spawn`] plus the optional agent-session provenance write.
///
/// Shared by `phux spawn` and `phux launch`. A failed write kills the new
/// Terminal and turns the result into `SpawnFailed`, so a caller never gets
/// a pane it does not know about. The spawn's degradation notices print
/// first, keeping them in order with [`persist_record`]'s inline notices.
///
/// # Errors
///
/// Transport and decode failures from [`crate::spawn::spawn`].
pub async fn spawn_with_agent_session(
    conn: &mut Connection,
    frame: &FrameKind,
    agent_session: Option<&AgentSessionRecord>,
) -> Result<SpawnResult, AttachError> {
    let (mut result, degradation) = crate::spawn::spawn(conn, frame).await?;
    warn_partial(degradation.notices());
    if let (Some(record), SpawnResult::Ok(terminal)) = (agent_session, &result) {
        let request_id = match frame {
            FrameKind::SpawnResource { request_id, .. } => *request_id,
            _ => 1,
        };
        if let Err(err) = persist_record(conn, terminal, record, request_id.wrapping_add(1)).await {
            let cleanup_note = roll_back(
                conn,
                terminal,
                request_id.wrapping_add(4),
                "spawned terminal removed",
            )
            .await;
            result = SpawnResult::Err(SpawnError::SpawnFailed(format!(
                "agent session record could not be confirmed: {err}; {cleanup_note}"
            )));
        }
    }
    Ok(result)
}

/// [`spawn_with_agent_session`] over a fresh connection.
///
/// # Errors
///
/// Transport failures from [`Connection::connect`] or
/// [`spawn_with_agent_session`].
pub async fn spawn_with_agent_session_on(
    socket_path: &Path,
    frame: &FrameKind,
    agent_session: Option<&AgentSessionRecord>,
) -> Result<SpawnResult, AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    let outcome = spawn_with_agent_session(&mut conn, frame, agent_session).await?;
    drop(conn);
    Ok(outcome)
}

/// [`persist_record`] for an already-spawned pane (`phux workspace
/// restore`), over a fresh connection, with the same rollback on failure.
///
/// # Errors
///
/// A connect failure, or [`persist_record`]'s failure with the rollback's
/// outcome folded in.
pub async fn confirm_agent_session_record_on(
    socket_path: &Path,
    terminal: &ResourceId,
    record: &AgentSessionRecord,
    request_id: u32,
) -> Result<(), String> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| format!("could not confirm restored agent session: {err}"))?;
    let Err(err) = persist_record(&mut conn, terminal, record, request_id).await else {
        return Ok(());
    };
    let cleanup_note = roll_back(
        &mut conn,
        terminal,
        request_id.wrapping_add(3),
        "restored terminal removed",
    )
    .await;
    drop(conn);
    Err(format!(
        "restored agent session record could not be confirmed: {err}; {cleanup_note}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{ScriptSpec, serve_one};
    use phux_protocol::ids::GroupId;

    #[test]
    fn record_round_trips_and_rejects_untrusted_shape() {
        let record =
            AgentSessionRecord::new("com.phux.agents", "codex", "thread-42").expect("valid record");
        assert_eq!(
            AgentSessionRecord::parse(&record.encode().expect("encode")).expect("parse"),
            record
        );
        assert!(
            AgentSessionRecord::parse(
                br#"{"plugin_id":"p","integration_id":"i","native_id":"n","argv":["sh"]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn record_rejects_oversized_or_control_bearing_identity() {
        assert!(AgentSessionRecord::new("p", "i", " line").is_err());
        assert!(AgentSessionRecord::new("p", "i", &"x".repeat(1_025)).is_err());
        assert!(AgentSessionRecord::new("p\n", "i", "native").is_err());
        assert!(AgentSessionRecord::new("p", "i", "--dangerous").is_err());
    }

    fn spawn_frame() -> FrameKind {
        FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(vec!["agent".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        }
    }

    /// Run [`spawn_with_agent_session`] against `spec`; the result and the
    /// frames the server saw.
    async fn spawn_against(spec: ScriptSpec) -> (SpawnResult, Vec<FrameKind>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let spec = spec.spawn_result(SpawnResult::Ok(ResourceId::local(9)));
        let (socket, server) = serve_one(dir.path(), spec);

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let record = AgentSessionRecord::new("com.phux.agents", "codex", "thread-1").unwrap();
        let result = spawn_with_agent_session(&mut conn, &spawn_frame(), Some(&record))
            .await
            .expect("scripted server answers");
        drop(conn);
        (result, server.await.expect("scripted server task"))
    }

    #[tokio::test]
    async fn spawn_with_agent_session_persists_and_confirms_the_record() {
        let (result, seen) = spawn_against(ScriptSpec::new()).await;
        assert_eq!(result, SpawnResult::Ok(ResourceId::local(9)));
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { scope: Scope::Resource(id), key, .. }
                    if *id == ResourceId::local(9) && key == RESOURCE_AGENT_SESSION_KEY
            )),
            "expected the SET of the agent session record; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn spawn_with_agent_session_rolls_back_on_a_refused_write() {
        use phux_protocol::wire::frame::ErrorCode;

        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "no");
        let (result, seen) = spawn_against(spec).await;
        assert!(matches!(
            result,
            SpawnResult::Err(SpawnError::SpawnFailed(_))
        ));
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResource { terminal_id, .. },
                    ..
                } if *terminal_id == ResourceId::local(9)
            )),
            "expected the KILL_RESOURCE rollback; sent {seen:?}"
        );
    }
}
