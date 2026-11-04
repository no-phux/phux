//! The `AccessibilityTree` resource over a [`Connection`].
//!
//! Open one under the Terminal its producer runs in, publish whole
//! documents to it, read its newest document, or follow every document as
//! it arrives (`docs/resource-kinds.md`).

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind, SpawnError,
    SpawnResource, SpawnResult,
};

use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};

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
    /// The document, one JSON value.
    pub bytes: Vec<u8>,
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
    /// The server does not serve accessibility trees.
    #[error("this server does not serve accessibility trees (needs ACCESSIBILITY_TREE)")]
    Unsupported,
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

/// Whether the server advertises the kind.
///
/// # Errors
///
/// [`TreeError::Unsupported`] when it does not.
pub fn require_support(conn: &Connection) -> Result<(), TreeError> {
    if conn
        .server_features_ext()
        .contains(ServerFeatureExt::AccessibilityTree)
    {
        Ok(())
    } else {
        Err(TreeError::Unsupported)
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
    require_support(conn)?;
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
        resource: Some(Box::new(SpawnResource::accessibility_tree(
            parent.clone(),
            target,
        ))),
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
        SpawnResult::Err(SpawnError::UnsupportedKind) => Err(TreeError::Unsupported),
        SpawnResult::Err(other) => Err(TreeError::Refused(format!("{other:?}"))),
        other => Err(AttachError::Protocol(format!(
            "SPAWN_RESOURCE answered with an unrecognised spawn result: {other:?}"
        ))
        .into()),
    }
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
    require_support(conn)?;
    let (result, _) = conn
        .request(
            REQUEST_PUBLISH,
            Command::PublishResourceState {
                terminal_id: tree.clone(),
                bytes,
            },
        )
        .await?
        .into_parts();
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
}

impl Reader {
    const fn new(tree: ResourceId) -> Self {
        Self {
            tree,
            base_seq: 0,
            buffer: Vec::new(),
        }
    }

    fn absorb(&mut self, frame: FrameKind) -> Result<Absorbed, TreeError> {
        Ok(match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                base_seq,
                ..
            } if terminal_id == self.tree => {
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
                Absorbed::Document(Document {
                    seq: self.base_seq,
                    bytes: std::mem::take(&mut self.buffer),
                })
            }
            FrameKind::ResourceOutput {
                terminal_id,
                seq,
                bytes,
                ..
            } if terminal_id == self.tree => Absorbed::Document(Document {
                seq,
                bytes: bytes.to_vec(),
            }),
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
mod tests {
    use super::*;
    use phux_protocol::caps::BootstrapStreamProfile;
    use phux_protocol::ids::{BootstrapId, StreamId};

    #[test]
    fn a_bootstrap_folds_into_one_document_and_live_frames_into_one_each() {
        let tree = ResourceId::local(4);
        let stream_id = StreamId::new(1).unwrap();
        let bootstrap_id = BootstrapId::new(1).unwrap();
        let mut reader = Reader::new(tree.clone());
        let begin = FrameKind::BootstrapBegin {
            terminal_id: tree.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::AccessibilityTreeJsonV1,
            cols: 0,
            rows: 0,
            base_seq: 7,
        };
        assert!(matches!(reader.absorb(begin).unwrap(), Absorbed::Nothing));
        for (chunk_seq, payload) in [(0, &b"{\"a\":"[..]), (1, &b"1}"[..])] {
            let chunk = FrameKind::BootstrapChunk {
                terminal_id: tree.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: bytes::Bytes::copy_from_slice(payload),
            };
            assert!(matches!(reader.absorb(chunk).unwrap(), Absorbed::Nothing));
        }
        let ready = FrameKind::BootstrapReady {
            terminal_id: tree.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        };
        let Absorbed::Document(document) = reader.absorb(ready).unwrap() else {
            panic!("READY yields the document");
        };
        assert_eq!(
            document,
            Document {
                seq: 7,
                bytes: b"{\"a\":1}".to_vec()
            }
        );
        let live = FrameKind::ResourceOutput {
            terminal_id: tree,
            stream_id,
            bootstrap_id,
            seq: 8,
            bytes: bytes::Bytes::from_static(b"{\"a\":2}"),
        };
        let Absorbed::Document(document) = reader.absorb(live).unwrap() else {
            panic!("live output yields a document");
        };
        assert_eq!(document.seq, 8);
    }
}
