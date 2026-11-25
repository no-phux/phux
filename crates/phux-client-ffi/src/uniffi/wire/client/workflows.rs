//! Workflows operations for the foreign client.

use super::*;

#[uniffi::export]
impl RemoteClient {
    pub fn attach_session(&self, name: String) {
        if let Some(client) = self.runtime_client() {
            client.attach_session(AttachTarget::ByName(name));
        }
    }

    pub fn attach_terminal(&self, terminal_id: String) -> u32 {
        self.with_terminal(&terminal_id, Client::attach_terminal)
            .unwrap_or(0)
    }

    pub fn detach_terminal(&self, terminal_id: String) -> u32 {
        self.with_terminal(&terminal_id, Client::detach_terminal)
            .unwrap_or(0)
    }

    /// Queue exact terminal creation; never hides accepted work behind zero.
    pub fn spawn_terminal(&self) -> WireWorkflowSubmission {
        self.submit_spawn(SpawnRequest::default())
    }

    pub fn spawn_terminal_in(&self, session: String) -> WireWorkflowSubmission {
        self.spawn_terminal_in_directory(session, None)
    }

    /// Create a terminal in an existing session, without ensuring or selecting it.
    pub fn spawn_terminal_in_directory(
        &self,
        session: String,
        cwd: Option<String>,
    ) -> WireWorkflowSubmission {
        let Some(client) = self.runtime_client() else {
            return WireWorkflowSubmission::NotQueued {
                reason: "not_connected".into(),
            };
        };
        client.with_control(|control| {
            let Some(session_id) = control
                .topology()
                .and_then(|topology| topology.session_named(&session).map(|item| item.id))
            else {
                return WireWorkflowSubmission::NotQueued {
                    reason: "session_unavailable".into(),
                };
            };
            control
                .submit_spawn(SpawnRequest {
                    cwd,
                    session_id: Some(session_id),
                    ..SpawnRequest::default()
                })
                .into()
        })
    }

    pub fn spawn_terminal_with_command(&self, command: Vec<String>) -> WireWorkflowSubmission {
        self.submit_spawn(SpawnRequest {
            command: Some(command),
            ..SpawnRequest::default()
        })
    }

    pub fn kill_terminal(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, Client::kill_terminal);
    }
    /// Request graceful close per live activation on organization peers,
    /// or an advertised keep-empty batch on older peers. The id correlates
    /// `WireEvent::TerminalsClosed`. Zero means no request was queued.
    /// Pane lifecycle events report closure.
    pub fn close_terminals(&self, terminal_ids: Vec<String>) -> u32 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let Some(ids) = terminal_ids
            .iter()
            .map(|id| id::parse(id))
            .collect::<Option<Vec<_>>>()
        else {
            return 0;
        };
        client.close_terminals(ids).unwrap_or(0)
    }
}

impl RemoteClient {
    fn submit_spawn(&self, request: SpawnRequest) -> WireWorkflowSubmission {
        self.runtime_client().map_or_else(
            || WireWorkflowSubmission::NotQueued {
                reason: "not_connected".into(),
            },
            |client| client.submit_spawn(request).into(),
        )
    }
}

#[cfg(test)]
mod tests;
