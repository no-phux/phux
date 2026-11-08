//! The document resources, an `AccessibilityTree`, a `PixelSurface` or a
//! typed `Document`, over a [`Connection`].
//!
//! Open one under the Terminal its producer runs in, publish whole
//! documents to it, read its newest document, or follow every document as
//! it arrives (`docs/resource-kinds.md`). A tree's documents are JSON; a
//! surface's are PNG or JPEG images; a typed document's are of the content type it
//! was opened with.

use phux_protocol::caps::{BootstrapStreamProfile, ServerFeatureExt};
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlClass, ErrorCode, FrameKind,
    SpawnError, SpawnResource, SpawnResult,
};

use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};

mod input;
pub use input::{DocumentInput, DocumentKey, Intent, intent, send_input};

/// Which document kind a call is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An application's accessibility tree, as JSON.
    Tree,
    /// A window or display's pixels, as PNG or JPEG images.
    Surface,
    /// A document of the content type it was opened with; the `target` of
    /// a call that opens one is that content type.
    Document,
}

impl Kind {
    /// The server feature bit that says the kind is served.
    #[must_use]
    pub const fn feature(self) -> ServerFeatureExt {
        match self {
            Self::Tree => ServerFeatureExt::AccessibilityTree,
            Self::Surface => ServerFeatureExt::PixelSurface,
            Self::Document => ServerFeatureExt::TypedDocument,
        }
    }

    /// How a message names the kind.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tree => "accessibility tree",
            Self::Surface => "pixel surface",
            Self::Document => "document",
        }
    }

    fn spawn(self, parent: ResourceId, target: &str) -> SpawnResource {
        match self {
            Self::Tree => SpawnResource::accessibility_tree(parent, target),
            Self::Surface => SpawnResource::pixel_surface(parent, target),
            Self::Document => SpawnResource::document(parent, target, None),
        }
    }
}

pub use phux_core::image::{ImageFormat, image_size};

/// The width and height a PNG's `IHDR` names, when `bytes` is one.
#[must_use]
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    match image_size(bytes) {
        Ok((ImageFormat::Png, width, height)) => Some((width, height)),
        _ => None,
    }
}

const REQUEST_SPAWN: u32 = 1;
const REQUEST_PUBLISH: u32 = 3;
const REQUEST_ATTACH: u32 = 4;
const REQUEST_DETACH: u32 = 5;

/// One document of a tree, at the sequence it carries. `seq` is `0` and
/// `bytes` empty before the producer's first publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// The stream sequence of this document.
    pub seq: u64,
    /// The document: JSON for a tree, PNG or JPEG for a surface.
    pub bytes: Vec<u8>,
    /// A typed document's content type; `None` for a tree or surface.
    pub content_type: Option<String>,
}

/// A publish the server took.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct Published {
    /// The sequence the document carries.
    pub seq: u64,
    /// When the server took it, Unix milliseconds.
    pub ts_ms: u64,
}

/// A tree just opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The tree.
    pub resource: ResourceId,
    /// The Terminal it is bound to.
    pub parent: ResourceId,
}

/// Why a follow ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowEnd {
    /// The sink asked to stop.
    Stopped,
    /// The tree closed.
    Closed,
    /// The connection went away.
    Disconnected,
}

/// What can go wrong with a tree.
#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    /// The server does not serve the kind named, or no document kind when
    /// `None`.
    #[error("{}", unsupported(*.0))]
    Unsupported(Option<Kind>),
    /// The named resource is not an accessibility tree.
    #[error("{resource} is not an accessibility tree")]
    WrongKind {
        /// The resource named.
        resource: String,
    },
    /// The parent of a spawn does not exist.
    #[error("parent {parent} not found")]
    ParentNotFound {
        /// The parent named.
        parent: String,
    },
    /// The parent of a spawn is not a Terminal.
    #[error("parent {parent} is not a Terminal")]
    ParentKindMismatch {
        /// The parent named.
        parent: String,
    },
    /// This connection may not publish.
    #[error("not the producer: {0}")]
    NotProducer(String),
    /// Nothing produces the document now, so input has nowhere to go.
    #[error("no producer: {0}")]
    NoProducer(String),
    /// The document is not JSON.
    #[error("document refused: {0}")]
    Invalid(String),
    /// The server refused for another reason.
    #[error("{0}")]
    Refused(String),
    /// The connection failed.
    #[error(transparent)]
    Protocol(#[from] AttachError),
}

