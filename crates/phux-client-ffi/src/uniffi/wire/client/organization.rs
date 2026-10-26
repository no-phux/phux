//! The organization surface of [`RemoteClient`]: Intake and the typed
//! organization commands.
//!
//! Keys cross as full base32 text and operation ids as 32 hex digits. Each
//! command returns its operation id; the outcome arrives later as
//! [`WireEvent::OrganizationOperation`] (and [`WireEvent::KillFinished`] for
//! `kill_session`). A submission that cannot be made throws
//! [`WireError::Runtime`] with a stable reason word: `not_connected`,
//! `invalid_session_key`, `invalid_resource_key`, `invalid_operation_id`,
//! `invalid_session_name`, `unknown_record` (the replica does not hold a
//! named record), `duplicate_operation`, `operation_journal_full`,
//! `unknown_receipt` (no retained receipt for `undo`), or
//! `organization_unavailable`.

use std::collections::VecDeque;

use phux_client_core::org_ops::{OrgOutcome, SubmitError};
use phux_client_core::rename::check_session_name;
use phux_client_runtime::control::mint_operation;
use phux_protocol::ids::{OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Command, Committed, LaunchSpec, SessionLifetime, TerminateMode};

use super::*;
use crate::projection::organization;

pub(super) mod arrangement;

/// Most committed receipts a client keeps for `undo`; the oldest is dropped
/// first.
const MAX_RECEIPTS: usize = 256;

/// Whether a session outlives its last resource.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireSessionLifetime {
    /// Survives empty.
    Persistent,
    /// Deleted with its last resource; never empty.
    Ephemeral,
}

impl From<WireSessionLifetime> for SessionLifetime {
    fn from(value: WireSessionLifetime) -> Self {
        match value {
            WireSessionLifetime::Persistent => Self::Persistent,
            WireSessionLifetime::Ephemeral => Self::Ephemeral,
        }
    }
}

fn reason(word: &str) -> WireError {
    WireError::Runtime {
        reason: word.to_owned(),
    }
}

fn session_key(text: &str) -> Result<SessionKey, WireError> {
    SessionKey::parse(text).ok_or_else(|| reason("invalid_session_key"))
}

fn resource_key(text: &str) -> Result<ResourceKey, WireError> {
    ResourceKey::parse(text).ok_or_else(|| reason("invalid_resource_key"))
}

/// A name to create or assign: one `check_session_name` accepts.
fn session_name(name: &str) -> Result<(), WireError> {
    check_session_name(name).map_err(|_| reason("invalid_session_name"))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the error"
)]
fn submit_error(error: SubmitError) -> WireError {
    reason(match error {
        SubmitError::NotAnOperation => "unknown_record",
        SubmitError::DuplicateOperation => "duplicate_operation",
        SubmitError::Full => "operation_journal_full",
        SubmitError::Unsupported => "organization_unavailable",
    })
}

/// An operation id's text form: 32 lowercase hex digits.
pub(super) fn operation_text(operation: &OperationId) -> String {
    topology::group_id_text(operation.as_bytes())
}

/// Parse [`operation_text`] back.
fn parse_operation(text: &str) -> Option<OperationId> {
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    OperationId::new(bytes)
}

fn submitted(result: Result<OperationId, SubmitError>) -> Result<String, WireError> {
    result
        .map(|operation| operation_text(&operation))
        .map_err(submit_error)
}

/// The `SESSION_CREATE` `create_session` sends. A `cwd` launches the first
/// shell there; an ephemeral session always launches one (it is never
/// empty), in the server's default directory when `cwd` is absent.
fn create_command(
    name: String,
    lifetime: WireSessionLifetime,
    cwd: Option<String>,
) -> Result<Command, WireError> {
    session_name(&name)?;
    let lifetime = SessionLifetime::from(lifetime);
    let cwd = cwd.filter(|cwd| !cwd.is_empty());
    let launch = (cwd.is_some() || lifetime == SessionLifetime::Ephemeral)
        .then(|| Box::new(LaunchSpec::new(Vec::new()).with_cwd(cwd)));
    Ok(Command::SessionCreate {
        operation: mint_operation(),
        name,
        lifetime,
        attrs: Vec::new(),
        launch,
    })
}

/// Keep `committed` in `receipts`, dropping the oldest past the bound.
fn remember(receipts: &mut VecDeque<Committed>, committed: &Committed) {
    if receipts.len() >= MAX_RECEIPTS {
        receipts.pop_front();
    }
    receipts.push_back(committed.clone());
}

