//! The [`Registry`] — single source of truth for sessions, windows, and terminals.
//!
//! All domain entities live in [`slotmap::SlotMap`]s keyed by the typed IDs
//! from [`crate::ids`]. The registry preserves parent → child invariants:
//!
//! * Removing a [`TerminalDescriptor`] removes it from its parent [`Window`]'s `panes`
//!   list and collapses it out of the layout tree.
//! * Removing a [`Window`] cascades to all of its terminals and unlinks the
//!   window from its parent [`Session`].
//! * Removing a [`Session`] cascades fully to every window and terminal it owns.
//!
//! Lookups by an unknown (e.g. removed) key return `None`. Mutating calls
//! that reference an unknown parent return [`RegistryError`].

use std::path::PathBuf;
use std::time::SystemTime;

use slotmap::SlotMap;
use thiserror::Error;

use crate::ids::{SessionId, TerminalId, WindowId};
use crate::session::Session;
use crate::terminal::TerminalDescriptor;
use crate::window::{SplitDir, Window};

/// Errors returned by the [`Registry`] when a parent ID does not resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[allow(clippy::enum_variant_names)] // variants name the *kind* of ID — the shared `Unknown` prefix is the point.
pub enum RegistryError {
    /// The provided [`SessionId`] does not refer to a live session.
    #[error("unknown session id: {0:?}")]
    UnknownSession(SessionId),
    /// The provided [`WindowId`] does not refer to a live window.
    #[error("unknown window id: {0:?}")]
    UnknownWindow(WindowId),
    /// The provided [`TerminalId`] does not refer to a live terminal.
    #[error("unknown terminal id: {0:?}")]
    UnknownTerminal(TerminalId),
}

/// Owns every session, window, and terminal in a running phux server.
///
/// The registry is single-threaded and synchronous; concurrent access is the
/// caller's responsibility (the server crate wraps it behind its actor /
/// event-loop boundary).
#[derive(Debug, Default)]
pub struct Registry {
    sessions: SlotMap<SessionId, Session>,
    windows: SlotMap<WindowId, Window>,
    terminals: SlotMap<TerminalId, TerminalDescriptor>,
}

