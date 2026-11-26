//! Durable storage and validation of an execution binding.
use phux_protocol::api::ExecutionRef;
use phux_protocol::ids::{AuthorityId, ResourceKey, SessionKey};
use rusqlite::{Transaction, params};

use super::{Alias, Error, ObjectiveState, OpaqueId, OpenRun, RunState, StartBinding};

/// Keeper identity. Neither public JSON defaults nor additions define stored data.
#[derive(Debug)]
pub(super) struct ExecutionPin {
    authority: AuthorityId,
    resource: ResourceKey,
    activation: std::num::NonZeroU64,
    instance: [u8; 16],
}

/// Released keeper JSON layout; field order is also the binding unique-index key.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredExecution {
    authority: String,
    resource: String,
    activation: u64,
    instance: String,
}

impl ExecutionPin {
    fn parse(
        authority: &str,
        resource: &str,
        activation: u64,
        instance: &str,
    ) -> Result<Self, Error> {
        let invalid = |reason| Error::invalid_text("execution", reason);
        let mut bytes = [0; 16];
        if instance.len() != 32 || !instance.is_ascii() {
            return Err(invalid("instance must be nonzero 32-hex"));
        }
        for (byte, pair) in bytes.iter_mut().zip(instance.as_bytes().as_chunks::<2>().0) {
            let digit = |c: u8| (c as char).to_digit(16);
            *byte = match (digit(pair[0]), digit(pair[1])) {
                (Some(hi), Some(lo)) => u8::try_from((hi << 4) | lo)
                    .map_err(|_| invalid("instance must be nonzero 32-hex"))?,
                _ => return Err(invalid("instance must be nonzero 32-hex")),
            };
        }
        if bytes == [0; 16] {
            return Err(invalid("instance must be nonzero 32-hex"));
        }
        Ok(Self {
            authority: AuthorityId::parse(authority)
                .ok_or_else(|| invalid("invalid authority key"))?,
            resource: ResourceKey::parse(resource)
                .ok_or_else(|| invalid("invalid resource key"))?,
            activation: std::num::NonZeroU64::new(activation)
                .ok_or_else(|| invalid("activation must be nonzero"))?,
            instance: bytes,
        })
    }

    fn stored(&self) -> StoredExecution {
        StoredExecution {
            authority: self.authority.to_string(),
            resource: self.resource.to_string(),
            activation: self.activation.get(),
            instance: phux_protocol::ids::hex16(&self.instance),
        }
    }

    fn public(self) -> ExecutionRef {
        let stored = self.stored();
        ExecutionRef {
            authority: stored.authority,
            resource: stored.resource,
            activation: stored.activation,
            instance: stored.instance,
        }
    }
}

impl serde::Serialize for ExecutionPin {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.stored(), serializer)
    }
}