fn unsupported(kind: Option<Kind>) -> String {
    kind.map_or_else(
        || "this server does not serve documents (needs ACCESSIBILITY_TREE, PIXEL_SURFACE or TYPED_DOCUMENT)".to_owned(),
        |kind| {
            format!(
                "this server does not serve {}s (needs {})",
                kind.label(),
                kind.feature().wire_name()
            )
        },
    )
}

/// Whether the server advertises `kind`.
///
/// # Errors
///
/// [`TreeError::Unsupported`] when it does not.
pub fn require_kind(conn: &Connection, kind: Kind) -> Result<(), TreeError> {
    if conn.server_features_ext().contains(kind.feature()) {
        Ok(())
    } else {
        Err(TreeError::Unsupported(Some(kind)))
    }
}

/// Whether the server advertises any document kind; a call on an existing
/// resource does not know the kind, and the server refuses a wrong one.
///
/// # Errors
///
/// [`TreeError::Unsupported`] when it serves none.
pub fn require_support(conn: &Connection) -> Result<(), TreeError> {
    if [Kind::Tree, Kind::Surface, Kind::Document]
        .iter()
        .any(|kind| require_kind(conn, *kind).is_ok())
    {
        Ok(())
    } else {
        Err(TreeError::Unsupported(None))
    }
}

fn id(resource: &ResourceId) -> String {
    crate::selector::format_terminal_id(resource)
}

fn map_refusal(resource: &ResourceId, code: ErrorCode, message: String) -> TreeError {
    match code {
        ErrorCode::WrongResourceKind => TreeError::WrongKind {
            resource: id(resource),
        },
        ErrorCode::NotProducer => TreeError::NotProducer(message),
        ErrorCode::RecordInvalid => TreeError::Invalid(message),
        ErrorCode::TerminalNotFound => {
            TreeError::Refused(format!("{} not found: {message}", id(resource)))
        }
        _ => TreeError::Refused(format!("{code:?}: {message}")),
    }
}

/// Open a tree of `target` under `parent`; the caller becomes its producer.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn open(
    conn: &mut Connection,
    parent: &ResourceId,
    target: &str,
) -> Result<Opened, TreeError> {
    open_kind(conn, Kind::Tree, parent, target, None).await
}

/// Open a document of `kind` observing `target` under `parent`; the caller
/// becomes its producer.
///
/// `control` declares what input to it controls
/// (`docs/resource-kinds.md#authority`); `None` takes input from the owner
/// socket only.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn open_kind(
    conn: &mut Connection,
    kind: Kind,
    parent: &ResourceId,
    target: &str,
    control: Option<ControlClass>,
) -> Result<Opened, TreeError> {
    let spawn = kind.spawn(parent.clone(), target).with_control(control);
    open_spawn(conn, kind, parent, target, spawn).await
}

/// Open a document of `content_type` (a MIME type, e.g. `text/markdown`)
/// under `parent`, optionally named; the caller becomes its producer.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn open_document(
    conn: &mut Connection,
    parent: &ResourceId,
    content_type: &str,
    name: Option<&str>,
) -> Result<Opened, TreeError> {
    let spawn = SpawnResource::document(parent.clone(), content_type, name.map(str::to_owned));
    open_spawn(conn, Kind::Document, parent, content_type, spawn).await
}

