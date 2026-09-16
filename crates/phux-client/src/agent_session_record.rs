//! Provider-native agent-session provenance and the wire work behind `phux
//! spawn` / `phux launch`'s optional native-session restore.
//!
//! `phux.agent-session/v1` is live, terminal-scoped L3 metadata: inert
//! provenance (`plugin_id`, `integration_id`, `native_id`) that
//! `workspace save` copies into the durable archive, and `workspace
//! restore` replays by re-resolving the current integration's argv.
//! Executable paths and arguments are deliberately absent from the record
//! itself.
//!
//! Distinct from two similarly named things: `phux.agent/v1`
//! ([`crate::agent_record`], ADR-0040) is a human/wrapper-declared identity
//! plus lifecycle. The `AgentSession` *resource* kind
//! ([`crate::agent_session`], ADR-0103) is a server-tracked resource with
//! its own open/close/emit/log verbs. This module is metadata on an
//! ordinary Terminal.

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

/// Inert provenance persisted for one exact provider-native agent session.
///
/// Executable paths and arguments are deliberately absent. Restore
/// re-resolves the current enabled integration and uses its current
/// structured argv policy.
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
    /// JSON encoding is not expected to fail for this shape; the `Result`
    /// exists so a caller composing this into a larger fallible pipeline
    /// need not `unwrap`.
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

