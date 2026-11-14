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

/// What kind of thing a resource is.
///
/// Closed on purpose: every match in the workspace is exhaustive, so a new
/// kind does not compile until each one answers for it. The open form is the
/// wire tag, whose `Unknown` a client must treat as opaque, never as a
/// Terminal.
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
    /// A replicated accessibility tree of an application, produced by a
    /// helper that runs in a Terminal; the newest snapshot wins.
    AccessibilityTree,
    /// A replicated pixel surface, a window or display mirrored as images by
    /// a helper that runs in a Terminal; the newest image wins, and its size
    /// is the surface's geometry.
    PixelSurface,
    /// A replicated document of any content type, fixed when it is opened
    /// (Markdown, SVG, JSON, PNG...), published by a helper that runs in a
    /// Terminal; the newest document wins.
    Document,
}

impl ResourceKind {
    /// Short lowercase name for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::AgentSession => "agent-session",
            Self::AccessibilityTree => "accessibility-tree",
            Self::PixelSurface => "pixel-surface",
            Self::Document => "document",
        }
    }
}

impl ResourceKind {
    /// Whether a resource of this kind must be created under a parent: a
    /// producer-fed projection binds to the resource that supervises its
    /// producer (docs/resource-kinds.md, Producers).
    #[must_use]
    pub const fn requires_parent(self) -> bool {
        match self {
            Self::Terminal => false,
            Self::AgentSession | Self::AccessibilityTree | Self::PixelSurface | Self::Document => {
                true
            }
        }
    }

    /// Whether a resource of this kind can be such a parent: it runs a
    /// process that can supervise a producer. A projection runs none. This is
    /// the structural fact the organization can know; whether a live parent
    /// is fit to supervise a given producer is its app's check at spawn.
    #[must_use]
    pub const fn can_parent(self) -> bool {
        match self {
            Self::Terminal => true,
            Self::AgentSession | Self::AccessibilityTree | Self::PixelSurface | Self::Document => {
                false
            }
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
    /// What the program in the pane last asked to be resumed with. Never
    /// sent to a client; a relaunch runs the user's `[[restore.program]]`
    /// command for its app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeToken>,
}

/// An opaque id a program handed over (`resume=` on OSC 7501) so the
/// command the user allowlisted for `app` can pick up where it left off.
/// Untrusted: it names no command and is only ever one whole argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeToken {
    /// The OSC 7501 `app` that reported the token.
    pub app: String,
    /// The token.
    pub token: String,
}

impl ResumeToken {
    /// Longest accepted token, in bytes.
    pub const MAX_TOKEN_BYTES: usize = 256;

    /// `app` and `token` as a token, when both are well formed
    /// ([`Self::is_valid`]).
    #[must_use]
    pub fn new(app: &str, token: &str) -> Option<Self> {
        let resume = Self {
            app: app.to_owned(),
            token: token.to_owned(),
        };
        resume.is_valid().then_some(resume)
    }

    /// Whether `app` is 1 to 32 bytes of `[A-Za-z0-9_.+-]`.
    #[must_use]
    pub fn valid_app(app: &str) -> bool {
        (1..=32).contains(&app.len())
            && app
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
    }

    /// Whether the app is well formed and the token is 1 to 256 bytes of
    /// `[A-Za-z0-9_.+,/=-]` that do not start with `-`. A stored record is
    /// checked again before use: the store is a file someone can edit.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        Self::valid_app(&self.app)
            && (1..=Self::MAX_TOKEN_BYTES).contains(&self.token.len())
            && !self.token.starts_with('-')
            && self
                .token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.+,/=-".contains(&b))
    }
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
    /// A [`ResourceKind::AccessibilityTree`] binding.
    Tree(TreeLaunch),
    /// A [`ResourceKind::PixelSurface`] binding.
    Surface(SurfaceLaunch),
    /// A [`ResourceKind::Document`] binding.
    Document(DocumentLaunch),
}

impl LaunchSpec {
    /// The kind this spec launches.
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        match self {
            Self::Terminal(_) => ResourceKind::Terminal,
            Self::Agent(_) => ResourceKind::AgentSession,
            Self::Tree(_) => ResourceKind::AccessibilityTree,
            Self::Surface(_) => ResourceKind::PixelSurface,
            Self::Document(_) => ResourceKind::Document,
        }
    }
}

/// What an accessibility tree mirrors: the application its producer
/// observes, as the producer names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeLaunch {
    /// The observed application, e.g. a bundle id or a window title.
    pub target: String,
}

/// What a pixel surface mirrors: the window or display its producer
/// captures, as the producer names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceLaunch {
    /// The captured window or display.
    pub target: String,
}

/// What a document holds: its content type, a MIME type such as
/// `text/markdown`, fixed for the resource's life.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLaunch {
    /// The documents' content type.
    pub content_type: String,
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

#[cfg(test)]
mod tests {
    use super::{ResumeToken, TerminalLaunch};

    #[test]
    fn resume_tokens_are_bounded_plain_and_never_option_shaped() {
        let longest = "a".repeat(ResumeToken::MAX_TOKEN_BYTES);
        for token in ["abc123", "a/b_c.d+e,f=g-h", longest.as_str()] {
            assert!(ResumeToken::new("nvim", token).is_some(), "{token}");
        }
        let too_long = format!("{longest}a");
        for token in [
            "", "-rf", "--help", "a b", "a;b", "a$b", "a\nb", "a:b", "é", &too_long,
        ] {
            assert!(ResumeToken::new("nvim", token).is_none(), "{token:?}");
        }
        let app = "a".repeat(33);
        for app in ["", "my app", "a/b", "a,b", app.as_str()] {
            assert!(ResumeToken::new(app, "abc").is_none(), "{app:?}");
        }
    }

    /// A launch without a token is stored exactly as before the field.
    #[test]
    fn a_launch_without_a_token_serializes_without_the_field() {
        let json = serde_json::to_string(&TerminalLaunch::default()).unwrap();
        assert!(!json.contains("resume"), "{json}");
        let launch: TerminalLaunch = serde_json::from_str(&json).unwrap();
        assert_eq!(launch.resume, None);
    }
}
