use super::ServerState;

impl ServerState {
    /// Install the `[[startup]]` supervisor handle. Called once, when the
    /// supervisor task is spawned.
    pub(crate) fn set_plugin_startup(&mut self, handle: crate::plugin_startup::StartupHandle) {
        self.plugin_startup = Some(handle);
    }

    /// The supervisor handle, once serving has installed it.
    #[must_use]
    pub(crate) const fn plugin_startup(&self) -> Option<&crate::plugin_startup::StartupHandle> {
        self.plugin_startup.as_ref()
    }
}
