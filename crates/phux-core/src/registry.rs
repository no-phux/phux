//! The [`Registry`] — single source of truth for sessions, windows, and
//! resources.
//!
//! Removal cascades parent to child: session -> windows -> slot resources
//! -> bound child resources. Unknown keys look up as `None`; mutations
//! naming an unknown parent return [`RegistryError`].

use std::path::PathBuf;
use std::time::SystemTime;

use slotmap::SlotMap;
use thiserror::Error;

use crate::ids::{ResourceId, SessionId, WindowId};
use crate::resource::{AgentFacet, ResourceDescriptor, ResourceKind};
use crate::session::Session;
use crate::terminal::TerminalFacet;
use crate::window::{SplitDir, Window};

/// Errors returned by the [`Registry`] when a parent ID does not resolve or
/// has the wrong kind for the requested binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RegistryError {
    /// The provided [`SessionId`] does not refer to a live session.
    #[error("unknown session id: {0:?}")]
    UnknownSession(SessionId),
    /// The provided [`WindowId`] does not refer to a live window.
    #[error("unknown window id: {0:?}")]
    UnknownWindow(WindowId),
    /// The provided [`ResourceId`] does not refer to a live resource.
    #[error("unknown resource id: {0:?}")]
    UnknownResource(ResourceId),
    /// The named parent exists but is not of the kind the binding requires.
    #[error("resource {parent:?} is {actual}, binding requires {required}")]
    ParentKindMismatch {
        /// The parent that was offered.
        parent: ResourceId,
        /// What the parent actually is.
        actual: ResourceKind,
        /// What the binding needs the parent to be.
        required: ResourceKind,
    },
}

/// Owns every session, window, and resource in a running phux server.
///
/// The registry is single-threaded and synchronous; concurrent access is the
/// caller's responsibility (the server crate wraps it behind its actor /
/// event-loop boundary).
#[derive(Debug, Default)]
pub struct Registry {
    sessions: SlotMap<SessionId, Session>,
    windows: SlotMap<WindowId, Window>,
    resources: SlotMap<ResourceId, ResourceDescriptor>,
}