impl Registry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ---- creation ---------------------------------------------------------

    /// Insert a new session with the given name and return its ID.
    ///
    /// `created_at` is stamped with [`SystemTime::now`]. The new session has
    /// no windows and no active window.
    pub fn new_session(&mut self, name: String) -> SessionId {
        self.sessions.insert_with_key(|id| Session {
            id,
            name,
            windows: Vec::new(),
            active: None,
            created_at: SystemTime::now(),
        })
    }

    /// Insert a new window under `session` and return its ID.
    ///
    /// The new window is appended to the session's `windows` list. If the
    /// session previously had no active window, the new window becomes
    /// active.
    pub fn new_window(&mut self, session: SessionId) -> Result<WindowId, RegistryError> {
        if !self.sessions.contains_key(session) {
            return Err(RegistryError::UnknownSession(session));
        }
        let window_id = self.windows.insert_with_key(|id| Window {
            id,
            session,
            panes: Vec::new(),
            layout: None,
            active: None,
        });
        // Safe: existence checked above.
        if let Some(s) = self.sessions.get_mut(session) {
            s.windows.push(window_id);
            if s.active.is_none() {
                s.active = Some(window_id);
            }
        }
        Ok(window_id)
    }

    /// Insert a new terminal under `window` and return its ID.
    ///
    /// The terminal is appended to the window's `panes` list and inserted into
    /// the layout tree. If the window was empty the new terminal becomes the
    /// sole [`Leaf`](crate::window::LayoutNode::Leaf); otherwise it is added
    /// by splitting the currently active terminal horizontally at `0.5`
    /// (tmux-default behavior). If the window had no active terminal, the new
    /// terminal becomes active. Default dims are `(80, 24)`; cwd defaults to
    /// the empty path; title is `None`.
    pub fn new_terminal(&mut self, window: WindowId) -> Result<TerminalId, RegistryError> {
        if !self.windows.contains_key(window) {
            return Err(RegistryError::UnknownWindow(window));
        }
        let terminal_id = self.terminals.insert_with_key(|id| TerminalDescriptor {
            id,
            window,
            dims: (80, 24),
            cwd: PathBuf::new(),
            title: None,
        });
        if let Some(w) = self.windows.get_mut(window) {
            let target = w.active;
            w.panes.push(terminal_id);
            match target {
                None => {
                    // Window was empty — seed the layout. This cannot fail
                    // because the layout is None here.
                    let _ = w.seed_layout(terminal_id);
                    w.active = Some(terminal_id);
                }
                Some(t) => {
                    // Split the active terminal horizontally at the tmux default
                    // ratio. If the active terminal is somehow not in the tree
                    // (shouldn't happen), fall back to seeding — but we
                    // intentionally swallow the error here rather than
                    // making `new_terminal` fallible on layout grounds; the
                    // proptest invariants would catch any drift.
                    let _ = w.split(t, terminal_id, SplitDir::Horizontal, 0.5);
                }
            }
        }
        Ok(terminal_id)
    }

    /// Re-parent a live terminal into another window (ADR-0056), possibly
    /// in a different session.
    ///
    /// Ownership only: the terminal keeps its id, dims, cwd, and title.
    /// The source window's `panes`, layout, and `active` drop the leaf
    /// exactly as [`Self::remove_terminal`] would; the destination window
    /// gains it exactly as [`Self::new_terminal`] would (seed when empty,
    /// else split the active pane horizontally at `0.5` — geometry is the
    /// caller's L3 concern, this placement is just a valid tree). Moving a
    /// terminal to the window it already occupies is an idempotent no-op.
    /// An emptied source window persists; the caller reaps it by its
    /// existing rules.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownTerminal`] / [`RegistryError::UnknownWindow`]
    /// when either end does not exist; nothing is mutated on error.
    pub fn move_terminal(&mut self, id: TerminalId, window: WindowId) -> Result<(), RegistryError> {
        if !self.windows.contains_key(window) {
            return Err(RegistryError::UnknownWindow(window));
        }
        let source = self
            .terminals
            .get(id)
            .map(|t| t.window)
            .ok_or(RegistryError::UnknownTerminal(id))?;
        if source == window {
            return Ok(());
        }
        if let Some(w) = self.windows.get_mut(source) {
            w.panes.retain(|p| *p != id);
            let _ = w.kill_pane(id);
            if w.active == Some(id) {
                w.active = w.panes.first().copied();
            }
        }
        if let Some(t) = self.terminals.get_mut(id) {
            t.window = window;
        }
        if let Some(w) = self.windows.get_mut(window) {
            let target = w.active;
            w.panes.push(id);
            match target {
                None => {
                    let _ = w.seed_layout(id);
                    w.active = Some(id);
                }
                Some(t) => {
                    let _ = w.split(t, id, SplitDir::Horizontal, 0.5);
                }
            }
        }
        Ok(())
    }

    // ---- removal ----------------------------------------------------------

    /// Remove a terminal and unlink it from its parent window.
    ///
    /// Returns the removed [`TerminalDescriptor`] if it existed, otherwise `None`. The
    /// parent window's `panes`, layout tree, and `active` are all updated
    /// to drop the removed key. When the removed terminal was the only leaf the
    /// window's `layout` is cleared (the window persists with no terminals
    /// until [`Self::remove_window`] is called).
    pub fn remove_terminal(&mut self, id: TerminalId) -> Option<TerminalDescriptor> {
        let terminal = self.terminals.remove(id)?;
        if let Some(w) = self.windows.get_mut(terminal.window) {
            w.panes.retain(|p| *p != id);
            // Collapse the layout. `LastPane` is fine — the layout becomes
            // None and the window can be removed by a subsequent call.
            let _ = w.kill_pane(id);
            if w.active == Some(id) {
                w.active = w.panes.first().copied();
            }
        }
        Some(terminal)
    }

    /// Remove a window, cascading to all of its terminals.
    ///
    /// Returns the removed [`Window`] if it existed. All terminals that
    /// belonged to the window are removed from the terminal map. The parent
    /// session's `windows` and `active` are updated.
    pub fn remove_window(&mut self, id: WindowId) -> Option<Window> {
        let window = self.windows.remove(id)?;
        for terminal_id in &window.panes {
            let _ = self.terminals.remove(*terminal_id);
        }
        if let Some(s) = self.sessions.get_mut(window.session) {
            s.windows.retain(|w| *w != id);
            if s.active == Some(id) {
                s.active = s.windows.first().copied();
            }
        }
        Some(window)
    }

    /// Remove a session, cascading to all of its windows and their terminals.
    ///
    /// Returns the removed [`Session`] if it existed.
    pub fn remove_session(&mut self, id: SessionId) -> Option<Session> {
        let session = self.sessions.remove(id)?;
        for window_id in &session.windows {
            if let Some(window) = self.windows.remove(*window_id) {
                for terminal_id in &window.panes {
                    let _ = self.terminals.remove(*terminal_id);
                }
            }
        }
        Some(session)
    }

    // ---- lookups ----------------------------------------------------------

    /// Borrow a session by ID, or `None` if the ID is unknown.
    #[must_use]
    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(id)
    }

    /// Iterate over every live `(SessionId, &Session)` pair in the registry.
    ///
    /// Order matches [`slotmap::SlotMap::iter`] — i.e. insertion-stable for
    /// the slots currently occupied, but **not** strictly insertion order
    /// across remove+reinsert cycles (a removed slot may be re-occupied by a
    /// later `new_session` and appear earlier in iteration than newer slots).
    /// Callers that need a stable ordering should sort on a session field
    /// they own (e.g. `created_at` or `name`).
    ///
    /// This is the canonical lookup-by-name primitive: the server crate uses
    /// it to resolve `ATTACH` requests without maintaining a side ledger.
    pub fn sessions(&self) -> impl Iterator<Item = (SessionId, &Session)> + '_ {
        self.sessions.iter()
    }

    /// Mutably borrow a session by ID, or `None` if the ID is unknown.
    #[must_use]
    pub fn session_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(id)
    }

    /// Borrow a window by ID, or `None` if the ID is unknown.
    #[must_use]
    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.get(id)
    }

    /// Mutably borrow a window by ID, or `None` if the ID is unknown.
    #[must_use]
    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.get_mut(id)
    }

    /// Borrow a terminal by ID, or `None` if the ID is unknown.
    #[must_use]
    pub fn terminal(&self, id: TerminalId) -> Option<&TerminalDescriptor> {
        self.terminals.get(id)
    }

    /// Mutably borrow a terminal by ID, or `None` if the ID is unknown.
    #[must_use]
    pub fn terminal_mut(&mut self, id: TerminalId) -> Option<&mut TerminalDescriptor> {
        self.terminals.get_mut(id)
    }

    // ---- counts -----------------------------------------------------------

    /// Number of live sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Number of live windows.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }
}