/// Lower an organization outcome event; `None` for any other event.
pub(super) fn lower(event: &Event) -> Option<WireEvent> {
    match event {
        Event::OrganizationChanged => Some(WireEvent::OrganizationChanged),
        Event::OrganizationUnavailable { message } => Some(WireEvent::OrganizationUnavailable {
            message: message.clone(),
        }),
        Event::OrganizationOperation { operation, outcome } => {
            let read = organization::resolution(outcome);
            Some(WireEvent::OrganizationOperation {
                operation: operation_text(operation),
                status: read.status.word().to_owned(),
                refusal: read.refusal,
                refusal_word: read.refusal_word.to_owned(),
                conflict_revision: read.conflict_revision,
                revision: read.revision,
                message: read.message,
                undoable: matches!(outcome, OrgOutcome::Committed(_)),
                created_session_key: match outcome {
                    OrgOutcome::Committed(committed) => {
                        committed.session.map(|key| key.to_string())
                    }
                    _ => None,
                },
            })
        }
        _ => None,
    }
}

impl RemoteClient {
    /// Lower an organization outcome, keeping a committed receipt for
    /// `undo`. A `kill_session` outcome lowers as
    /// [`WireEvent::KillFinished`] under the id that call returned.
    pub(super) fn organization_event(&self, event: Event) -> Option<WireEvent> {
        if let Event::OrganizationOperation { operation, outcome } = &event {
            if let Some(workflow) = self.kills.lock().unwrap().remove(operation) {
                let read = organization::resolution(outcome);
                return Some(WireEvent::KillFinished {
                    workflow,
                    status: read.status.word().to_owned(),
                    message: read.message,
                    targets: Vec::new(),
                });
            }
            if let OrgOutcome::Committed(committed) = outcome {
                remember(&mut self.receipts.lock().unwrap(), committed);
            }
        }
        lower(&event)
    }

    fn connected(&self) -> Result<Client, WireError> {
        self.runtime_client().ok_or_else(|| reason("not_connected"))
    }
}

#[uniffi::export]
impl RemoteClient {
    /// Reseed the organization replica; `None` before negotiation or when
    /// the server does not advertise `ORGANIZATION`.
    pub fn refresh_organization(&self) -> Option<u32> {
        self.runtime_client()
            .and_then(|client| client.refresh_organization())
    }

    /// Read a session's arrangement into the replica, so `topology()`
    /// reports each pane's group. `None` when the session has no
    /// arrangement or the server does not advertise `ORGANIZATION`.
    ///
    /// # Errors
    /// `invalid_session_key`.
    pub fn fetch_arrangement(&self, session_key_text: String) -> Result<Option<u32>, WireError> {
        let key = session_key(&session_key_text)?;
        let Some(client) = self.runtime_client() else {
            return Ok(None);
        };
        let arrangement = client
            .organization()
            .session(&key)
            .and_then(|session| session.arrangement);
        Ok(arrangement.and_then(|arrangement| client.fetch_arrangement(arrangement)))
    }

    /// The owner-local Intake: the designated Intake session and every session
    /// awaiting organization, with their member records.
    pub fn inbox(&self) -> InboxDescriptor {
        self.intake()
    }

    /// Preferred Intake API; descriptor field names retain binding compatibility.
    pub fn intake(&self) -> IntakeDescriptor {
        let Some(client) = self.runtime_client() else {
            return InboxDescriptor {
                inbox: None,
                sessions: Vec::new(),
                records: Vec::new(),
            };
        };
        let replica = client.organization();
        let view = replica.intake_view();
        let sessions: Vec<String> = view.sessions.iter().map(|s| s.key.to_string()).collect();
        let records = client
            .topology()
            .map(|graph| topology::session_graph(graph, Some(&replica)).records)
            .unwrap_or_default()
            .into_iter()
            .filter(|record| sessions.contains(&record.session_key))
            .map(Into::into)
            .collect();
        InboxDescriptor {
            inbox: view.inbox.map(|key| key.to_string()),
            sessions,
            records,
        }
    }

    /// Create a session.
    ///
    /// # Errors
    /// See the module docs.
    pub fn create_session(
        &self,
        name: String,
        lifetime: WireSessionLifetime,
        cwd: Option<String>,
    ) -> Result<String, WireError> {
        let command = create_command(name, lifetime, cwd)?;
        submitted(self.connected()?.submit_organization(command))
    }

    /// Rename a session.
    ///
    /// # Errors
    /// See the module docs.
    pub fn rename_session(
        &self,
        session_key_text: String,
        name: String,
    ) -> Result<String, WireError> {
        session_name(&name)?;
        let key = session_key(&session_key_text)?;
        submitted(self.connected()?.rename_session(key, &name))
    }

