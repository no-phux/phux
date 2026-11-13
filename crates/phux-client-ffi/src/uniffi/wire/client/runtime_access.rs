//! Snapshot the runtime handle before invoking terminal operations.

use super::*;

impl RemoteClient {
    pub(super) fn runtime_client(&self) -> Option<Client> {
        self.client.lock().unwrap().clone()
    }

    pub(super) fn with_terminal<T>(
        &self,
        terminal_id: &str,
        call: impl FnOnce(&Client, &ResourceId) -> T,
    ) -> Option<T> {
        let id = id::parse(terminal_id)?;
        let client = self.runtime_client()?;
        Some(call(&client, &id))
    }
}
