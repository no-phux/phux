//! The replicated-document engine behind the `AccessibilityTree` and
//! `PixelSurface` kinds: one document a producer replaces whole, and viewers
//! that always want the newest (`docs/resource-kinds.md`).
//!
//! The engine keeps the latest document and its sequence. Every publish
//! takes the next sequence and goes out on the output broadcast as one
//! [`ResourceOutput::Live`] message; a viewer that falls behind is recut
//! from the latest document by its pump, so intermediate documents are the
//! only thing a slow viewer ever skips. A [`DocumentKind`] says which
//! resource kind an engine serves and what a valid document is.

use std::marker::PhantomData;
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

/// What one engine replicates: the kind it serves and the documents it
/// accepts.
pub trait DocumentKind: std::fmt::Debug + Send + Sync + 'static {
    /// The resource kind an engine of this document serves.
    const KIND: ResourceKind;
    /// Whether `bytes` is one complete document; `Err` says why not.
    fn validate(bytes: &[u8]) -> Result<(), String>;
}

/// An accessibility tree: one JSON value.
#[derive(Debug)]
pub struct TreeDocument;

impl DocumentKind for TreeDocument {
    const KIND: ResourceKind = ResourceKind::AccessibilityTree;

    fn validate(bytes: &[u8]) -> Result<(), String> {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .map(|_| ())
            .map_err(|error| format!("the document is not JSON: {error}"))
    }
}

/// A pixel surface: one PNG image, whose size is the surface's geometry.
#[derive(Debug)]
pub struct SurfaceDocument;

impl DocumentKind for SurfaceDocument {
    const KIND: ResourceKind = ResourceKind::PixelSurface;

    fn validate(bytes: &[u8]) -> Result<(), String> {
        png_size(bytes).map(|_| ())
    }
}

/// The width and height of a PNG from its signature and `IHDR` chunk, or
/// why `bytes` is not one.
///
/// # Errors
///
/// A message naming what is missing or zero.
pub fn png_size(bytes: &[u8]) -> Result<(u32, u32), String> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return Err("the document is not a PNG: no signature and IHDR chunk".to_owned());
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (width, height) = (be(16), be(20));
    if width == 0 || height == 0 {
        return Err(format!("the PNG is {width} x {height}; a surface has area"));
    }
    Ok((width, height))
}

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

/// The facet of a document resource's [`ResourceHandle`].
#[derive(Debug)]
pub struct DocumentHandle<K: DocumentKind> {
    /// What the producer observes, as it named it.
    pub target: Arc<str>,
    /// Producer publishes.
    pub publish: mpsc::Sender<PublishRequest>,
    /// Viewer snapshot cuts.
    pub snapshot: mpsc::Sender<SnapshotRequest>,
    kind: PhantomData<K>,
}

/// The `AccessibilityTree` facet.
pub type TreeHandle = DocumentHandle<TreeDocument>;
/// The `PixelSurface` facet.
pub type SurfaceHandle = DocumentHandle<SurfaceDocument>;

impl<K: DocumentKind> ResourceFacet for DocumentHandle<K> {
    const KIND: ResourceKind = K::KIND;
}

/// A document has no grid to size, no process to hand off and no input
/// lease.
impl<K: DocumentKind> ResourceOps for DocumentHandle<K> {
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

/// Reach a document facet of a handle.
pub trait DocumentResource {
    /// The facet of document kind `K`.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not of `K::KIND`.
    fn document<K: DocumentKind>(&self) -> Result<&DocumentHandle<K>, WrongResourceKind>;

    /// The `AccessibilityTree` facet.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not an accessibility tree.
    fn tree(&self) -> Result<&TreeHandle, WrongResourceKind> {
        self.document::<TreeDocument>()
    }

    /// The `PixelSurface` facet.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not a pixel surface.
    fn surface(&self) -> Result<&SurfaceHandle, WrongResourceKind> {
        self.document::<SurfaceDocument>()
    }
}

/// The tree facet, by its old name.
pub use DocumentResource as TreeResource;

impl DocumentResource for ResourceHandle {
    fn document<K: DocumentKind>(&self) -> Result<&DocumentHandle<K>, WrongResourceKind> {
        self.facet::<DocumentHandle<K>>()
    }
}

/// A [`DocumentActor`] and everything the runtime needs to publish it.
#[derive(Debug)]
pub struct DocumentBundle<K: DocumentKind> {
    /// The engine future's owner; run it on the `LocalSet`.
    pub actor: DocumentActor<K>,
    /// Cross-task handle registered in the resource table.
    pub handle: ResourceHandle,
    /// Fires when the engine's run loop ends.
    pub exit_notify: oneshot::Receiver<phux_core::process::ExitOutcome>,
}

/// A tree engine and its handle.
pub type TreeBundle = DocumentBundle<TreeDocument>;

/// The producer-fed replicated-document engine.
#[derive(Debug)]
pub struct DocumentActor<K: DocumentKind> {
    core: ResourceCore,
    latest: Option<Bytes>,
    publish_rx: mpsc::Receiver<PublishRequest>,
    snapshot_rx: mpsc::Receiver<SnapshotRequest>,
    kind: PhantomData<K>,
}

/// The accessibility-tree engine.
pub type TreeActor = DocumentActor<TreeDocument>;
/// The pixel-surface engine.
pub type SurfaceActor = DocumentActor<SurfaceDocument>;

impl<K: DocumentKind> DocumentActor<K> {
    /// Build a document of `target` bound to `parent`, whose run loop
    /// watches `token`.
    #[must_use]
    pub fn build(parent: ResourceId, target: &str, token: CancellationToken) -> DocumentBundle<K> {
        let (core, channels) = ResourceCore::new(
            K::KIND,
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
        let facet = DocumentHandle::<K> {
            target: Arc::from(target),
            publish: publish_tx,
            snapshot: snapshot_tx,
            kind: PhantomData,
        };
        let handle = ResourceHandle::new(Some(parent), output, facet);
        DocumentBundle {
            actor: Self {
                core,
                latest: None,
                publish_rx,
                snapshot_rx,
                kind: PhantomData,
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
        if let Err(error) = K::validate(&bytes) {
            let _ = reply.send(Err(PublishRejection::Invalid(error)));
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

    #[test]
    fn a_surface_takes_only_a_png_with_area() {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_size(&png), Ok((640, 480)));
        assert!(SurfaceDocument::validate(&png).is_ok());
        assert!(SurfaceDocument::validate(b"{\"not\":\"png\"}").is_err());
        let mut flat = png.clone();
        flat[20..24].copy_from_slice(&0u32.to_be_bytes());
        assert!(SurfaceDocument::validate(&flat).is_err());
    }
}
