//! `ServerState`'s wire-id facade over [`IdSpace`](super::IdSpace).

use phux_core::ids::{ResourceId, WindowId};
use phux_protocol::ids::{ResourceId as WireResourceId, WindowId as WireWindowId};

use super::ServerState;

impl ServerState {
    /// Wire pane id for `terminal`, allocating if needed (idempotent).
    pub fn intern_terminal_wire(&mut self, terminal: ResourceId) -> WireResourceId {
        self.idspace.intern_terminal(terminal)
    }

    /// Reverse lookup: which core pane id (if any) does `wire`
    /// resolve to?
    #[must_use]
    pub fn terminal_from_wire(&self, wire: &WireResourceId) -> Option<ResourceId> {
        self.idspace.terminal_from_wire(wire)
    }

    /// Wire window id for `window`, allocating one if needed.
    pub fn intern_window_wire(&mut self, window: WindowId) -> WireWindowId {
        self.idspace.intern_window(window)
    }
}