impl<'de> serde::Deserialize<'de> for ExecutionPin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = <StoredExecution as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(
            &stored.authority,
            &stored.resource,
            stored.activation,
            &stored.instance,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Frozen receipt binding; deliberately not the public request/response DTO.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct StoredBinding {
    session_key: String,
    session: String,
    terminal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution: Option<ExecutionPin>,
    pub(super) worktree_path: String,
    pub(super) branch: String,
    tab_title: String,
}

impl StoredBinding {
    pub(super) fn from_public(binding: StartBinding) -> Result<Self, Error> {
        let execution = binding
            .execution
            .map(|pin| {
                ExecutionPin::parse(&pin.authority, &pin.resource, pin.activation, &pin.instance)
            })
            .transpose()?;
        Ok(Self {
            session_key: binding.session_key,
            session: binding.session,
            terminal_id: binding.terminal_id,
            execution,
            worktree_path: binding.worktree_path,
            branch: binding.branch,
            tab_title: binding.tab_title,
        })
    }

    pub(super) fn public(self) -> StartBinding {
        StartBinding {
            session_key: self.session_key,
            session: self.session,
            terminal_id: self.terminal_id,
            execution: self.execution.map(ExecutionPin::public),
            worktree_path: self.worktree_path,
            branch: self.branch,
            tab_title: self.tab_title,
        }
    }
}

pub(super) fn map_open_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpenRun> {
    let alias: String = row.get(0)?;
    let objective_id: Vec<u8> = row.get(1)?;
    let revision: i64 = row.get(2)?;
    let state: String = row.get(3)?;
    let run_id: Vec<u8> = row.get(4)?;
    let run_state: String = row.get(5)?;
    let ordinal: i64 = row.get(6)?;
    let max_agents: i64 = row.get(9)?;
    let execution: Option<String> = row.get(15)?;
    let authority: Option<String> = row.get(16)?;
    let credential_lost: i64 = row.get(17)?;
    let stored = |err: Error| rusqlite::Error::ToSqlConversionFailure(Box::new(err));
    let execution = execution
        .as_deref()
        .map(serde_json::from_str::<ExecutionPin>)
        .transpose()
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?
        .map(ExecutionPin::public);
    let authority = authority
        .map(|text| {
            AuthorityId::parse(&text).ok_or_else(|| Error::Corrupt("launch authority".to_owned()))
        })
        .transpose()
        .map_err(stored)?;
    Ok(OpenRun {
        alias: Alias::parse(&alias).map_err(stored)?,
        objective_id: OpaqueId::from_slice(&objective_id).map_err(stored)?,
        revision: u64::try_from(revision)
            .map_err(|_| stored(Error::Corrupt("revision".to_owned())))?,
        state: ObjectiveState::parse(&state).map_err(stored)?,
        run_id: OpaqueId::from_slice(&run_id).map_err(stored)?,
        run_state: RunState::parse(&run_state).map_err(stored)?,
        ordinal: u64::try_from(ordinal)
            .map_err(|_| stored(Error::Corrupt("ordinal".to_owned())))?,
        agent_kind: row.get(7)?,
        model: row.get(8)?,
        max_agents: u32::try_from(max_agents).unwrap_or(0),
        repo_root: row.get(10)?,
        authority,
        worktree_path: row.get(11)?,
        branch: row.get(12)?,
        terminal_id: row.get(13)?,
        session_key: row.get(14)?,
        execution,
        credential_lost: credential_lost != 0,
    })
}

pub(super) fn insert_binding(
    tx: &Transaction<'_>,
    run_id: OpaqueId,
    binding: &StoredBinding,
) -> Result<(), Error> {
    let pin = binding.execution.as_ref().ok_or_else(|| {
        Error::invalid_text(
            "execution",
            "new worker bindings require an exact execution pin",
        )
    })?;
    if SessionKey::parse(&binding.session_key).is_none() {
        return Err(Error::invalid_text(
            "session_key",
            "invalid durable session key",
        ));
    }
    if binding
        .terminal_id
        .as_ref()
        .is_some_and(|id| !id.parse::<u32>().is_ok_and(|id| id > 0))
    {
        return Err(Error::invalid_text(
            "terminal_id",
            "invalid presentation hint",
        ));
    }
    let execution = serde_json::to_string(pin)?;
    let id = OpaqueId::generate()?;
    tx.execute(
        "INSERT INTO binding (
            id, run_id, server_id, session_key, terminal_id, worktree_path, branch, execution
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7)",
        params![
            id.as_slice(),
            run_id.as_slice(),
            binding.session_key,
            binding.terminal_id,
            binding.worktree_path,
            binding.branch,
            execution,
        ],
    )?;
    Ok(())
}

pub(super) fn validate_binding_authority(
    binding: &StoredBinding,
    authority: Option<AuthorityId>,
) -> Result<(), Error> {
    let expected = authority.ok_or_else(|| {
        Error::invalid_text("authority", "worker launch intent does not name an owner")
    })?;
    let actual = binding.execution.as_ref().map(|pin| pin.authority);
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(Error::invalid_text(
            "execution",
            "worker receipt does not match the launch authority",
        ))
    }
}
