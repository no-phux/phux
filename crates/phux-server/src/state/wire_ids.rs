//! `ServerState`'s wire-id facade.
//!
//! The tables and allocators live on [`IdSpace`](super::IdSpace); these are
//! the delegating shims the runtime calls, kept on [`ServerState`] because
//! that is the receiver every dispatch site already holds.

use phux_core::ids::{TerminalId, WindowId};
use phux_protocol::ids::{TerminalId as WireTerminalId, WindowId as WireWindowId};

use super::ServerState;

impl ServerState {
    /// Wire pane id for `pane`, allocating one if needed.
    ///
    /// Delegates to `IdSpace::intern_terminal` (crate-internal);
    /// idempotent, and several call sites depend on that.
    pub fn intern_terminal_wire(&mut self, terminal: TerminalId) -> WireTerminalId {
        self.idspace.intern_terminal(terminal)
    }

    /// Reverse lookup: which core pane id (if any) does `wire`
    /// resolve to?
    #[must_use]
    pub fn terminal_from_wire(&self, wire: &WireTerminalId) -> Option<TerminalId> {
        self.idspace.terminal_from_wire(wire)
    }

    /// Wire window id for `window`, allocating one if needed.
    pub fn intern_window_wire(&mut self, window: WindowId) -> WireWindowId {
        self.idspace.intern_window(window)
    }
}
