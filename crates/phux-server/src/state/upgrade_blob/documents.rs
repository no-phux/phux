//! Carrying document resources (trees, surfaces, typed documents) across an
//! upgrade: same wire id, same record, the newest document, and no producer
//! until one publishes to it again (`runtime::apps::document`).

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

use base64::Engine as _;
use bytes::Bytes;
use phux_protocol::wire::frame::ControlClass;
use phux_tree::{
    DocumentActor, DocumentKind, DocumentResource as _, SnapshotRequest, SurfaceDocument,
    TreeDocument, TypedDocument,
};
use tokio::sync::mpsc;

use crate::upgrade::blob::DocumentBlob;

/// Each carried document's newest document at the cut; `None` before its
/// first publish. A document without an entry is not carried.
pub(crate) type DocumentCuts = HashMap<ResourceId, Option<Bytes>>;

/// The snapshot channel of a document resource, whatever its kind.
fn snapshot_sender(handle: &ResourceHandle) -> Option<mpsc::Sender<SnapshotRequest>> {
    handle
        .document::<TreeDocument>()
        .map(|d| d.snapshot.clone())
        .or_else(|_| {
            handle
                .document::<SurfaceDocument>()
                .map(|d| d.snapshot.clone())
        })
        .or_else(|_| {
            handle
                .document::<TypedDocument>()
                .map(|d| d.snapshot.clone())
        })
        .ok()
}

/// A rebuilt document engine: its handle, run loop and exit receiver.
type BuiltDocument = (
    ResourceHandle,
    std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    oneshot::Receiver<phux_core::process::ExitOutcome>,
);

/// Build a `K` engine on `target`, seeded with `document`.
fn build<K: DocumentKind>(
    parent: ResourceId,
    target: &str,
    document: Option<Bytes>,
    token: CancellationToken,
) -> BuiltDocument {
    let bundle = DocumentActor::<K>::build(parent, target, token);
    (
        bundle.handle,
        Box::pin(bundle.actor.seeded(document).run()),
        bundle.exit_notify,
    )
}

impl ServerState {
    /// Every live document resource with a wire id and a record, with the
    /// channel its snapshot is cut through.
    pub(crate) fn upgrade_documents(&self) -> Vec<(ResourceId, mpsc::Sender<SnapshotRequest>)> {
        self.resource_ids()
            .into_iter()
            .filter(|&id| {
                self.resource_wire(id).is_some() && self.sessions.registry.resource(id).is_some()
            })
            .filter_map(|id| Some((id, snapshot_sender(self.resource_handle(id)?)?)))
            .collect()
    }

    /// One [`DocumentBlob`] per cut whose document is still registered under
    /// a pane that crosses live, in wire-id order.
    pub(in crate::state) fn document_blobs(
        &self,
        cuts: &DocumentCuts,
        live_panes: &HashSet<u32>,
    ) -> Vec<DocumentBlob> {
        let mut blobs: Vec<DocumentBlob> = cuts
            .iter()
            .filter_map(|(&id, document)| {
                let desc = self.sessions.registry.resource(id)?;
                let parent_wire_id = self.resource_wire(desc.parent?)?;
                live_panes.contains(&parent_wire_id).then_some(())?;
                Some(DocumentBlob {
                    wire_id: self.resource_wire(id)?,
                    parent_wire_id,
                    key: desc.key,
                    control: self.document_control(id).map(ControlClass::to_u8),
                    document: document
                        .as_ref()
                        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
                })
            })
            .collect();
        blobs.sort_by_key(|blob| blob.wire_id);
        blobs
    }

    /// Rebuild every carried document under its recorded wire id, seeded
    /// with its newest document. One whose parent pane or record did not
    /// cross is dropped; losing one document must not cost the whole tree.
    pub(in crate::state) fn rebuild_documents<W>(
        &mut self,
        blob: &StateBlob,
        panes: &mut RebuiltPanes<W>,
    ) -> Result<(), RebuildError> {
        for carried in &blob.documents {
            let parent = panes.core_ids.get(&carried.parent_wire_id).copied();
            let core = self.sessions.registry.resource_id(&carried.key);
            let launch = core
                .and_then(|core| self.sessions.registry.record(core))
                .and_then(|record| record.launch.clone());
            let (Some(parent), Some(core), Some(launch)) = (parent, core, launch) else {
                tracing::warn!(
                    document = carried.wire_id,
                    "resume: document's parent pane or record did not cross; dropped"
                );
                continue;
            };
            let document = carried.document.as_deref().and_then(|text| {
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .ok()
                    .map(Bytes::from)
            });
            let token = CancellationToken::new();
            let (handle, engine, exit_notify) = match &launch {
                LaunchSpec::Tree(t) => {
                    build::<TreeDocument>(parent, &t.target, document, token.clone())
                }
                LaunchSpec::Surface(s) => {
                    build::<SurfaceDocument>(parent, &s.target, document, token.clone())
                }
                LaunchSpec::Document(d) => {
                    build::<TypedDocument>(parent, &d.content_type, document, token.clone())
                }
                _ => continue,
            };
            // Pre-bind so the registration's intern returns the blob's id.
            self.idspace
                .bind_resource(core, WireResourceId::local(carried.wire_id));
            self.spawn_resource_actor(core, handle, token, engine)?;
            panes.exit_watchers.push((core, exit_notify));
        }
        Ok(())
    }

    /// Reinstate each carried document's control class, after the rebuilt
    /// tree is committed.
    pub(in crate::state) fn restore_document_controls(&mut self, blob: &StateBlob) {
        for carried in &blob.documents {
            if let Some(core) = self.resource_from_wire(&WireResourceId::local(carried.wire_id)) {
                self.set_document_control(core, carried.control.and_then(ControlClass::from_u8));
            }
        }
    }
}
