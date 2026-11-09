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

    /// Install the standing-mirror supervisor's reload doorbell.
    pub(crate) fn set_standing_mirrors(&mut self, reload: std::sync::Arc<tokio::sync::Notify>) {
        self.standing_mirrors = Some(reload);
    }

    /// The standing-mirror supervisor's reload doorbell, once installed.
    #[must_use]
    pub(crate) const fn standing_mirrors(&self) -> Option<&std::sync::Arc<tokio::sync::Notify>> {
        self.standing_mirrors.as_ref()
    }
}