    /// Link a session into a named project, or clear its link with `None`.
    /// A server without `SESSION_PROJECT` refuses `organization_unavailable`.
    ///
    /// # Errors
    /// See the module docs.
    pub fn set_session_project(
        &self,
        session_key_text: String,
        project: Option<String>,
    ) -> Result<String, WireError> {
        let key = session_key(&session_key_text)?;
        submitted(
            self.connected()?
                .set_session_project(key, project.as_deref()),
        )
    }

    /// Make a session persistent or ephemeral.
    ///
    /// # Errors
    /// See the module docs.
    pub fn set_session_lifetime(
        &self,
        session_key_text: String,
        lifetime: WireSessionLifetime,
    ) -> Result<String, WireError> {
        let key = session_key(&session_key_text)?;
        submitted(self.connected()?.set_session_lifetime(key, lifetime.into()))
    }

    /// Delete an empty session; one with members is refused `not_empty`
    /// (use `kill_session`).
    ///
    /// # Errors
    /// See the module docs.
    pub fn delete_session(&self, session_key_text: String) -> Result<String, WireError> {
        let key = session_key(&session_key_text)?;
        submitted(self.connected()?.delete_session(key))
    }

    /// Clear a session's Intake mark; legacy binding name retained.
    ///
    /// # Errors
    /// See the module docs.
    pub fn inbox_resolve(&self, session_key_text: String) -> Result<String, WireError> {
        self.intake_resolve(session_key_text)
    }

    /// Clear a session's Intake mark; its work is untouched.
    ///
    /// # Errors
    /// See the module docs.
    pub fn intake_resolve(&self, session_key_text: String) -> Result<String, WireError> {
        let key = session_key(&session_key_text)?;
        submitted(self.connected()?.intake_resolve(key))
    }

    /// Move resources into a session (membership only; they keep running).
    /// One operation id per atomic batch.
    ///
    /// # Errors
    /// See the module docs.
    pub fn move_resources(&self, keys: Vec<String>, to: String) -> Result<Vec<String>, WireError> {
        let keys = keys
            .iter()
            .map(|key| resource_key(key))
            .collect::<Result<Vec<_>, _>>()?;
        let to = session_key(&to)?;
        Ok(self
            .connected()?
            .move_resources(&keys, to)
            .map_err(submit_error)?
            .iter()
            .map(operation_text)
            .collect())
    }

    /// End a resource's current activation, gracefully or by force.
    ///
    /// # Errors
    /// See the module docs.
    pub fn terminate_resource(&self, key: String, force: bool) -> Result<String, WireError> {
        let key = resource_key(&key)?;
        let mode = if force {
            TerminateMode::Force
        } else {
            TerminateMode::Graceful
        };
        submitted(self.connected()?.terminate_resource(key, mode))
    }

    /// Remove an inactive resource record.
    ///
    /// # Errors
    /// See the module docs.
    pub fn remove_resource(&self, key: String) -> Result<String, WireError> {
        let key = resource_key(&key)?;
        submitted(self.connected()?.remove_resource(key))
    }

    /// Start a new activation of an inactive resource.
    ///
    /// # Errors
    /// See the module docs.
    pub fn relaunch_resource(&self, key: String) -> Result<String, WireError> {
        let key = resource_key(&key)?;
        submitted(self.connected()?.relaunch_resource(key))
    }

    /// Undo a committed organization edit by its operation id. Only the
    /// newest 256 committed receipts this client published can be undone.
    ///
    /// # Errors
    /// See the module docs.
    pub fn undo(&self, operation: String) -> Result<String, WireError> {
        let operation =
            parse_operation(&operation).ok_or_else(|| reason("invalid_operation_id"))?;
        let receipt = self
            .receipts
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|receipt| receipt.operation == operation)
            .cloned()
            .ok_or_else(|| reason("unknown_receipt"))?;
        submitted(self.connected()?.undo(&receipt))
    }

    /// Kill a whole session with one `SESSION_KILL`: the server ends every
    /// member and deletes the session. Returns the id
    /// [`WireEvent::KillFinished`] reports the outcome under.
    ///
    /// # Errors
    /// See the module docs; `organization_unavailable` when the server does
    /// not advertise `SESSION_KILL`.
    pub fn kill_session(&self, session_key_text: String) -> Result<u64, WireError> {
        let key = session_key(&session_key_text)?;
        let operation = self.connected()?.kill_session(key).map_err(submit_error)?;
        let workflow = 1 + self
            .last_kill
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.kills.lock().unwrap().insert(operation, workflow);
        Ok(workflow)
    }
}

#[cfg(test)]
mod tests;
