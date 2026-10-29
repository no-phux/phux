//! Resources: the persistent [`Resource`] record and the live
//! [`ResourceDescriptor`] the server serves.
//!
//! A resource is an identified thing with a kind and a lifecycle. The
//! record carries organization and execution facts; the descriptor carries
//! live, kind-specific facets keyed by the in-memory [`ResourceId`] handle.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::activation::{Activation, ExitPolicy};
use crate::actor::Actor;
use crate::ids::ResourceId;
use crate::keys::{OperationId, ResourceKey, Revision, SessionKey};
use crate::terminal::TerminalFacet;

/// What kind of thing a resource is. Open: a client that meets a kind it
/// does not know must treat the resource as opaque, never as a Terminal.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// A PTY child behind a libghostty terminal. Accepts input atoms and
    /// has a grid.
    Terminal,
    /// An agent conversation: producer-fed output, no grid, no input atoms;
    /// bound to a Terminal parent. The spelling stays for wire
    /// compatibility.
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

/// How a Terminal starts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalLaunch {
    /// Program and arguments; empty means the user's login shell.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Working directory.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Extra environment, in order.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Opaque agent-session provenance the server installs on the Terminal
    /// at every activation, so a relaunch resumes the same conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<Vec<u8>>,
}

/// How an agent conversation binds to its harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunch {
    /// Harness that produces the conversation (for example `claude`).
    pub provider: String,
    /// The provider's own conversation id, when it has one.
    #[serde(default)]
    pub native_id: Option<String>,
}

/// Kind-specific instructions for starting (or relaunching) a resource.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchSpec {
    /// A [`ResourceKind::Terminal`] launch.
    Terminal(TerminalLaunch),
    /// A [`ResourceKind::AgentSession`] binding.
    Agent(AgentLaunch),
}

impl LaunchSpec {
    /// The kind this spec launches.
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        match self {
            Self::Terminal(_) => ResourceKind::Terminal,
            Self::Agent(_) => ResourceKind::AgentSession,
        }
    }
}

/// Where a resource came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Who created it.
    pub created_by: Actor,
    /// The operation that created it, so a late retry can be matched.
    pub operation: OperationId,
    /// The resource it is bound to or was opened from. May name a resource
    /// that no longer exists.
    #[serde(default)]
    pub parent: Option<ResourceKey>,
}

/// A resource record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// Persistent key.
    pub key: ResourceKey,
    /// The one session it is a member of.
    pub session: SessionKey,
    /// Optional human label.
    #[serde(default)]
    pub name: Option<String>,
    /// Which kind it is.
    pub kind: ResourceKind,
    /// Who created it, and from what.
    pub provenance: Provenance,
    /// How a relaunch starts; `None` for a resource that cannot launch.
    #[serde(default)]
    pub launch: Option<LaunchSpec>,
    /// What an observed exit does to the record.
    pub on_exit: ExitPolicy,
    /// Keep the durable result when its engine naturally ends or its screen expires.
    /// Legacy timed screen retention leaves this false; explicit purge always removes.
    #[serde(default)]
    pub durable_result: bool,
    /// The current or most recent execution.
    #[serde(default)]
    pub activation: Option<Activation>,
    /// Bumps on every committed change to this record.
    pub revision: Revision,
}

/// The [`ResourceKind::AgentSession`] facet: which harness the conversation
/// belongs to and what it is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFacet {
    /// Harness that produces the conversation's records.
    pub provider: String,
    /// The provider's own opaque conversation id, when it has one.
    pub native_id: Option<String>,
    /// Derived activity state, when the stream has established one.
    pub state: Option<String>,
}

/// Live descriptor for a resource the server serves. Exactly the facet
/// named by `kind` is populated (`Terminal`: `terminal`; `AgentSession`:
/// `agent`); the registry upholds that shape.
#[derive(Debug, Clone)]
pub struct ResourceDescriptor {
    /// The in-memory handle issued by the registry.
    pub id: ResourceId,
    /// The record this descriptor serves.
    pub key: ResourceKey,
    /// Which facet this resource carries.
    pub kind: ResourceKind,
    /// The live handle of the record's provenance parent, when that parent
    /// exists.
    pub parent: Option<ResourceId>,
    /// Terminal facet, present iff `kind == Terminal`.
    pub terminal: Option<TerminalFacet>,
    /// Agent facet, present iff `kind == AgentSession`.
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

    /// Borrow the agent facet, or `None` for another kind.
    #[must_use]
    pub const fn agent(&self) -> Option<&AgentFacet> {
        self.agent.as_ref()
    }
}
