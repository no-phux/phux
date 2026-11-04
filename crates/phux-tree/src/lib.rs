//! The `AccessibilityTree` engine: one document a producer replaces whole,
//! and viewers that always want the newest (`docs/resource-kinds.md`).
//!
//! The engine keeps the latest document and its sequence. Every publish
//! takes the next sequence and goes out on the output broadcast as one
//! [`ResourceOutput::Live`] message; a viewer that falls behind is recut
//! from the latest document by its pump, so intermediate documents are the
//! only thing a slow viewer ever skips.

use std::sync::Arc;

use bytes::Bytes;
use phux_core::ids::ResourceId;
use phux_core::resource::ResourceKind;
use phux_resource::{
    Delivery, LeaseChange, ResourceCore, ResourceCoreChannels, ResourceFacet, ResourceHandle,
    ResourceOps, ResourceOutput, WrongResourceKind,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Depth of the engine's mailboxes.
pub const TREE_MAILBOX: usize = 64;

/// A producer's whole-document publish (`PUBLISH_RESOURCE_STATE`).
#[derive(Debug)]
pub struct PublishRequest {
    /// One complete JSON document.
    pub bytes: Bytes,
    /// Where the verdict goes.
    pub reply: oneshot::Sender<Result<Published, PublishRejection>>,
}

/// A publish the engine took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Published {
    /// The sequence this document carries.
    pub seq: u64,
    /// When the engine took it, Unix milliseconds.
    pub ts_ms: u64,
}

/// Why a publish was refused; nothing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishRejection {
    /// The bytes are not one JSON document.
    Invalid(String),
    /// The sequence space is exhausted.
    Overflow,
}

/// A viewer's request for the newest document.
#[derive(Debug)]
pub struct SnapshotRequest {
    /// Where the snapshot goes.
    pub reply: oneshot::Sender<TreeSnapshot>,
}

/// The newest document at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeSnapshot {
    /// The sequence of `document`; `0` before the first publish.
    pub base_seq: u64,
    /// The newest document, or `None` before the first publish.
    pub document: Option<Bytes>,
}

/// The `AccessibilityTree` facet of a [`ResourceHandle`].
#[derive(Debug)]
pub struct TreeHandle {
    /// What the producer observes, as it named it.
    pub target: Arc<str>,
    /// Producer publishes.
    pub publish: mpsc::Sender<PublishRequest>,
    /// Viewer snapshot cuts.
    pub snapshot: mpsc::Sender<SnapshotRequest>,
}

impl ResourceFacet for TreeHandle {
    const KIND: ResourceKind = ResourceKind::AccessibilityTree;
}

/// A tree has no grid to size, no process to hand off and no input lease.
impl ResourceOps for TreeHandle {
    fn resize(&self, _size: (u16, u16), _cell_px: Option<(u16, u16)>) -> bool {
        false
    }

    fn upgrade_sender(&self) -> Option<mpsc::Sender<phux_resource::handoff::UpgradeHandleRequest>> {
        None
    }

    fn lease_changed(&self, _change: LeaseChange) -> Delivery {
        Box::pin(std::future::ready(()))
    }

    fn try_lease_changed(&self, _change: LeaseChange) {}

    fn retire(&self) -> Delivery {
        Box::pin(std::future::ready(()))
    }

    /// A tree is a document, not a record stream.
    fn is_live_stream(&self) -> bool {
        false
    }
}

/// Reach the tree facet of a handle.
pub trait TreeResource {
    /// The `AccessibilityTree` facet.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not an accessibility tree.
    fn tree(&self) -> Result<&TreeHandle, WrongResourceKind>;
}

impl TreeResource for ResourceHandle {
    fn tree(&self) -> Result<&TreeHandle, WrongResourceKind> {
        self.facet::<TreeHandle>()
    }
}

/// A [`TreeActor`] and everything the runtime needs to publish it.
#[derive(Debug)]
pub struct TreeBundle {
    /// The engine future's owner; run it on the `LocalSet`.
    pub actor: TreeActor,
    /// Cross-task handle registered in the resource table.
    pub handle: ResourceHandle,
    /// Fires when the engine's run loop ends.
    pub exit_notify: oneshot::Receiver<phux_core::process::ExitOutcome>,
}

/// The producer-fed accessibility-tree engine.
#[derive(Debug)]
pub struct TreeActor {
    core: ResourceCore,
    latest: Option<Bytes>,
    publish_rx: mpsc::Receiver<PublishRequest>,
    snapshot_rx: mpsc::Receiver<SnapshotRequest>,
}

