//! Carrying document resources across a graceful upgrade (ADR-0032): same
//! wire id, same record, the newest document and control class, and no
//! producer until one publishes to it again. Each document app carries its
//! own kind; the three share the blob's `documents`.

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::os::fd::RawFd;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::ControlClass;
use phux_tree::{DocumentActor, DocumentResource as _, SnapshotRequest};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::DocumentApi;
use crate::runtime::apps::{KindResume, UpgradeCut};
use crate::runtime::connections::pane_events::PaneEvents;
use crate::runtime::upgrade::UpgradeError;
use crate::state::upgrade_blob::{KindRebuild, RebuildError, ResumeCx, live_pane_wires};
use crate::state::{ServerState, SharedState};
use crate::upgrade::blob::{DocumentBlob, StateBlob};

/// Each carried `A` document's newest document at the cut; `None` before
/// its first publish. A document without an entry is not carried.
pub(crate) struct DocumentCut<A> {
    cuts: HashMap<ResourceId, Option<Bytes>>,
    kind: PhantomData<A>,
}

impl<A> DocumentCut<A> {
    pub(crate) fn begin() -> Box<dyn UpgradeCut>
    where
        A: DocumentApi,
    {
        Box::new(Self {
            cuts: HashMap::new(),
            kind: PhantomData,
        })
    }
}

impl<A: DocumentApi> UpgradeCut for DocumentCut<A> {
    fn cut<'a>(
        &'a mut self,
        state: &SharedState,
        deadline: Duration,
    ) -> LocalBoxFuture<'a, Result<(), UpgradeError>> {
        let documents = state.with(snapshot_senders::<A>);
        async move {
            self.cuts = collect_document_cuts(documents, deadline).await;
            Ok(())
        }
        .boxed_local()
    }

    fn carry(&self, s: &ServerState, blob: &mut StateBlob) -> Result<(), UpgradeError> {
        let live_panes = live_pane_wires(blob);
        blob.documents
            .extend(document_blobs(s, &self.cuts, &live_panes));
        blob.documents.sort_by_key(|document| document.wire_id);
        Ok(())
    }

    fn inherited_fds(&self, _blob: &StateBlob) -> Vec<RawFd> {
        Vec::new()
    }

    fn lift(&mut self) -> LocalBoxFuture<'_, ()> {
        // A cut only reads the newest document; nothing was held.
        Box::pin(std::future::ready(()))
    }
}

/// Every live `A` document with a wire id and a record, with the channel
/// its snapshot is cut through.
fn snapshot_senders<A: DocumentApi>(
    s: &ServerState,
) -> Vec<(ResourceId, mpsc::Sender<SnapshotRequest>)> {
    s.resource_ids()
        .into_iter()
        .filter(|&id| s.resource_wire(id).is_some() && s.registry().resource(id).is_some())
        .filter_map(|id| {
            let document = s.resource_handle(id)?.document::<A>().ok()?;
            Some((id, document.snapshot.clone()))
        })
        .collect()
}

/// Cut each document's newest document within `deadline`. A document that
/// does not answer in time is not carried: its producer opens a new one, as
/// before documents crossed, and the upgrade goes on.
pub(crate) async fn collect_document_cuts(
    documents: Vec<(ResourceId, mpsc::Sender<SnapshotRequest>)>,
    deadline: Duration,
) -> HashMap<ResourceId, Option<Bytes>> {
    let mut pending = documents
        .into_iter()
        .map(|(id, snapshot)| async move {
            let (reply, rx) = oneshot::channel();
            snapshot.send(SnapshotRequest { reply }).await.ok()?;
            Some((id, rx.await.ok()?.document))
        })
        .collect::<FuturesUnordered<_>>();
    let mut cuts = HashMap::new();
    let _ = tokio::time::timeout(deadline, async {
        while let Some(cut) = pending.next().await {
            cuts.extend(cut);
        }
    })
    .await;
    cuts
}

/// One [`DocumentBlob`] per cut whose document is still registered under a
/// pane that crosses live.
fn document_blobs(
    s: &ServerState,
    cuts: &HashMap<ResourceId, Option<Bytes>>,
    live_panes: &HashSet<u32>,
) -> Vec<DocumentBlob> {
    cuts.iter()
        .filter_map(|(&id, document)| {
            let desc = s.registry().resource(id)?;
            let parent_wire_id = s.resource_wire(desc.parent?)?;
            live_panes.contains(&parent_wire_id).then_some(())?;
            Some(DocumentBlob {
                wire_id: s.resource_wire(id)?,
                parent_wire_id,
                key: desc.key,
                control: s.document_control(id).map(ControlClass::to_u8),
                document: document
                    .as_ref()
                    .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
            })
        })
        .collect()
}

/// Rebuilds every carried `A` document, then its control class.
pub(crate) struct DocumentResume<A> {
    /// Each rebuilt document's wire id and carried control byte.
    controls: Vec<(u32, Option<u8>)>,
    kind: PhantomData<A>,
}

impl<A: DocumentApi> DocumentResume<A> {
    pub(crate) fn begin() -> Box<dyn KindResume> {
        Box::new(Self {
            controls: Vec::new(),
            kind: PhantomData,
        })
    }
}

impl<A: DocumentApi> KindRebuild for DocumentResume<A> {
    /// Rebuild every carried document of this kind under its recorded wire
    /// id, seeded with its newest document. Its record's launch names the
    /// kind; another kind's entry is left to its own app.
    fn rebuild(&mut self, cx: &mut ResumeCx<'_>) -> Result<(), RebuildError> {
        let blob = cx.blob;
        for carried in &blob.documents {
            let Some(core) = cx.record(carried.wire_id) else {
                continue;
            };
            let target = cx
                .state
                .registry()
                .record(core)
                .and_then(|record| record.launch.as_ref())
                .and_then(A::launch_target)
                .map(str::to_owned);
            let (Some(target), Some(parent)) = (target, cx.parent(carried.parent_wire_id)) else {
                continue;
            };
            let document = carried.document.as_deref().and_then(|text| {
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .ok()
                    .map(Bytes::from)
            });
            let token = CancellationToken::new();
            let bundle = DocumentActor::<A>::build(parent, &target, token.clone());
            cx.spawn(
                core,
                carried.wire_id,
                bundle.handle,
                token,
                bundle.actor.seeded(document).run(),
                Some(bundle.exit_notify),
            )?;
            self.controls.push((carried.wire_id, carried.control));
        }
        Ok(())
    }

    /// Reinstate each rebuilt document's control class.
    fn restore(&mut self, state: &mut ServerState, _blob: &StateBlob) {
        for (wire, control) in self.controls.drain(..) {
            if let Some(core) = state.resource_from_wire(&WireResourceId::local(wire)) {
                state.set_document_control(core, control.and_then(ControlClass::from_u8));
            }
        }
    }
}

impl<A: DocumentApi> KindResume for DocumentResume<A> {
    fn start(&mut self, _state: &SharedState) -> Vec<(ResourceId, PaneEvents)> {
        Vec::new()
    }
}