/// GET-confirm an atomically installed record, falling back to SET for an
/// older server that ignored the additive spawn field.
///
/// # Errors
///
/// A malformed encode, a transport failure, a refused write, or a
/// server-returned record that does not match `record`.
#[allow(
    clippy::print_stderr,
    reason = "moved verbatim from crates/phux/src/commands/agent/session.rs (pre-L21b): \
              the direct eprintln keeps this function's signature and behavior byte-identical \
              for its other caller, workspace/archive.rs (owned by a concurrent lane), rather \
              than threading a Degradation return through a call site this pass does not own"
)]
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
    let (existing, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Resource(terminal.clone()),
            RESOURCE_AGENT_SESSION_KEY.to_owned(),
        )
        .await
        .map_err(|err| err.to_string())?
        .into_parts();
    for message in crate::state::degradation_notices(&interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
    match existing {
        Answer::Ok(Some(stored)) if stored == value => return Ok(()),
        Answer::Ok(Some(_)) => {
            return Err("server returned a different agent session record".to_owned());
        }
        Answer::Err(refusal) => {
            return Err(format!("agent session record was refused: {refusal}"));
        }
        Answer::Ok(None) => {}
    }

    conn.send(&FrameKind::SetMetadata {
        request_id: request_id.wrapping_add(1),
        scope: Scope::Resource(terminal.clone()),
        key: RESOURCE_AGENT_SESSION_KEY.to_owned(),
        value: value.clone(),
    })
    .await
    .map_err(|err| err.to_string())?;
    let (answer, interleaved) = conn
        .request_metadata(
            request_id.wrapping_add(2),
            Scope::Resource(terminal.clone()),
            RESOURCE_AGENT_SESSION_KEY.to_owned(),
        )
        .await
        .map_err(|err| err.to_string())?
        .into_parts();
    for message in crate::state::degradation_notices(&interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
    match answer {
        Answer::Ok(Some(stored)) if stored == value => Ok(()),
        Answer::Ok(Some(_)) => Err("server returned a different agent session record".to_owned()),
        Answer::Ok(None) => Err("agent session record did not persist".to_owned()),
        Answer::Err(refusal) => Err(format!("agent session record was refused: {refusal}")),
    }
}

/// Fetch every valid live resume record by its exact local Terminal id.
///
/// Unlike display-only agent metadata, this is not best effort: silently
/// dropping one record would make a later restore start a blank shell
/// instead of the saved conversation.
///
/// # Errors
///
/// A connect failure, a transport failure reading any pane's record, or a
/// stored record that fails [`AgentSessionRecord::parse`].
#[allow(
    clippy::print_stderr,
    reason = "moved verbatim from crates/phux/src/commands/agent/session.rs (pre-L21b); see \
              persist_record's reason"
)]
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
        let (answer, interleaved) = conn
            .request_metadata(
                request_id,
                Scope::Resource(pane.id.clone()),
                RESOURCE_AGENT_SESSION_KEY.to_owned(),
            )
            .await
            .map_err(|err| err.to_string())?
            .into_parts();
        for message in crate::state::degradation_notices(&interleaved) {
            eprintln!("phux: warning: partial results — {message}");
        }
        match answer {
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

/// [`crate::spawn::spawn`] plus the optional agent-session provenance write
/// (and its same-connection `KILL_RESOURCE` rollback on failure) shared by
/// `phux spawn` and `phux launch`.
///
/// On a successful spawn with a requested `agent_session`, the record is
/// persisted via [`persist_record`]; if that fails, the spawned Terminal is
/// killed and the overall result becomes
/// `SpawnResult::Err(SpawnError::SpawnFailed(..))` naming both the
/// persistence failure and how the rollback went — never a spawned pane the
/// caller does not know about.
///
/// Prints the spawn's own interleaved degradation notices itself (the
/// historical `dispatch_spawn_async` ordering: the spawn's notices, then
/// [`persist_record`]'s own inline notices from its GET/SET/GET) rather
/// than returning a [`crate::state::Degradation`] for the caller to print
/// afterward — [`persist_record`] already prints inline for the same
/// reason (see its doc comment), and printing here first is what keeps the
/// two interleaved prints in encounter order.
///
/// # Errors
///
/// Transport and decode failures from [`crate::spawn::spawn`].
#[allow(
    clippy::print_stderr,
    reason = "prints the spawn's own degradation notices before calling \
              persist_record (L21b review): persist_record already prints \
              inline for its own GET/SET/GET, so printing this function's \
              notices here, before that call, is what preserves the \
              historical dispatch_spawn_async ordering (spawn notices, then \
              persist_record's) instead of the two interleaving out of order"
)]
pub async fn spawn_with_agent_session(
    conn: &mut Connection,
    frame: &FrameKind,
    agent_session: Option<&AgentSessionRecord>,
) -> Result<SpawnResult, AttachError> {
    let (mut result, degradation) = crate::spawn::spawn(conn, frame).await?;
    for message in degradation.notices() {
        eprintln!("phux: warning: partial results — {message}");
    }
    if let (Some(record), SpawnResult::Ok(terminal)) = (agent_session, &result) {
        let request_id = match frame {
            FrameKind::SpawnResource { request_id, .. } => *request_id,
            _ => 1,
        };
        if let Err(err) = persist_record(conn, terminal, record, request_id.wrapping_add(1)).await {
            let cleanup = conn
                .request(
                    request_id.wrapping_add(4),
                    Command::KillResource {
                        terminal_id: terminal.clone(),
                        operation_id: None,
                    },
                )
                .await;
            let cleanup_note = match cleanup {
                Ok(reply) => match reply.into_parts().0 {
                    CommandResult::Ok => "spawned terminal removed".to_owned(),
                    other => format!("cleanup returned {other:?}"),
                },
                Err(cleanup_err) => format!("cleanup failed: {cleanup_err}"),
            };
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

/// [`persist_record`] over a fresh connection, with the rollback dance from
/// [`spawn_with_agent_session`].
///
/// Used by `phux workspace restore` to confirm a resumed native agent
/// session's provenance write for a terminal whose spawn has already
/// completed (unlike [`spawn_with_agent_session`], this is a follow-up write
/// on an already-existing pane, not part of the spawn itself).
///
/// # Errors
///
/// A connect failure, or [`persist_record`]'s own failure, with the
/// rollback kill's own outcome folded into the message.
pub async fn confirm_agent_session_record_on(
    socket_path: &Path,
    terminal: &ResourceId,
    record: &AgentSessionRecord,
    request_id: u32,
) -> Result<(), String> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| format!("could not confirm restored agent session: {err}"))?;
    if let Err(err) = persist_record(&mut conn, terminal, record, request_id).await {
        let cleanup = conn
            .request(
                request_id.wrapping_add(3),
                Command::KillResource {
                    terminal_id: terminal.clone(),
                    operation_id: None,
                },
            )
            .await;
        drop(conn);
        let cleanup_note = match cleanup {
            Ok(reply) => match reply.into_parts().0 {
                CommandResult::Ok => "restored terminal removed".to_owned(),
                other => format!("cleanup returned {other:?}"),
            },
            Err(cleanup_err) => format!("cleanup failed: {cleanup_err}"),
        };
        return Err(format!(
            "restored agent session record could not be confirmed: {err}; {cleanup_note}"
        ));
    }
    drop(conn);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn spawn_with_agent_session_persists_and_confirms_the_record() {
        use crate::testkit::{ScriptSpec, ScriptedServer};

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let spec = ScriptSpec::new().spawn_result(SpawnResult::Ok(ResourceId::local(9)));
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let record = AgentSessionRecord::new("com.phux.agents", "codex", "thread-1").unwrap();
        let result = spawn_with_agent_session(&mut conn, &spawn_frame(), Some(&record))
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

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
        use crate::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::wire::frame::ErrorCode;

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let spec = ScriptSpec::new()
            .spawn_result(SpawnResult::Ok(ResourceId::local(9)))
            .refuse_metadata(ErrorCode::PermissionDenied, "no");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let record = AgentSessionRecord::new("com.phux.agents", "codex", "thread-1").unwrap();
        let result = spawn_with_agent_session(&mut conn, &spawn_frame(), Some(&record))
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

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