impl TreeActor {
    /// Build a tree of `target` bound to `parent`, whose run loop watches
    /// `token`.
    #[must_use]
    pub fn build(parent: ResourceId, target: &str, token: CancellationToken) -> TreeBundle {
        let (core, channels) = ResourceCore::new(
            ResourceKind::AccessibilityTree,
            Some(parent),
            token,
            phux_resource::output_broadcast_capacity(),
        );
        let ResourceCoreChannels {
            output,
            exit_notify,
        } = channels;
        let (publish_tx, publish_rx) = mpsc::channel(TREE_MAILBOX);
        let (snapshot_tx, snapshot_rx) = mpsc::channel(TREE_MAILBOX);
        let facet = TreeHandle {
            target: Arc::from(target),
            publish: publish_tx,
            snapshot: snapshot_tx,
        };
        let handle = ResourceHandle::new(Some(parent), output, facet);
        TreeBundle {
            actor: Self {
                core,
                latest: None,
                publish_rx,
                snapshot_rx,
            },
            handle,
            exit_notify,
        }
    }

    /// Serve publishes and snapshots until cancelled or every handle is gone.
    pub async fn run(mut self) {
        let token = self.core.token.clone();
        loop {
            tokio::select! {
                // Snapshots first: a cut taken in the same wake-up as a
                // publish sees the document the publish's sequence names.
                biased;
                () = token.cancelled() => break,
                request = self.snapshot_rx.recv() => match request {
                    Some(request) => self.handle_snapshot(request),
                    None => break,
                },
                request = self.publish_rx.recv() => match request {
                    Some(request) => self.handle_publish(request),
                    None => break,
                },
            }
        }
        self.core
            .notify_exit(phux_core::process::ExitOutcome::UNKNOWN);
    }

    fn handle_snapshot(&self, request: SnapshotRequest) {
        let _ = request.reply.send(TreeSnapshot {
            base_seq: self.core.seq(),
            document: self.latest.clone(),
        });
    }

    /// Validate, sequence, retain and broadcast one document, all or nothing.
    fn handle_publish(&mut self, request: PublishRequest) {
        let PublishRequest { bytes, reply } = request;
        if let Err(error) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let _ = reply.send(Err(PublishRejection::Invalid(error.to_string())));
            return;
        }
        let Some(seq) = self.core.next_seq() else {
            let _ = reply.send(Err(PublishRejection::Overflow));
            return;
        };
        let ts_ms = phux_resource::now_unix_ms();
        self.latest = Some(bytes.clone());
        let _ = self.core.output_tx.send(ResourceOutput::Live {
            seq,
            bytes,
            at: std::time::Instant::now(),
        });
        let _ = reply.send(Ok(Published { seq, ts_ms }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn publish(handle: &ResourceHandle, bytes: &[u8]) -> Result<Published, PublishRejection> {
        let (reply, rx) = oneshot::channel();
        handle
            .tree()
            .unwrap()
            .publish
            .send(PublishRequest {
                bytes: Bytes::copy_from_slice(bytes),
                reply,
            })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    async fn snapshot(handle: &ResourceHandle) -> TreeSnapshot {
        let (reply, rx) = oneshot::channel();
        handle
            .tree()
            .unwrap()
            .snapshot
            .send(SnapshotRequest { reply })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    #[test]
    fn the_newest_document_wins_and_a_broken_one_changes_nothing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let token = CancellationToken::new();
            let bundle = TreeActor::build(ResourceId::default(), "Safari", token.clone());
            let handle = bundle.handle;
            let mut live = handle.output.subscribe();
            tokio::task::spawn_local(bundle.actor.run());

            assert_eq!(
                snapshot(&handle).await,
                TreeSnapshot {
                    base_seq: 0,
                    document: None
                }
            );
            assert_eq!(
                publish(&handle, b"{\"role\":\"window\"}")
                    .await
                    .map(|p| p.seq),
                Ok(1)
            );
            assert!(matches!(
                publish(&handle, b"{not json").await,
                Err(PublishRejection::Invalid(_))
            ));
            assert_eq!(publish(&handle, b"[1,2]").await.map(|p| p.seq), Ok(2));
            let cut = snapshot(&handle).await;
            assert_eq!(cut.base_seq, 2);
            assert_eq!(cut.document.as_deref(), Some(&b"[1,2]"[..]));
            let ResourceOutput::Live { seq, .. } = live.try_recv().unwrap() else {
                panic!("live output");
            };
            assert_eq!(seq, 1);
            token.cancel();
        });
    }
}
