use super::ServerState;

impl ServerState {
    /// Install the event-hook dispatcher handle (phux-r82.1). Called once
    /// at server startup, after [`crate::hooks::spawn_hook_dispatcher`].
    pub fn set_hook_dispatcher(&mut self, dispatcher: crate::hooks::HookDispatcher) {
        self.hook_dispatcher = Some(dispatcher);
    }

    /// The installed event-hook dispatcher handle, if any. `None` means no
    /// hooks are configured and firing events is a no-op.
    #[must_use]
    pub const fn hook_dispatcher(&self) -> Option<&crate::hooks::HookDispatcher> {
        self.hook_dispatcher.as_ref()
    }
}
