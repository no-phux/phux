//! [`ResourceDescriptor`] — the leaf record for anything the server serves.
//!
//! A resource is one server-owned addressable thing: a PTY-plus-libghostty
//! Terminal, or an agent session whose output is producer-fed. Every
//! resource has a [`ResourceKind`]; the kind fixes which *facet* the
//! descriptor carries. The facets are plain data. The server attaches the
//! engine (libghostty terminal, PTY plumbing, record ring) on top of this
//! record, keyed by [`ResourceId`].

use crate::ids::{ResourceId, WindowId};
use crate::terminal::TerminalFacet;

/// What kind of thing a resource is. Open: a client that meets a kind it
/// does not know must treat the resource as opaque, never as a Terminal.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A PTY child behind a libghostty terminal. Accepts input atoms, has
    /// a grid, and occupies a window slot.
    Terminal,
    /// An agent harness session. Producer-fed output, no grid, no input
    /// atoms; always bound to a Terminal parent.
    AgentSession,
}

impl ResourceKind {
    /// Short lowercase name for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::AgentSession => "agent-session",
        }
    }
}

impl std::fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The [`ResourceKind::AgentSession`] facet: which harness the session
/// belongs to and what it is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFacet {
    /// Harness that produces the session's records (for example `claude`).
    pub provider: String,
    /// The provider's own opaque session id, when it has one.
    pub native_id: Option<String>,
    /// Derived activity state, when the stream has established one.
    pub state: Option<String>,
}

/// Descriptor for a single resource.
///
/// Pure data — no PTY, no grid, no async state. Exactly the facet named by
/// `kind` is populated: a `Terminal` carries `terminal` and `window`, an
/// `AgentSession` carries `agent` and `parent`. The [`Registry`] upholds
/// that shape; nothing outside it constructs a descriptor.
///
/// [`Registry`]: crate::registry::Registry
#[derive(Debug, Clone)]
pub struct ResourceDescriptor {
    /// The stable identifier issued by the [`Registry`].
    ///
    /// [`Registry`]: crate::registry::Registry
    pub id: ResourceId,
    /// Which facet this resource carries.
    pub kind: ResourceKind,
    /// The resource this one is bound to. Set at creation, immutable for
    /// the resource's lifetime; closing the parent closes the child.
    pub parent: Option<ResourceId>,
    /// The window whose layout holds this resource. Terminal kind only.
    pub window: Option<WindowId>,
    /// Terminal facet, present iff `kind == Terminal`.
    pub terminal: Option<TerminalFacet>,
    /// Agent-session facet, present iff `kind == AgentSession`.
    pub agent: Option<AgentFacet>,
}

impl ResourceDescriptor {
    /// Borrow the Terminal facet, or `None` for another kind.
    #[must_use]
    pub const fn terminal(&self) -> Option<&TerminalFacet> {
        self.terminal.as_ref()
    }

    /// Mutably borrow the Terminal facet, or `None` for another kind.
    #[must_use]
    pub const fn terminal_mut(&mut self) -> Option<&mut TerminalFacet> {
        self.terminal.as_mut()
    }

    /// Borrow the agent-session facet, or `None` for another kind.
    #[must_use]
    pub const fn agent(&self) -> Option<&AgentFacet> {
        self.agent.as_ref()
    }

    /// Mutably borrow the agent-session facet, or `None` for another kind.
    #[must_use]
    pub const fn agent_mut(&mut self) -> Option<&mut AgentFacet> {
        self.agent.as_mut()
    }
}