impl Registry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ---- creation ---------------------------------------------------------

    /// Insert a new, empty session with the given name.
    pub fn new_session(&mut self, name: String) -> SessionId {
        self.sessions.insert_with_key(|id| Session {
            id,
            name,
            windows: Vec::new(),
            active: None,
            created_at: SystemTime::now(),
            keep_empty: false,
        })
    }

    /// Append a new window to `session`, making it active if none was.
    pub fn new_window(&mut self, session: SessionId) -> Result<WindowId, RegistryError> {
        if !self.sessions.contains_key(session) {
            return Err(RegistryError::UnknownSession(session));
        }
        let window_id = self.windows.insert_with_key(|id| Window {
            id,
            session,
            slots: Vec::new(),
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

    /// Insert a new Terminal-kind resource under `window` and return its ID.
    ///
    /// It seeds an empty layout (and becomes active) or splits the active
    /// terminal horizontally at `0.5`. Dims default to `(80, 24)`.
    pub fn new_terminal(&mut self, window: WindowId) -> Result<ResourceId, RegistryError> {
        if !self.windows.contains_key(window) {
            return Err(RegistryError::UnknownWindow(window));
        }
        let terminal_id = self.resources.insert_with_key(|id| ResourceDescriptor {
            id,
            kind: ResourceKind::Terminal,
            parent: None,
            window: Some(window),
            terminal: Some(TerminalFacet {
                dims: (80, 24),
                cwd: PathBuf::new(),
                title: None,
            }),
            agent: None,
        });
        if let Some(w) = self.windows.get_mut(window) {
            Self::place_in_window(w, terminal_id);
        }
        Ok(terminal_id)
    }

    /// Insert a new AgentSession-kind resource bound (permanently) to the
    /// Terminal `parent`. It occupies no window slot.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownResource`] when `parent` does not exist;
    /// [`RegistryError::ParentKindMismatch`] when it is not a Terminal.
    pub fn new_agent_session(
        &mut self,
        parent: ResourceId,
        agent: AgentFacet,
    ) -> Result<ResourceId, RegistryError> {
        let parent_kind = self
            .resources
            .get(parent)
            .map(|r| r.kind)
            .ok_or(RegistryError::UnknownResource(parent))?;
        if parent_kind != ResourceKind::Terminal {
            return Err(RegistryError::ParentKindMismatch {
                parent,
                actual: parent_kind,
                required: ResourceKind::Terminal,
            });
        }
        Ok(self.resources.insert_with_key(|id| ResourceDescriptor {
            id,
            kind: ResourceKind::AgentSession,
            parent: Some(parent),
            window: None,
            terminal: None,
            agent: Some(agent),
        }))
    }

    /// Re-parent a live terminal into another window (ADR-0056), keeping its
    /// id, facet, and children. The source drops the leaf as
    /// [`Self::remove_resource`] would; the destination gains it as
    /// [`Self::new_terminal`] would. Moving to the current window is a no-op.
    ///
    /// # Errors
    ///
    /// `UnknownWindow` / `UnknownResource` when either end is missing or `id`
    /// holds no window slot; nothing is mutated on error.
    pub fn move_terminal(&mut self, id: ResourceId, window: WindowId) -> Result<(), RegistryError> {
        if !self.windows.contains_key(window) {
            return Err(RegistryError::UnknownWindow(window));
        }
        let source = self
            .resources
            .get(id)
            .and_then(|t| t.window)
            .ok_or(RegistryError::UnknownResource(id))?;
        if source == window {
            return Ok(());
        }
        if let Some(w) = self.windows.get_mut(source) {
            Self::vacate_slot(w, id);
        }
        if let Some(t) = self.resources.get_mut(id) {
            t.window = Some(window);
        }
        if let Some(w) = self.windows.get_mut(window) {
            Self::place_in_window(w, id);
        }
        Ok(())
    }

    /// Append `id` to `w.slots` and place it in the layout: seed when the
    /// window is empty, else split the active slot horizontally at `0.5`.
    fn place_in_window(w: &mut Window, id: ResourceId) {
        let target = w.active;
        w.slots.push(id);
        match target {
            None => {
                let _ = w.seed_layout(id);
                w.active = Some(id);
            }
            Some(t) => {
                // A layout error is not a registry error: slots stay
                // authoritative.
                let _ = w.split(t, id, SplitDir::Horizontal, 0.5);
            }
        }
    }

    /// Drop `id` from `w.slots` and the layout, and move focus off it.
    /// `LastPane` is fine: the layout becomes `None`.
    fn vacate_slot(w: &mut Window, id: ResourceId) {
        w.slots.retain(|p| *p != id);
        let _ = w.kill_pane(id);
        if w.active == Some(id) {
            w.active = w.slots.first().copied();
        }
    }

    // ---- removal ----------------------------------------------------------

    /// Remove a resource, its bound descendants, and (for a Terminal) its
    /// window slot. An emptied window persists until [`Self::remove_window`].
    pub fn remove_resource(&mut self, id: ResourceId) -> Option<ResourceDescriptor> {
        let resource = self.resources.remove(id)?;
        self.remove_children_of(id);
        if let Some(w) = resource.window.and_then(|wid| self.windows.get_mut(wid)) {
            Self::vacate_slot(w, id);
        }
        Some(resource)
    }

    /// Remove every resource bound to `parent`, recursively, so no
    /// descendant outlives its ancestor.
    fn remove_children_of(&mut self, parent: ResourceId) {
        for child in self.children(parent) {
            self.remove_children_of(child);
            self.resources.remove(child);
        }
    }

    /// Drop every resource in `window`'s slots and their descendants.
    fn remove_slots_of(&mut self, window: &Window) {
        for terminal_id in &window.slots {
            self.resources.remove(*terminal_id);
            self.remove_children_of(*terminal_id);
        }
    }

    /// Remove a window, cascading to its slots' resources and their
    /// children, and unlink it from its session.
    pub fn remove_window(&mut self, id: WindowId) -> Option<Window> {
        let window = self.windows.remove(id)?;
        self.remove_slots_of(&window);
        if let Some(s) = self.sessions.get_mut(window.session) {
            s.windows.retain(|w| *w != id);
            if s.active == Some(id) {
                s.active = s.windows.first().copied();
            }
        }
        Some(window)
    }

    /// Remove a session, cascading to all of its windows and their
    /// resources.
    pub fn remove_session(&mut self, id: SessionId) -> Option<Session> {
        let session = self.sessions.remove(id)?;
        for window_id in &session.windows {
            if let Some(window) = self.windows.remove(*window_id) {
                self.remove_slots_of(&window);
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

    /// Iterate over every live session in slotmap order, which is not
    /// insertion order across remove/reinsert; sort on a field if it matters.
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

    /// Borrow a resource of any kind by ID, or `None` if the ID is unknown.
    #[must_use]
    pub fn resource(&self, id: ResourceId) -> Option<&ResourceDescriptor> {
        self.resources.get(id)
    }

    /// Mutably borrow a resource of any kind by ID, or `None` if the ID is
    /// unknown.
    #[must_use]
    pub fn resource_mut(&mut self, id: ResourceId) -> Option<&mut ResourceDescriptor> {
        self.resources.get_mut(id)
    }

    /// Iterate over every live `(ResourceId, &ResourceDescriptor)` pair.
    /// Same ordering caveat as [`Self::sessions`].
    pub fn resources(&self) -> impl Iterator<Item = (ResourceId, &ResourceDescriptor)> + '_ {
        self.resources.iter()
    }

    /// Borrow the Terminal facet of resource `id`: `None` if the ID is
    /// unknown or the resource is not a Terminal.
    #[must_use]
    pub fn terminal(&self, id: ResourceId) -> Option<&TerminalFacet> {
        self.resources
            .get(id)
            .and_then(ResourceDescriptor::terminal)
    }

    /// Mutably borrow the Terminal facet of resource `id`: `None` if the ID
    /// is unknown or the resource is not a Terminal.
    #[must_use]
    pub fn terminal_mut(&mut self, id: ResourceId) -> Option<&mut TerminalFacet> {
        self.resources
            .get_mut(id)
            .and_then(ResourceDescriptor::terminal_mut)
    }

    /// The resources bound to `parent`, in slotmap iteration order.
    #[must_use]
    pub fn children(&self, parent: ResourceId) -> Vec<ResourceId> {
        self.resources
            .iter()
            .filter(|(_, r)| r.parent == Some(parent))
            .map(|(id, _)| id)
            .collect()
    }

    /// Number of live sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Number of live Terminal-kind resources across every session.
    #[must_use]
    pub fn terminal_count(&self) -> usize {
        self.resources
            .values()
            .filter(|r| r.kind == ResourceKind::Terminal)
            .count()
    }
}