async fn open_spawn(
    conn: &mut Connection,
    kind: Kind,
    parent: &ResourceId,
    target: &str,
    spawn: SpawnResource,
) -> Result<Opened, TreeError> {
    require_kind(conn, kind)?;
    validate_target(kind, target)?;
    let frame = FrameKind::SpawnResource {
        request_id: REQUEST_SPAWN,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: Some(Box::new(spawn)),
    };
    let (answer, interleaved) = conn.request_spawn(&frame).await?.into_parts();
    crate::state::report_degradation(&interleaved);
    let result = match answer {
        Answer::Ok(result) => result,
        Answer::Err(refusal) => return Err(TreeError::Refused(refusal.to_string())),
    };
    match result {
        SpawnResult::Ok(resource) => Ok(Opened {
            resource,
            parent: parent.clone(),
        }),
        SpawnResult::Err(SpawnError::ParentNotFound) => {
            Err(TreeError::ParentNotFound { parent: id(parent) })
        }
        SpawnResult::Err(SpawnError::ParentKindMismatch) => {
            Err(TreeError::ParentKindMismatch { parent: id(parent) })
        }
        SpawnResult::Err(SpawnError::UnsupportedKind) => Err(TreeError::Unsupported(Some(kind))),
        SpawnResult::Err(other) => Err(TreeError::Refused(format!("{other:?}"))),
        other => Err(AttachError::Protocol(format!(
            "SPAWN_RESOURCE answered with an unrecognised spawn result: {other:?}"
        ))
        .into()),
    }
}

fn validate_target(kind: Kind, target: &str) -> Result<(), TreeError> {
    if kind == Kind::Document {
        return phux_protocol::wire::frame::typed_document::validate_content_type(target)
            .map_err(|error| TreeError::Invalid(format!("{target:?}: {error}")));
    }
    let max = phux_protocol::wire::frame::MAX_RESOURCE_PROVIDER_BYTES;
    if target.is_empty() || target.len() > max {
        return Err(TreeError::Invalid(format!(
            "target must contain 1–{max} UTF-8 bytes"
        )));
    }
    Ok(())
}

/// Replace the tree's document.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn publish(
    conn: &mut Connection,
    tree: &ResourceId,
    bytes: Vec<u8>,
) -> Result<Published, TreeError> {
    publish_collecting(conn, tree, bytes, &mut Vec::new()).await
}

/// [`publish`], keeping every input intent the server relayed while it
/// waited for the reply; a producer that takes input publishes this way.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn publish_collecting(
    conn: &mut Connection,
    tree: &ResourceId,
    bytes: Vec<u8>,
    intents: &mut Vec<Intent>,
) -> Result<Published, TreeError> {
    require_support(conn)?;
    let (result, interleaved) = conn
        .request(
            REQUEST_PUBLISH,
            Command::PublishResourceState {
                terminal_id: tree.clone(),
                bytes,
            },
        )
        .await?
        .into_parts();
    intents.extend(interleaved.iter().filter_map(intent));
    match result {
        CommandResult::OkWith(CommandValue::Json(text)) => serde_json::from_str(&text)
            .map_err(|error| AttachError::Protocol(format!("publish receipt: {error}")).into()),
        CommandResult::Error { code, message } => Err(map_refusal(tree, code, message)),
        other => Err(AttachError::Protocol(format!(
            "PUBLISH_RESOURCE_STATE answered with an unrecognised result: {other:?}"
        ))
        .into()),
    }
}

/// The tree's newest document.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn snapshot(conn: &mut Connection, tree: &ResourceId) -> Result<Document, TreeError> {
    let mut newest = None;
    follow_until(conn, tree, false, |document| {
        newest = Some(document);
        false
    })
    .await?;
    newest.ok_or_else(|| {
        AttachError::Protocol("the tree's bootstrap ended without READY".to_owned()).into()
    })
}

/// Every document from the newest on, until `sink` returns `false`, the
/// tree closes, or the connection ends.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn follow(
    conn: &mut Connection,
    tree: &ResourceId,
    sink: impl FnMut(Document) -> bool,
) -> Result<FollowEnd, TreeError> {
    follow_until(conn, tree, true, sink).await
}

