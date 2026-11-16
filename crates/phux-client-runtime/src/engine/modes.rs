//! Terminal modes a client consults before it encodes input itself, as the
//! replica last saw them.

use libghostty_vt::mode::Mode;

use super::{Command, EngineError, EngineHandle, Query, ResourceId};

impl EngineHandle {
    /// Whether the replica has DEC 2004 bracketed paste on: the mode the
    /// server will encode a paste against.
    pub fn bracketed_paste(&self, terminal_id: &ResourceId) -> Result<bool, EngineError> {
        self.mode(terminal_id, Mode::BRACKETED_PASTE)
    }

    /// Whether the replica has DEC 1007 alternate scroll on: the wheel over
    /// the alternate screen becomes arrow keys. On unless a program resets it.
    pub fn alt_scroll(&self, terminal_id: &ResourceId) -> Result<bool, EngineError> {
        self.mode(terminal_id, Mode::ALT_SCROLL)
    }

    fn mode(&self, terminal_id: &ResourceId, mode: Mode) -> Result<bool, EngineError> {
        self.request(|reply| Command::Query(Query::Mode(terminal_id.clone(), mode, reply)))?
    }
}
