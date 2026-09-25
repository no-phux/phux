//! Input, subscription, and protocol extension methods.

use super::{
    Client, Command, ControlPlane, FocusEvent, FrameKind, KeyEvent, MouseEvent, PasteTrust,
    ResourceId, lock,
};

impl Client {
    // ----- input ------------------------------------------------------

    /// Send one key on the raw path.
    #[must_use]
    pub fn send_key(&self, terminal_id: &ResourceId, event: KeyEvent) -> bool {
        self.inner
            .with(|control| control.send_key(terminal_id, event))
    }

    /// Type text on the raw path.
    #[must_use]
    pub fn send_text(&self, terminal_id: &ResourceId, text: &str) -> bool {
        self.inner
            .with(|control| control.send_text(terminal_id, text))
    }

    /// Send a paste as one frame.
    #[must_use]
    pub fn send_paste(&self, terminal_id: &ResourceId, data: Vec<u8>, trust: PasteTrust) -> bool {
        self.inner
            .with(|control| control.send_paste(terminal_id, data, trust))
    }

    /// Send one mouse event.
    #[must_use]
    pub fn send_mouse(&self, terminal_id: &ResourceId, event: MouseEvent) -> bool {
        self.inner
            .with(|control| control.send_mouse(terminal_id, event))
    }

    /// Report host focus.
    #[must_use]
    pub fn send_focus(&self, terminal_id: &ResourceId, event: FocusEvent) -> bool {
        self.inner
            .with(|control| control.send_focus(terminal_id, event))
    }

    /// Atomically deliver a line and Enter through the acknowledged path.
    #[must_use]
    pub fn apply_line(&self, terminal_id: &ResourceId, text: &str) -> u64 {
        self.acknowledged(|control| control.apply_line(terminal_id, text))
    }

    /// Atomically deliver an untrusted paste through the acknowledged path.
    #[must_use]
    pub fn apply_paste(&self, terminal_id: &ResourceId, text: &str) -> u64 {
        self.acknowledged(|control| control.apply_paste(terminal_id, text))
    }

    /// Atomically flush a draft and press Tab through the acknowledged path.
    #[must_use]
    pub fn apply_tab_completion(&self, terminal_id: &ResourceId, text: &str) -> u64 {
        self.acknowledged(|control| control.apply_tab_completion(terminal_id, text))
    }

    /// Publish an immediate binding-boundary refusal from the runtime's
    /// correlation sequence (for example, an invalid textual resource id).
    #[must_use]
    pub fn refuse_acknowledged_input(&self, message: &str) -> u64 {
        let message = message.to_owned();
        self.acknowledged(|control| control.refuse_acknowledged_input(&message))
    }

    fn acknowledged(&self, f: impl FnOnce(&mut ControlPlane) -> u64) -> u64 {
        let (id, resolved) = self.inner.with(|control| {
            let before = control.has_events();
            let id = f(control);
            (id, !before && control.has_events())
        });
        if resolved {
            self.inner.wake();
        }
        id
    }

    /// Whether raw input for the terminal would pass the server's gate now.
    #[must_use]
    pub fn input_ready(&self, terminal_id: &ResourceId) -> bool {
        lock(&self.inner.control).input_ready(terminal_id)
    }

    /// Whether the terminal is fenced behind an unknown delivery.
    #[must_use]
    pub fn delivery_fenced(&self, terminal_id: &ResourceId) -> bool {
        lock(&self.inner.control).delivery_fenced(terminal_id)
    }

    /// Confirm that a fresh authoritative projection reached the consumer.
    pub fn acknowledge_projection(&self, terminal_id: &ResourceId) {
        self.inner
            .with(|control| control.acknowledge_projection(terminal_id));
    }

    /// Capture the exact delivery fence to associate with an authoritative
    /// projection. For an atomic projection snapshot use `with_control`.
    #[must_use]
    pub fn projection_fence(
        &self,
        terminal_id: &ResourceId,
    ) -> Option<crate::control::ProjectionFence> {
        lock(&self.inner.control).projection_fence(terminal_id)
    }

    /// Validate and clear the captured fence under the control-owner lock.
    /// This never clears a newer Unknown, even within the same connection.
    #[must_use]
    pub fn acknowledge_projection_if(
        &self,
        terminal_id: &ResourceId,
        expected: crate::control::ProjectionFence,
    ) -> bool {
        self.inner
            .with(|control| control.acknowledge_projection_if(terminal_id, expected))
    }

    // ----- events subscription and extension points --------------------

    /// Subscribe to the server-wide event stream from a journal cursor.
    pub fn subscribe_events(&self, after_seq: Option<u64>) {
        self.inner
            .with(|control| control.subscribe_events(after_seq));
    }

    /// Extension point: send any `COMMAND`; its reply is
    /// [`Event::CommandResult`](crate::control::Event::CommandResult).
    #[must_use]
    pub fn send_command(&self, command: Command) -> u32 {
        self.inner.with(|control| control.send_command(command))
    }

    /// Extension point: allocate a correlation id from the runtime's one
    /// sequence, so binding-owned frames cannot collide with built-in work.
    #[must_use]
    pub fn next_request_id(&self) -> u32 {
        self.inner.with(ControlPlane::next_request_id)
    }

    /// Extension point: queue any frame.
    pub fn queue_frame(&self, frame: &FrameKind) {
        self.inner.with(|control| control.queue_frame(frame));
    }
}