async fn follow_until(
    conn: &mut Connection,
    tree: &ResourceId,
    live: bool,
    mut sink: impl FnMut(Document) -> bool,
) -> Result<FollowEnd, TreeError> {
    require_support(conn)?;
    let (result, primed) = conn
        .request(
            REQUEST_ATTACH,
            Command::attach_terminal(tree.clone(), conn.observer_role_policy()),
        )
        .await?
        .into_parts();
    if let CommandResult::Error { code, message } = result {
        return Err(map_refusal(tree, code, message));
    }
    conn.bind_terminal(tree).await?;
    if live {
        conn.send(&FrameKind::SubscribeEvents {
            terminal: Some(tree.clone()),
            after_seq: None,
        })
        .await?;
    }
    let mut reader = Reader::new(tree.clone());
    let mut pending = primed.into_iter();
    let end = loop {
        let frame = match pending.next() {
            Some(frame) => Ok(frame),
            None => conn.recv().await,
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(AttachError::Disconnected) => break FollowEnd::Disconnected,
            Err(err) => return Err(err.into()),
        };
        match reader.absorb(frame)? {
            Absorbed::Document(document) => {
                if !sink(document) {
                    break FollowEnd::Stopped;
                }
            }
            Absorbed::Closed => break FollowEnd::Closed,
            Absorbed::Nothing => {}
        }
    };
    let _ = conn
        .send(&FrameKind::Command {
            request_id: REQUEST_DETACH,
            command: Command::DetachResource {
                terminal_id: tree.clone(),
            },
        })
        .await;
    conn.unbind_terminal(tree);
    Ok(end)
}

enum Absorbed {
    Document(Document),
    Closed,
    Nothing,
}

/// Folds a tree's frames into documents: a bootstrap's chunks make one, and
/// every live `RESOURCE_OUTPUT` is one. A tombstone starts a replacement
/// generation; its bootstrap carries the newest document.
struct Reader {
    tree: ResourceId,
    base_seq: u64,
    buffer: Vec<u8>,
    /// Whether the bootstrap is typed, and the content type it named.
    typed: bool,
    content_type: Option<String>,
}

impl Reader {
    const fn new(tree: ResourceId) -> Self {
        Self {
            tree,
            base_seq: 0,
            buffer: Vec::new(),
            typed: false,
            content_type: None,
        }
    }

    /// The bootstrap's document, split from its content type when typed.
    fn ready(&mut self) -> Result<Document, TreeError> {
        let buffer = std::mem::take(&mut self.buffer);
        if !self.typed {
            return Ok(self.document(self.base_seq, buffer));
        }
        let (content_type, bytes) =
            phux_protocol::wire::frame::typed_document::split_bootstrap(&buffer).ok_or_else(
                || AttachError::Protocol("a typed bootstrap without a content type".to_owned()),
            )?;
        self.content_type = Some(content_type.to_owned());
        Ok(self.document(self.base_seq, bytes.to_vec()))
    }

    fn document(&self, seq: u64, bytes: Vec<u8>) -> Document {
        Document {
            seq,
            bytes,
            content_type: self.content_type.clone(),
        }
    }

    fn absorb(&mut self, frame: FrameKind) -> Result<Absorbed, TreeError> {
        Ok(match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                base_seq,
                profile,
                ..
            } if terminal_id == self.tree => {
                self.typed = profile == BootstrapStreamProfile::DocumentTypedV1;
                self.base_seq = base_seq;
                self.buffer.clear();
                Absorbed::Nothing
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } if terminal_id == self.tree => {
                self.buffer.extend_from_slice(&payload);
                Absorbed::Nothing
            }
            FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == self.tree => {
                Absorbed::Document(self.ready()?)
            }
            FrameKind::ResourceOutput {
                terminal_id,
                seq,
                bytes,
                ..
            } if terminal_id == self.tree => Absorbed::Document(self.document(seq, bytes.to_vec())),
            FrameKind::ResourceClosed {
                terminal_id: terminal,
                ..
            }
            | FrameKind::Event {
                terminal: Some(terminal),
                event: AgentEvent::ResourceClosed { .. },
                ..
            } if terminal == self.tree => Absorbed::Closed,
            FrameKind::Error {
                request_id: Some(_),
                message,
                ..
            } => return Err(TreeError::Refused(message)),
            _ => Absorbed::Nothing,
        })
    }
}

#[cfg(test)]
mod tests;
