//! The replicated-document engine behind the `AccessibilityTree`,
//! `PixelSurface` and `Document` kinds: one document a producer replaces whole, and viewers
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
pub use phux_core::image::{ImageFormat, image_size};
use phux_core::resource::ResourceKind;
use phux_protocol::wire::frame::typed_document::is_textual;
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
    /// Whether `bytes` is one complete document of a resource opened on
    /// `target` (for a typed document, its content type); `Err` says why not.
    fn validate(target: &str, bytes: &[u8]) -> Result<(), String>;
}

/// An accessibility tree: one JSON value.
#[derive(Debug)]
pub struct TreeDocument;

impl DocumentKind for TreeDocument {
    const KIND: ResourceKind = ResourceKind::AccessibilityTree;

    fn validate(_target: &str, bytes: &[u8]) -> Result<(), String> {
        json(bytes)
    }
}

/// A pixel surface: one PNG or JPEG image, told apart by its signature,
/// whose size is the surface's geometry.
#[derive(Debug)]
pub struct SurfaceDocument;

impl DocumentKind for SurfaceDocument {
    const KIND: ResourceKind = ResourceKind::PixelSurface;

    fn validate(_target: &str, bytes: &[u8]) -> Result<(), String> {
        image_size(bytes).map(|_| ())
    }
}

/// The largest typed document, the `PUBLISH_RESOURCE_STATE` payload bound.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// A document of the content type its resource was opened with. The engine
/// checks only what is cheap: a PNG has its header, JSON parses, text is
/// UTF-8; any other type is opaque bytes.
#[derive(Debug)]
pub struct TypedDocument;

impl DocumentKind for TypedDocument {
    const KIND: ResourceKind = ResourceKind::Document;

    fn validate(content_type: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(format!(
                "the document is {} bytes; the most is {MAX_DOCUMENT_BYTES}",
                bytes.len()
            ));
        }
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if essence == "image/png" {
            match image_size(bytes)? {
                (ImageFormat::Png, ..) => Ok(()),
                _ => Err("an image/png document is a PNG".to_owned()),
            }
        } else if essence == "application/json" {
            json(bytes)
        } else if is_textual(&essence) {
            std::str::from_utf8(bytes)
                .map(|_| ())
                .map_err(|error| format!("a {essence} document is UTF-8: {error}"))
        } else {
            Ok(())
        }
    }
}

fn json(bytes: &[u8]) -> Result<(), String> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .map(|_| ())
        .map_err(|error| format!("the document is not JSON: {error}"))
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
/// The `Document` facet; its `target` is the content type.
pub type TypedHandle = DocumentHandle<TypedDocument>;

impl<K: DocumentKind> ResourceFacet for DocumentHandle<K> {
    const KIND: ResourceKind = K::KIND;
}

/// A document has no grid to size and no input lease.
impl<K: DocumentKind> ResourceOps for DocumentHandle<K> {
    fn resize(&self, _size: (u16, u16), _cell_px: Option<(u16, u16)>) -> bool {
        false
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
    target: Arc<str>,
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
        let target: Arc<str> = Arc::from(target);
        let facet = DocumentHandle::<K> {
            target: Arc::clone(&target),
            publish: publish_tx,
            snapshot: snapshot_tx,
            kind: PhantomData,
        };
        let handle = ResourceHandle::new(Some(parent), output, facet);
        DocumentBundle {
            actor: Self {
                core,
                target,
                latest: None,
                publish_rx,
                snapshot_rx,
                kind: PhantomData,
            },
            handle,
            exit_notify,
        }
    }

    /// Start from `document` at sequence 1, as a document carried across a
    /// server upgrade does; `None` starts empty.
    #[must_use]
    pub fn seeded(mut self, document: Option<Bytes>) -> Self {
        if document.is_some() && self.core.next_seq().is_some() {
            self.latest = document;
        }
        self
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
        if let Err(error) = K::validate(&self.target, &bytes) {
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
    fn a_surface_takes_a_png_or_a_jpeg_with_area() {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(image_size(&png), Ok((ImageFormat::Png, 640, 480)));
        assert!(SurfaceDocument::validate("", &png).is_ok());
        let jpeg = [0xff, 0xd8, 0xff, 0xc2, 0, 11, 8, 0, 3, 0, 5, 1, 1, 0x11, 0];
        assert!(SurfaceDocument::validate("", &jpeg).is_ok());
        assert!(SurfaceDocument::validate("", b"GIF89a\x05\x00\x03\x00").is_err());
        assert!(SurfaceDocument::validate("", b"{\"not\":\"png\"}").is_err());
        let mut flat = png.clone();
        flat[20..24].copy_from_slice(&0u32.to_be_bytes());
        assert!(SurfaceDocument::validate("", &flat).is_err());
    }

    #[test]
    fn a_typed_document_is_checked_by_its_content_type_family() {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1]);
        let check = |content_type: &str, bytes: &[u8]| TypedDocument::validate(content_type, bytes);
        assert!(check("image/png", &png).is_ok());
        assert!(check("image/png", b"not a png").is_err());
        assert!(
            check(
                "image/png",
                &[0xff, 0xd8, 0xff, 0xc0, 0, 11, 8, 0, 1, 0, 1, 1]
            )
            .is_err()
        );
        assert!(check("application/json", b"{\"a\":[1]}").is_ok());
        assert!(check("Application/JSON; charset=utf-8", b"{oops").is_err());
        assert!(check("text/markdown", "# héllo".as_bytes()).is_ok());
        assert!(check("text/csv", b"a,\xff").is_err());
        assert!(check("image/svg+xml", b"\xc3").is_err());
        assert!(check("application/ld+json", b"{}").is_ok());
        assert!(check("image/jpeg", b"\xff\xd8 anything").is_ok());
        assert!(check("application/octet-stream", &[0xff; 16]).is_ok());
        assert!(check("text/plain", &vec![b'a'; MAX_DOCUMENT_BYTES + 1]).is_err());
    }
}
