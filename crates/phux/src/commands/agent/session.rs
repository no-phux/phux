//! Durable bridge for provider-native agent resume identity.
//!
//! `phux.agent-session/v1` is live, terminal-scoped L3 metadata. It is not the
//! durable store: terminal metadata dies with the Terminal. `workspace save`
//! copies a confirmed record into the versioned archive, and restore stamps the
//! returned replacement Terminal after replaying the provider's native argv.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use phux_client::attach::connection::{Answer, Connection};
use phux_plugin::ResolvedLaunch;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, MAX_AGENT_SESSION_RECORD_BYTES, Scope};
use phux_protocol::wire::info::SessionSnapshot;
use serde::{Deserialize, Serialize};

pub(crate) use phux_protocol::wire::frame::TERMINAL_AGENT_SESSION_KEY;
const MAX_PROVENANCE_ID_BYTES: usize = 120;

/// Inert provenance persisted for one exact provider-native agent session.
///
/// Executable paths and arguments are deliberately absent. Restore re-resolves
/// the current enabled integration and uses its current structured argv policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the versioned L3 schema names each provenance field explicitly"
)]
pub(crate) struct AgentSessionRecord {
    pub(crate) plugin_id: String,
    pub(crate) integration_id: String,
    pub(crate) native_id: String,
}

/// A launch carrying one established or resumed provider-native session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedAgentSession {
    pub(crate) record: AgentSessionRecord,
    pub(crate) argv: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) cwd: PathBuf,
}

impl AgentSessionRecord {
    pub(crate) fn new(
        plugin_id: &str,
        integration_id: &str,
        native_id: &str,
    ) -> Result<Self, String> {
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

    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self)
            .map_err(|err| format!("could not encode agent session record: {err}"))
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_AGENT_SESSION_RECORD_BYTES {
            return Err(format!(
                "invalid {TERMINAL_AGENT_SESSION_KEY}: encoded record must contain 1..={MAX_AGENT_SESSION_RECORD_BYTES} bytes"
            ));
        }
        let raw: Self = serde_json::from_slice(bytes)
            .map_err(|err| format!("invalid {TERMINAL_AGENT_SESSION_KEY} JSON: {err}"))?;
        Self::new(&raw.plugin_id, &raw.integration_id, &raw.native_id)
    }
}

/// Prepare a native session for a new `phux launch`.
///
/// A caller-supplied value in the template's `native_env` means “resume this
/// exact existing provider session.” Without one, providers that document a
/// caller-supplied fresh identity receive a generated UUID through their
/// structured `fresh_args`. Providers without such an API remain ordinary
/// non-restorable fresh launches; callers may still resume a known identity by
/// supplying `native_env`.
pub(crate) fn prepare_for_launch(
    resolved: &ResolvedLaunch,
) -> Result<Option<PreparedAgentSession>, String> {
    let Some(policy) = resolved
        .session_identity
        .as_ref()
        .filter(|policy| policy.supports_native_restore())
    else {
        return Ok(None);
    };
    if let Some(value) = std::env::var_os(&policy.native_env) {
        let native_id = value.into_string().map_err(|_| {
            format!(
                "integration {:?} session identity in {} is not UTF-8",
                resolved.integration_id, policy.native_env
            )
        })?;
        return prepare(resolved, &native_id).map(Some);
    }
    if !policy.supports_native_fresh() {
        return Ok(None);
    }
    let native_id = uuid::Uuid::new_v4().to_string();
    let argv = resolved
        .fresh_argv(&native_id)
        .map_err(|err| format!("integration {:?}: {err}", resolved.integration_id))?;
    prepare_with_argv(resolved, &native_id, argv).map(Some)
}

/// Construct a provider-native resume launch from trusted current template
/// data plus inert archived identity.
pub(crate) fn prepare(
    resolved: &ResolvedLaunch,
    native_id: &str,
) -> Result<PreparedAgentSession, String> {
    let argv = resolved
        .resume_argv(native_id)
        .map_err(|err| format!("integration {:?}: {err}", resolved.integration_id))?;
    prepare_with_argv(resolved, native_id, argv)
}

fn prepare_with_argv(
    resolved: &ResolvedLaunch,
    native_id: &str,
    argv: Vec<String>,
) -> Result<PreparedAgentSession, String> {
    let policy = resolved.session_identity.as_ref().ok_or_else(|| {
        format!(
            "integration {:?} no longer declares session identity",
            resolved.integration_id
        )
    })?;
    let record = AgentSessionRecord::new(&resolved.plugin_id, &resolved.integration_id, native_id)?;
    let mut env = BTreeMap::new();
    env.insert(policy.native_env.clone(), native_id.to_owned());
    Ok(PreparedAgentSession {
        record,
        argv,
        env,
        cwd: resolved.cwd.clone(),
    })
}

/// GET-confirm an atomically installed record, falling back to SET for an older
/// server that ignored the additive spawn field.
pub(crate) async fn persist_record(
    conn: &mut Connection,
    terminal: &TerminalId,
    record: &AgentSessionRecord,
    request_id: u32,
) -> Result<(), String> {
    if !matches!(terminal, TerminalId::Local { .. }) {
        return Err("agent session records are local-terminal only".to_owned());
    }
    let value = record.encode()?;
    let (existing, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Terminal(terminal.clone()),
            TERMINAL_AGENT_SESSION_KEY.to_owned(),
        )
        .await
        .map_err(|err| err.to_string())?
        .into_parts();
    for message in phux_client::state::degradation_notices(&interleaved) {
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
        scope: Scope::Terminal(terminal.clone()),
        key: TERMINAL_AGENT_SESSION_KEY.to_owned(),
        value: value.clone(),
    })
    .await
    .map_err(|err| err.to_string())?;
    let (answer, interleaved) = conn
        .request_metadata(
            request_id.wrapping_add(2),
            Scope::Terminal(terminal.clone()),
            TERMINAL_AGENT_SESSION_KEY.to_owned(),
        )
        .await
        .map_err(|err| err.to_string())?
        .into_parts();
    for message in phux_client::state::degradation_notices(&interleaved) {
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
/// dropping one record would make a later restore start a blank shell instead
/// of the saved conversation.
pub(crate) async fn fetch_record_index(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
) -> Result<HashMap<TerminalId, AgentSessionRecord>, String> {
    let mut index = HashMap::new();
    let local = snapshot
        .panes
        .iter()
        .filter(|pane| matches!(pane.id, TerminalId::Local { .. }));
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| err.to_string())?;
    for (offset, pane) in local.enumerate() {
        let request_id = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
        let (answer, interleaved) = conn
            .request_metadata(
                request_id,
                Scope::Terminal(pane.id.clone()),
                TERMINAL_AGENT_SESSION_KEY.to_owned(),
            )
            .await
            .map_err(|err| err.to_string())?
            .into_parts();
        for message in phux_client::state::degradation_notices(&interleaved) {
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
    Ok(index)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
