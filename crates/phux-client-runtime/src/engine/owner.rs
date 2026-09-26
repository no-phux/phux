//! Kernel ownership, queries, projection, and publication.

use super::apply::apply_event;
#[cfg(feature = "engine")]
use super::apply::frame_colors;
#[cfg(feature = "engine")]
use super::predict::Predictor;
use super::{
    Adapter, Command, EffectBuffer, EngineApplyError, EngineEvent, EngineOutcome, HashSet,
    InputBlockReason, InputEligibility, KernelDamageKind, KernelEffect, Lifecycle, Query,
    ResourceId, SessionKernel, mpsc,
};
#[cfg(feature = "engine")]
use super::{
    Arc, CanonicalGeometry, ClosedReplica, DocumentAnchorId, DocumentPoint, DocumentSpace,
    EngineDocumentPoint, EngineDocumentSelection, EngineError, GhosttyAdapter, GhosttyReplica,
    GridBuffer, GridDamage, GridFrame, GridProjector, HashMap, MouseMode, Publication, ReplicaInfo,
    Rgb, Scroll, ScrollViewport, Scrollbar, SearchMatch, SelectionGestureEvent,
    SelectionGestureResult,
};
#[cfg(feature = "engine")]
use libghostty_vt::selection::Selection;
#[cfg(feature = "engine")]
use libghostty_vt::selection::gesture::{
    Behavior, Behaviors, DragEvent, Geometry, Gesture, PressEvent,
};
#[cfg(feature = "engine")]
use libghostty_vt::terminal::{Point, PointCoordinate, PointSpace};

#[cfg(feature = "engine")]
mod render;
#[cfg(feature = "engine")]
mod views;
#[cfg(feature = "engine")]
use views::{Presentation, View};

#[cfg(feature = "engine")]
struct PointerGesture {
    token: u128,
    handle: u64,
    gesture: Gesture<'static>,
}

#[cfg(feature = "engine")]
struct ProjectorSlot {
    token: u128,
    projector: GridProjector,
}

pub(super) struct Owner {
    kernel: SessionKernel<Adapter>,
    effects: EffectBuffer,
    /// Terminals with a published, non-removed projection.
    visible: HashSet<ResourceId>,
    #[cfg(feature = "engine")]
    publication: Arc<Publication>,
    #[cfg(feature = "engine")]
    projectors: HashMap<ResourceId, ProjectorSlot>,
    #[cfg(feature = "engine")]
    closed: HashMap<ResourceId, ClosedReplica<GhosttyAdapter>>,
    #[cfg(feature = "engine")]
    pending_releases: HashSet<ResourceId>,
    #[cfg(feature = "engine")]
    anchors: HashMap<u64, (ResourceId, Option<crate::ViewId>, DocumentAnchorId)>,
    #[cfg(feature = "engine")]
    presentations: HashMap<ResourceId, Presentation>,
    #[cfg(feature = "engine")]
    views: HashMap<crate::ViewId, View>,
    #[cfg(feature = "engine")]
    active_view: Option<crate::ViewId>,
    #[cfg(feature = "engine")]
    document_revisions: HashMap<ResourceId, u64>,
    #[cfg(feature = "engine")]
    next_document_revision: u64,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.retire_publications();
    }
}

impl Owner {
    pub(super) fn new(
        kernel: SessionKernel<Adapter>,
        #[cfg(feature = "engine")] publication: Arc<Publication>,
    ) -> Self {
        Self {
            kernel,
            effects: EffectBuffer::new(),
            visible: HashSet::new(),
            #[cfg(feature = "engine")]
            publication,
            #[cfg(feature = "engine")]
            projectors: HashMap::new(),
            #[cfg(feature = "engine")]
            closed: HashMap::new(),
            #[cfg(feature = "engine")]
            pending_releases: HashSet::new(),
            #[cfg(feature = "engine")]
            anchors: HashMap::new(),
            #[cfg(feature = "engine")]
            presentations: HashMap::new(),
            #[cfg(feature = "engine")]
            views: HashMap::new(),
            #[cfg(feature = "engine")]
            active_view: None,
            #[cfg(feature = "engine")]
            document_revisions: HashMap::new(),
            #[cfg(feature = "engine")]
            next_document_revision: 1,
        }
    }

    pub(super) fn run(mut self, commands: &mpsc::Receiver<Command>) {
        while let Ok(command) = commands.recv() {
            match command {
                Command::Stop(reply) => {
                    self.retire_publications();
                    let _ = reply.send(());
                    break;
                }
                Command::ApplyBatch(events, reply) => {
                    let _ = reply.send(self.apply_batch(events));
                }
                Command::Lifecycle(lifecycle) => self.lifecycle(lifecycle),
                Command::Query(query) => self.query(query),
                #[cfg(feature = "engine")]
                Command::View(command) => self.view_command(command),
            }
        }
    }

    fn retire_publications(&mut self) {
        self.visible.clear();
        #[cfg(feature = "engine")]
        {
            for view in self.views.keys() {
                self.publication.remove_view(*view);
            }
            self.views.clear();
            for id in self.presentations.keys() {
                self.publication.remove(id);
            }
            self.presentations.clear();
        }
    }

    fn lifecycle(&mut self, lifecycle: Lifecycle) {
        match lifecycle {
            Lifecycle::Detach(id, reply) => {
                let detached = self.kernel.detach_terminal(&id);
                if detached {
                    self.forget_projection(&id);
                }
                let _ = reply.send(detached);
            }
            Lifecycle::Reset(reply) => {
                self.kernel.release_active_attach();
                #[cfg(not(feature = "engine"))]
                let visible: Vec<_> = self.visible.iter().cloned().collect();
                #[cfg(feature = "engine")]
                let mut visible: Vec<_> = self.visible.iter().cloned().collect();
                #[cfg(feature = "engine")]
                visible.extend(
                    self.closed
                        .keys()
                        .filter(|id| !self.visible.contains(*id))
                        .cloned(),
                );
                for id in visible {
                    let _ = self.kernel.release_terminal(&id);
                    self.forget_projection(&id);
                }
                let _ = reply.send(());
            }
            #[cfg(feature = "engine")]
            Lifecycle::Retain(id, retain) => {
                self.kernel.set_retain_replica_on_close(&id, retain);
            }
            #[cfg(feature = "engine")]
            Lifecycle::Release(id) => {
                self.kernel.set_retain_replica_on_close(&id, false);
                if self.closed.contains_key(&id) {
                    self.pending_releases.insert(id.clone());
                    self.release_closed(&id);
                    self.projectors.remove(&id);
                    self.publication.remove(&id);
                    self.forget_views(&id);
                    self.invalidate_handles(&id);
                    self.presentations.remove(&id);
                    self.document_revisions.remove(&id);
                }
            }
        }
    }

    fn forget_projection(&mut self, id: &ResourceId) {
        self.visible.remove(id);
        #[cfg(feature = "engine")]
        {
            self.closed.remove(id);
            self.pending_releases.remove(id);
            self.projectors.remove(id);
            self.publication.remove(id);
            self.invalidate_handles(id);
            self.forget_views(id);
            self.presentations.remove(id);
            self.document_revisions.remove(id);
        }
    }

    fn query(&mut self, query: Query) {
        let Some(query) = self.query_common(query) else {
            return;
        };
        #[cfg(feature = "engine")]
        {
            let Some(query) = self.query_presentation(query) else {
                return;
            };
            self.query_document(query);
        }
        #[cfg(not(feature = "engine"))]
        if let Query::TakeOutput(id, reply) = query {
            let bytes = if self.visible.contains(&id) {
                self.kernel
                    .published_engine_mut(&id)
                    .map(|replica| std::mem::take(&mut replica.bytes))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let _ = reply.send(bytes);
        }
    }

    fn query_common(&self, query: Query) -> Option<Query> {
        match query {
            Query::HasProjection(id, reply) => {
                let _ = reply.send(self.has_replica(&id));
            }
            Query::IsClosed(id, reply) => {
                let _ = reply.send(self.is_closed(&id));
            }
            Query::InputEligibility(id, reply) => {
                let _ = reply.send(self.kernel.input_eligibility(&id));
            }
            Query::InputReady(id, reply) => {
                let ready = matches!(
                    self.kernel.input_eligibility(&id),
                    InputEligibility::Eligible { .. }
                );
                let _ = reply.send(ready);
            }
            #[cfg(not(feature = "engine"))]
            query @ Query::TakeOutput(..) => return Some(query),
            #[cfg(feature = "engine")]
            query => return Some(query),
        }
        None
    }

    #[cfg(feature = "engine")]
    fn query_presentation(&mut self, query: Query) -> Option<Query> {
        match query {
            Query::Scroll(id, scroll, reply) => {
                let _ = reply.send(self.scroll(&id, scroll));
            }
            Query::IsAltScreen(id, reply) => {
                let alt = self
                    .replica(&id)
                    .and_then(GhosttyReplica::terminal)
                    .is_some_and(|terminal| {
                        matches!(
                            terminal.active_screen(),
                            Ok(libghostty_vt::screen::Screen::Alternate)
                        )
                    });
                let _ = reply.send(alt);
            }
            Query::PredictText(id, text, reply) => {
                let result = self
                    .predict_text(&id, &text)
                    .and_then(|()| self.render_and_publish(&id));
                let _ = reply.send(result);
            }
            Query::ClearPredictions(id, reply) => {
                if let Some(slot) = self.presentations.get_mut(&id) {
                    slot.predictor.clear();
                }
                let _ = reply.send(self.render_and_publish(&id));
            }
            Query::Republish(id, reply) => {
                let _ = reply.send(self.render_and_publish(&id));
            }
            Query::ReplicaInfo(id, reply) => {
                let _ = reply.send(self.replica_info(&id));
            }
            Query::MouseMode(id, reply) => {
                let _ = reply.send(self.mouse_mode(&id));
            }
            query => return Some(query),
        }
        None
    }

    #[cfg(feature = "engine")]
    fn query_document(&mut self, query: Query) {
        match query {
            Query::TrackAnchor(id, point, reply) => {
                let _ = reply.send(self.track_anchor(&id, point));
            }
            Query::ReleaseAnchor(id, anchor, reply) => {
                let _ = reply.send(self.release_anchor(&id, anchor));
            }
            Query::ClearPresentation(id, stream, bootstrap, reply) => {
                let _ = reply.send(self.clear_presentation(&id, stream, bootstrap));
            }
            Query::PinViewport(id, anchor, reply) => {
                let _ = reply.send(self.pin_viewport(&id, anchor));
            }
            Query::FollowLive(id, reply) => {
                let _ = reply.send(self.follow_live(&id));
            }
            Query::SetSelection(id, start, end, rectangle, reply) => {
                let _ = reply.send(self.set_selection(&id, start, end, rectangle));
            }
            Query::ClearSelection(id, reply) => {
                let _ = reply.send(self.clear_selection(&id));
            }
            Query::SelectionText(id, reply) => {
                let _ = reply.send(self.selection_text(&id));
            }
            Query::SelectionTextBounded(id, max_bytes, reply) => {
                let _ = reply.send(self.selection_text_bounded(&id, max_bytes));
            }
            Query::Search(id, query, case_sensitive, reply) => {
                let _ = reply.send(self.search(&id, &query, case_sensitive));
            }
            Query::Gesture(id, event, reply) => {
                let _ = reply.send(self.selection_gesture(&id, event));
            }
            _ => {}
        }
    }

    fn is_closed(&self, id: &ResourceId) -> bool {
        matches!(
            self.kernel.input_eligibility(id),
            InputEligibility::Ineligible(InputBlockReason::Closed)
        )
    }

    fn apply_batch(&mut self, events: Vec<EngineEvent>) -> Vec<EngineOutcome> {
        let mut damaged = Vec::new();
        let mut outcomes = Vec::with_capacity(events.len());
        for event in events {
            let outcome = self.apply_one(event, &mut damaged);
            let fatal = outcome.resync_required()
                || matches!(outcome.error.as_ref(), Some(EngineApplyError::Protocol(_)));
            outcomes.push(outcome);
            if fatal {
                break;
            }
        }
        #[cfg(feature = "engine")]
        self.publish_damaged(damaged);
        #[cfg(not(feature = "engine"))]
        drop(damaged);
        outcomes
    }

    fn apply_one(&mut self, event: EngineEvent, damaged: &mut Vec<ResourceId>) -> EngineOutcome {
        // A frame queued behind a terminal close is stale evidence, including
        // when the close and stale frame share this batch.
        if event
            .terminal_id()
            .is_some_and(|terminal_id| self.is_closed(terminal_id))
        {
            return EngineOutcome::default();
        }
        if let EngineEvent::AttachStarted { terminals, .. } = &event {
            self.prepare_attach(terminals);
        }
        #[cfg(feature = "engine")]
        let closing = match &event {
            EngineEvent::Closed { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        #[cfg(feature = "engine")]
        let output = match &event {
            EngineEvent::Output { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        let result = apply_event(&mut self.kernel, event, &mut self.effects);
        #[cfg(feature = "engine")]
        if result.is_ok()
            && let Some(id) = output
        {
            self.note_view_output(&id);
        }
        let effects = self.effects.take();
        // A retained close moves the final replica aside before the damage
        // walk sees the kernel's `Removed`, so the walk re-publishes it
        // from there instead of dropping its slot.
        #[cfg(feature = "engine")]
        if let Some(id) = closing {
            self.capture_closed(id);
        }
        self.note_damage(&effects, damaged);
        EngineOutcome {
            effects,
            error: result.as_ref().err().map(EngineApplyError::from_kernel),
        }
    }

    #[cfg(feature = "engine")]
    fn publish_damaged(&mut self, damaged: Vec<ResourceId>) {
        self.release_pending();
        for id in damaged {
            if let Err(error) = self.render_views(&id) {
                tracing::warn!(terminal = %id, %error, "grid projection failed");
            }
        }
    }

    /// Record which terminals gained or lost a projection in effect order,
    /// deduplicating across the whole owner-thread batch.
    fn note_damage(&mut self, effects: &[KernelEffect], damaged: &mut Vec<ResourceId>) {
        for effect in effects {
            #[cfg(feature = "engine")]
            self.note_history_damage(effect, damaged);
            let KernelEffect::Damage(damage) = effect else {
                continue;
            };
            let id = &damage.terminal_id;
            if damage.kind == KernelDamageKind::Removed {
                self.visible.remove(id);
                damaged.retain(|damaged| damaged != id);
                #[cfg(feature = "engine")]
                if self.closed.contains_key(id) {
                    // Retained: the final frame is projected from the
                    // closed replica.
                    damaged.push(id.clone());
                } else {
                    self.projectors.remove(id);
                    self.publication.remove(id);
                    self.forget_views(id);
                    self.invalidate_handles(id);
                    self.presentations.remove(id);
                }
            } else {
                self.visible.insert(id.clone());
                #[cfg(feature = "engine")]
                if let Err(error) = self.bump_document_revision(id) {
                    tracing::warn!(terminal = %id, %error, "document revision could not advance");
                }
                if !damaged.contains(id) {
                    damaged.push(id.clone());
                }
            }
        }
    }

    fn prepare_attach(&mut self, terminals: &[ResourceId]) {
        self.kernel.release_active_attach();
        for terminal_id in terminals {
            if !self.is_closed(terminal_id) {
                continue;
            }
            self.visible.remove(terminal_id);
            #[cfg(feature = "engine")]
            {
                self.closed.remove(terminal_id);
                self.pending_releases.remove(terminal_id);
            }
            let _ = self.kernel.release_terminal(terminal_id);
        }
    }

    #[cfg(feature = "engine")]
    fn has_replica(&self, id: &ResourceId) -> bool {
        self.replica(id).is_some()
    }

    #[cfg(not(feature = "engine"))]
    fn has_replica(&self, id: &ResourceId) -> bool {
        self.visible.contains(id) && self.kernel.published_engine(id).is_some()
    }

    #[cfg(feature = "engine")]
    fn capture_closed(&mut self, id: ResourceId) {
        if let Some(replica) = self.kernel.take_closed_replica(&id) {
            self.closed.insert(id, replica);
        } else {
            self.projectors.remove(&id);
            self.publication.remove(&id);
        }
    }

    #[cfg(feature = "engine")]
    fn release_closed(&mut self, id: &ResourceId) {
        if self.kernel.detach_terminal(id) {
            self.closed.remove(id);
            self.pending_releases.remove(id);
        }
    }

    #[cfg(feature = "engine")]
    fn release_pending(&mut self) {
        for id in self.pending_releases.clone() {
            self.release_closed(&id);
        }
    }

    #[cfg(feature = "engine")]
    fn replica(&self, id: &ResourceId) -> Option<&GhosttyReplica> {
        if self.pending_releases.contains(id) {
            return None;
        }
        if !self.visible.contains(id) {
            return self.closed.get(id).map(ClosedReplica::engine);
        }
        self.kernel
            .published_engine(id)
            .or_else(|| self.closed.get(id).map(ClosedReplica::engine))
    }

    /// The generation token, geometry, key, and last sequence of the replica
    /// a render of `id` reads.
    #[cfg(feature = "engine")]
    fn replica_identity(
        &self,
        id: &ResourceId,
    ) -> Option<(u128, CanonicalGeometry, u64, u64, u64)> {
        if self.pending_releases.contains(id) {
            return None;
        }
        let from_closed = || {
            self.closed.get(id).map(|replica| {
                let key = replica.key();
                (
                    key.generation_token(),
                    replica.geometry(),
                    key.stream_id.get(),
                    key.bootstrap_id.get(),
                    replica.last_seq(),
                )
            })
        };
        if !self.visible.contains(id) {
            return from_closed();
        }
        self.kernel
            .published(id)
            .map(|replica| {
                let key = replica.key();
                (
                    key.generation_token(),
                    replica.geometry(),
                    key.stream_id.get(),
                    key.bootstrap_id.get(),
                    replica.last_seq(),
                )
            })
            .or_else(from_closed)
    }

    #[cfg(feature = "engine")]
    fn predict_text(&mut self, id: &ResourceId, text: &str) -> Result<(), EngineError> {
        let Some((token, geometry, ..)) = self.replica_identity(id) else {
            return Ok(());
        };
        self.ensure_projector(id, token, geometry)?;
        let (cursor, alternate) = {
            let terminal = self
                .replica(id)
                .and_then(GhosttyReplica::terminal)
                .ok_or_else(|| engine_error("terminal has no renderable replica"))?;
            let cursor = (
                terminal.cursor_x().unwrap_or(0),
                terminal.cursor_y().unwrap_or(0),
            );
            let alternate = matches!(
                terminal.active_screen(),
                Ok(libghostty_vt::screen::Screen::Alternate)
            );
            (cursor, alternate)
        };
        let Some(slot) = self.presentations.get_mut(id) else {
            return Ok(());
        };
        slot.predictor
            .predict_text(text, cursor, alternate, monotonic_ms());
        Ok(())
    }

    #[cfg(feature = "engine")]
    fn ensure_projector(
        &mut self,
        id: &ResourceId,
        token: u128,
        geometry: CanonicalGeometry,
    ) -> Result<(), EngineError> {
        let replace = self
            .projectors
            .get(id)
            .is_none_or(|slot| slot.token != token);
        if !replace {
            return Ok(());
        }
        let projector = GridProjector::new().map_err(|error| {
            EngineError::Engine(format!("render state allocation failed: {error}"))
        })?;
        self.reset_generation_presentations(id, geometry);
        self.projectors
            .insert(id.clone(), ProjectorSlot { token, projector });
        Ok(())
    }

    #[cfg(feature = "engine")]
    fn replica_info(&self, id: &ResourceId) -> Result<ReplicaInfo, EngineError> {
        let replica = self
            .kernel
            .published(id)
            .ok_or_else(|| engine_error("terminal has no published READY generation"))?;
        let key = replica.key();
        Ok(ReplicaInfo {
            profile: key.profile,
            stream_id: key.stream_id.get(),
            bootstrap_id: key.bootstrap_id.get(),
            last_seq: replica.last_seq(),
            history: self.kernel.history_cache(id).map(|cache| {
                let mut status = cache.status();
                status.unread_rows = self
                    .presentations
                    .get(id)
                    .map_or(0, |state| state.unread_rows);
                status
            }),
            document_revision: self.document_revisions.get(id).copied().unwrap_or(0),
        })
    }

    #[cfg(feature = "engine")]
    fn mouse_mode(&self, id: &ResourceId) -> Result<MouseMode, EngineError> {
        use libghostty_vt::mouse::{EncoderOptions, TrackingMode};
        let terminal = self.terminal(id)?;
        match EncoderOptions::from_terminal(terminal)
            .map_err(|error| engine_error(error.to_string()))?
            .tracking_mode
        {
            TrackingMode::None => Ok(MouseMode::None),
            TrackingMode::X10 => Ok(MouseMode::X10),
            TrackingMode::Normal => Ok(MouseMode::Normal),
            TrackingMode::Button => Ok(MouseMode::Button),
            TrackingMode::Any => Ok(MouseMode::Any),
            _ => Err(engine_error("unsupported mouse tracking mode")),
        }
    }

    #[cfg(feature = "engine")]
    fn track_anchor(
        &mut self,
        id: &ResourceId,
        point: EngineDocumentPoint,
    ) -> Result<u64, EngineError> {
        let space = document_space(point.space)?;
        let anchor = self
            .kernel
            .track_document_anchor(
                id,
                DocumentPoint {
                    space,
                    x: point.column,
                    y: point.row,
                },
            )
            .map_err(|error| engine_error(error.to_string()))?;
        self.register_anchor(id, anchor)
    }

    #[cfg(feature = "engine")]
    fn register_anchor(
        &mut self,
        id: &ResourceId,
        anchor: DocumentAnchorId,
    ) -> Result<u64, EngineError> {
        let handle = crate::view::allocate_handle()
            .ok_or_else(|| engine_error("document anchor handle space exhausted"))?;
        self.anchors
            .insert(handle, (id.clone(), self.active_view, anchor));
        Ok(handle)
    }

    #[cfg(feature = "engine")]
    fn resolve_anchor(
        &self,
        id: &ResourceId,
        handle: u64,
    ) -> Result<DocumentAnchorId, EngineError> {
        let anchor = self.owned_anchor(id, handle)?;
        if self.anchor_point(id, anchor)?.is_none() {
            return Err(engine_error("document anchor was pruned or invalidated"));
        }
        Ok(anchor)
    }

    #[cfg(feature = "engine")]
    fn owned_anchor(&self, id: &ResourceId, handle: u64) -> Result<DocumentAnchorId, EngineError> {
        let (owner, view, anchor) = self
            .anchors
            .get(&handle)
            .ok_or_else(|| engine_error("document anchor is stale or unknown"))?;
        if owner != id || *view != self.active_view {
            return Err(engine_error("document anchor belongs to another view"));
        }
        Ok(*anchor)
    }

    #[cfg(feature = "engine")]
    fn release_anchor(&mut self, id: &ResourceId, handle: u64) -> Result<(), EngineError> {
        // Pruning invalidates the location, not the owner's obligation to
        // release its budget registration. Unknown/cross-view handles fail.
        let anchor = self.owned_anchor(id, handle)?;
        self.release_tracked_anchor(id, anchor)?;
        self.anchors.remove(&handle);
        Ok(())
    }

    #[cfg(feature = "engine")]
    fn clear_presentation(
        &mut self,
        id: &ResourceId,
        stream_id: u64,
        bootstrap_id: u64,
    ) -> Result<(), EngineError> {
        let info = self.replica_info(id)?;
        if (info.stream_id, info.bootstrap_id) != (stream_id, bootstrap_id) {
            return Err(engine_error("clear targets a stale terminal generation"));
        }
        self.kernel
            .clear_presentation(id)
            .map_err(|error| engine_error(error.to_string()))?;
        self.invalidate_handles(id);
        if let Some((_, geometry, ..)) = self.replica_identity(id) {
            self.reset_generation_presentations(id, geometry);
        }
        self.bump_document_revision(id)?;
        self.render_views(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    fn pin_viewport(&mut self, id: &ResourceId, handle: u64) -> Result<EngineOutcome, EngineError> {
        let anchor = self.resolve_anchor(id, handle)?;
        let point = self
            .kernel
            .document_anchor_point(id, anchor, DocumentSpace::History)
            .map_err(|error| engine_error(error.to_string()))?
            .ok_or_else(|| engine_error("viewport anchor is no longer available"))?;
        self.scroll(id, Scroll::Row(u64::from(point.y)))
    }

    #[cfg(feature = "engine")]
    fn follow_live(&mut self, id: &ResourceId) -> Result<EngineOutcome, EngineError> {
        self.scroll(id, Scroll::Bottom)
    }

    #[cfg(feature = "engine")]
    fn set_selection(
        &mut self,
        id: &ResourceId,
        start_handle: u64,
        end_handle: u64,
        rectangle: bool,
    ) -> Result<(), EngineError> {
        let start = self.resolve_anchor(id, start_handle)?;
        let end = self.resolve_anchor(id, end_handle)?;
        let start_point = self
            .kernel
            .document_anchor_point(id, start, DocumentSpace::History)
            .map_err(|error| engine_error(error.to_string()))?;
        let end_point = self
            .kernel
            .document_anchor_point(id, end, DocumentSpace::History)
            .map_err(|error| engine_error(error.to_string()))?;
        apply_terminal_selection(self.terminal(id)?, start_point, end_point, rectangle)?;
        self.presentation_mut(id)?.selection = Some(EngineDocumentSelection {
            start,
            end,
            rectangle,
        });
        self.render_and_publish(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    fn clear_selection(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        self.reset_gesture(id);
        self.terminal(id)?
            .set_selection(None)
            .map_err(|error| engine_error(error.to_string()))?;
        self.presentation_mut(id)?.selection = None;
        self.render_and_publish(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    fn selection_text(&self, id: &ResourceId) -> Result<Vec<u8>, EngineError> {
        let selection = self
            .presentations
            .get(id)
            .and_then(|state| state.selection)
            .ok_or_else(|| engine_error("terminal has no active selection"))?;
        self.kernel
            .format_document_selection(id, selection)
            .map(|text| text.unwrap_or_default().into_bytes())
            .map_err(|error| engine_error(error.to_string()))
    }

    #[cfg(feature = "engine")]
    fn selection_text_bounded(
        &self,
        id: &ResourceId,
        max_bytes: usize,
    ) -> Result<super::BoundedSelectionText, EngineError> {
        let Some(selection) = self.presentations.get(id).and_then(|state| state.selection) else {
            return Ok(super::BoundedSelectionText::Unavailable);
        };
        self.kernel
            .format_document_selection_bounded(id, selection, max_bytes)
            .map_err(|error| engine_error(error.to_string()))
    }

    #[cfg(feature = "engine")]
    fn search(
        &mut self,
        id: &ResourceId,
        query: &str,
        case_sensitive: bool,
    ) -> Result<Vec<SearchMatch>, EngineError> {
        if query.is_empty() {
            return Err(engine_error("search query is empty"));
        }
        self.clear_search_handles(id)?;
        self.kernel
            .adapter_mut()
            .set_search_case_sensitive(case_sensitive);
        let result = self.kernel.search_loaded_history(id, query, 4096);
        self.kernel.adapter_mut().set_search_case_sensitive(true);
        let matches = result.map_err(|error| engine_error(error.to_string()))?;
        let mut found = Vec::with_capacity(matches.len());
        for matched in matches {
            let start = self.register_anchor(id, matched.start)?;
            let end = match self.register_anchor(id, matched.end) {
                Ok(end) => end,
                Err(error) => {
                    let _ = self.release_anchor(id, start);
                    return Err(error);
                }
            };
            found.push(SearchMatch { start, end });
            self.presentation_mut(id)?.search.extend([start, end]);
        }
        Ok(found)
    }

    #[cfg(feature = "engine")]
    fn selection_gesture(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, EngineError> {
        validate_gesture(event)?;
        let token = self
            .replica_identity(id)
            .map(|identity| identity.0)
            .ok_or_else(|| engine_error("terminal has no published READY generation"))?;
        let mut state = self.gesture_state(id, event, token)?;
        let result = self.gesture_snapshot(id, event, &mut state);
        self.finish_gesture(id, event, state, result.is_err());
        result
    }

    #[cfg(feature = "engine")]
    fn gesture_state(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
        token: u128,
    ) -> Result<PointerGesture, EngineError> {
        if event.phase == 0 {
            self.clear_selection(id)?;
            let handle = crate::view::allocate_handle()
                .ok_or_else(|| engine_error("gesture handles exhausted"))?;
            return Ok(PointerGesture {
                token,
                handle,
                gesture: Gesture::new().map_err(|error| engine_error(error.to_string()))?,
            });
        }
        let valid = self
            .presentations
            .get(id)
            .and_then(|state| state.gesture.as_ref())
            .is_some_and(|state| state.token == token && state.handle == event.handle);
        if !valid {
            return Err(engine_error("stale selection gesture"));
        }
        self.presentation_mut(id)?
            .gesture
            .take()
            .ok_or_else(|| engine_error("missing selection gesture"))
    }

    #[cfg(feature = "engine")]
    fn finish_gesture(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
        mut state: PointerGesture,
        failed: bool,
    ) {
        if event.phase == 2 || failed {
            if let Ok(terminal) = self.terminal(id) {
                state.gesture.reset(terminal);
            }
        } else if let Some(presentation) = self.presentations.get_mut(id) {
            presentation.gesture = Some(state);
        }
    }

    #[cfg(feature = "engine")]
    fn gesture_snapshot(
        &mut self,
        id: &ResourceId,
        event: SelectionGestureEvent,
        state: &mut PointerGesture,
    ) -> Result<SelectionGestureResult, EngineError> {
        let mut result = SelectionGestureResult {
            handle: state.handle,
            ..SelectionGestureResult::default()
        };
        if event.phase == 2 {
            return Ok(result);
        }
        let terminal = self.terminal(id)?;
        let grid_ref = terminal
            .grid_ref(Point::Viewport(PointCoordinate {
                x: event.column,
                y: event.row,
            }))
            .map_err(|error| engine_error(error.to_string()))?;
        let selected = apply_gesture_event(terminal, grid_ref, event, &mut state.gesture)?;
        let Some(selected) = selected else {
            self.clear_selection(id)?;
            return Ok(result);
        };
        let (start, end) = gesture_document_points(terminal, &selected)?;
        (result.start, result.end) =
            self.commit_gesture_selection(id, start, end, event.rectangle)?;
        Ok(result)
    }

    #[cfg(feature = "engine")]
    fn commit_gesture_selection(
        &mut self,
        id: &ResourceId,
        start: EngineDocumentPoint,
        end: EngineDocumentPoint,
        rectangle: bool,
    ) -> Result<(u64, u64), EngineError> {
        let start = self.track_anchor(id, start)?;
        let end = match self.track_anchor(id, end) {
            Ok(anchor) => anchor,
            Err(error) => {
                let _ = self.release_anchor(id, start);
                return Err(error);
            }
        };
        if let Err(error) = self.set_selection(id, start, end, rectangle) {
            let _ = self.release_anchor(id, start);
            let _ = self.release_anchor(id, end);
            return Err(error);
        }
        let old = std::mem::replace(
            &mut self.presentation_mut(id)?.gesture_anchors,
            vec![start, end],
        );
        for handle in old {
            self.drop_anchor_handle(id, handle);
        }
        Ok((start, end))
    }

    #[cfg(feature = "engine")]
    fn terminal(
        &self,
        id: &ResourceId,
    ) -> Result<&libghostty_vt::Terminal<'static, 'static>, EngineError> {
        self.replica(id)
            .and_then(GhosttyReplica::terminal)
            .ok_or_else(|| engine_error("terminal has no renderable engine"))
    }

    #[cfg(feature = "engine")]
    fn reset_gesture(&mut self, id: &ResourceId) {
        let Some(mut state) = self
            .presentations
            .get_mut(id)
            .and_then(|state| state.gesture.take())
        else {
            return;
        };
        let current = self.replica_identity(id).map(|identity| identity.0);
        if current == Some(state.token)
            && let Ok(terminal) = self.terminal(id)
        {
            state.gesture.reset(terminal);
        }
    }

    #[cfg(feature = "engine")]
    fn invalidate_handles(&mut self, id: &ResourceId) {
        self.reset_gesture(id);
        self.anchors
            .retain(|_, (owner, view, _)| owner != id || *view != self.active_view);
        if let Some(state) = self.presentations.get_mut(id) {
            state.selection = None;
            state.viewport = None;
            state.search.clear();
            state.gesture_anchors.clear();
        }
    }

    #[cfg(feature = "engine")]
    fn bump_document_revision(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let revision = self.next_document_revision;
        self.next_document_revision = revision
            .checked_add(1)
            .ok_or_else(|| engine_error("document revision space exhausted"))?;
        self.document_revisions.insert(id.clone(), revision);
        Ok(())
    }

    #[cfg(feature = "engine")]
    fn scroll(&mut self, id: &ResourceId, scroll: Scroll) -> Result<EngineOutcome, EngineError> {
        let active = self.kernel.published_engine_mut(id).is_some();
        self.scroll_replica(id, scroll)?;
        if active {
            self.update_history_viewport(id)?;
        } else {
            self.remember_closed_scroll(id)?;
        }
        self.render_and_publish(id)?;
        Ok(EngineOutcome {
            effects: self.effects.take(),
            error: None,
        })
    }

    #[cfg(feature = "engine")]
    fn update_history_viewport(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        let scrollbar = self
            .terminal(id)?
            .scrollbar()
            .map_err(|error| engine_error(format!("scrollbar: {error}")))?;
        let at_tail = scrollbar.offset.saturating_add(scrollbar.len) >= scrollbar.total;
        if at_tail {
            self.follow_history_tail(id)?;
        } else {
            self.pin_history_viewport(id)?;
        }
        let rows_from_oldest = usize::try_from(scrollbar.offset)
            .map_err(|_| engine_error("history viewport offset exceeds usize"))?;
        let _ = self
            .kernel
            .prefetch_history(id, rows_from_oldest, &mut self.effects);
        Ok(())
    }

    #[cfg(feature = "engine")]
    fn follow_history_tail(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        if let Some(old) = self.presentation_mut(id)?.viewport.take() {
            self.release_tracked_anchor(id, old)?;
        }
        self.reset_unread(id)
    }

    #[cfg(feature = "engine")]
    fn pin_history_viewport(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        // The kernel cache still owns page budgets, cursor continuity, anchor
        // accounting, and prefetch. Its singleton viewport stays at Tail:
        // pinning it here would make pruning one view clear every view's
        // document anchors in SessionKernel::reconcile_pinned_anchor.
        // Presentation pinning belongs exclusively to this view record.
        let anchor = self
            .kernel
            .track_document_anchor(
                id,
                DocumentPoint {
                    space: DocumentSpace::Viewport,
                    x: 0,
                    y: 0,
                },
            )
            .map_err(|error| engine_error(error.to_string()))?;
        if let Some(old) = self.presentation_mut(id)?.viewport.replace(anchor) {
            self.release_tracked_anchor(id, old)?;
        }
        self.reset_unread(id)
    }
}

#[cfg(feature = "engine")]
fn viewport_scroll(scroll: Scroll) -> Result<ScrollViewport, EngineError> {
    match scroll {
        Scroll::Top => Ok(ScrollViewport::Top),
        Scroll::Bottom => Ok(ScrollViewport::Bottom),
        Scroll::Delta(delta) => isize::try_from(delta)
            .map(ScrollViewport::Delta)
            .map_err(|_| engine_error("scroll delta exceeds isize")),
        Scroll::Row(row) => usize::try_from(row)
            .map(ScrollViewport::Row)
            .map_err(|_| engine_error("scroll row exceeds usize")),
    }
}

#[cfg(feature = "engine")]
fn terminal_defaults(
    terminal: &libghostty_vt::Terminal<'_, '_>,
) -> Result<(Option<Rgb>, Option<Rgb>, bool), EngineError> {
    use libghostty_vt::terminal::Mode;
    let map = |color: libghostty_vt::style::RgbColor| Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    };
    let mut foreground = terminal
        .fg_color()
        .map_err(|error| engine_error(error.to_string()))?
        .map(map);
    let mut background = terminal
        .bg_color()
        .map_err(|error| engine_error(error.to_string()))?
        .map(map);
    let reversed = terminal
        .mode(Mode::REVERSE_COLORS)
        .map_err(|error| engine_error(error.to_string()))?;
    if reversed {
        std::mem::swap(&mut foreground, &mut background);
    }
    Ok((foreground, background, reversed))
}

#[cfg(feature = "engine")]
const fn apply_default_colors(
    colors: &mut super::FrameColors,
    defaults: (Option<Rgb>, Option<Rgb>, bool),
) {
    colors.has_foreground = defaults.0.is_some();
    colors.has_background = defaults.1.is_some();
    colors.reversed = defaults.2;
    if let Some(foreground) = defaults.0 {
        colors.foreground = foreground;
    }
    if let Some(background) = defaults.1 {
        colors.background = background;
    }
}

#[cfg(feature = "engine")]
fn engine_error(message: impl Into<String>) -> EngineError {
    EngineError::Engine(message.into())
}

#[cfg(feature = "engine")]
fn document_space(space: u32) -> Result<DocumentSpace, EngineError> {
    match space {
        0 => Ok(DocumentSpace::History),
        1 => Ok(DocumentSpace::Viewport),
        2 => Ok(DocumentSpace::Active),
        _ => Err(engine_error("unknown document point space")),
    }
}

#[cfg(feature = "engine")]
fn apply_terminal_selection(
    terminal: &libghostty_vt::Terminal<'_, '_>,
    start: Option<DocumentPoint>,
    end: Option<DocumentPoint>,
    rectangle: bool,
) -> Result<(), EngineError> {
    let (Some(start), Some(end)) = (start, end) else {
        return terminal
            .set_selection(None)
            .map(|_| ())
            .map_err(|error| engine_error(error.to_string()));
    };
    let start_ref = terminal
        .grid_ref(Point::History(PointCoordinate {
            x: start.x,
            y: start.y,
        }))
        .map_err(|error| engine_error(error.to_string()))?;
    let end_ref = terminal
        .grid_ref(Point::History(PointCoordinate { x: end.x, y: end.y }))
        .map_err(|error| engine_error(error.to_string()))?;
    terminal
        .set_selection(Some(&Selection::new(start_ref, end_ref, rectangle)))
        .map(|_| ())
        .map_err(|error| engine_error(error.to_string()))
}

#[cfg(feature = "engine")]
fn apply_gesture_event<'terminal>(
    terminal: &'terminal libghostty_vt::Terminal<'_, '_>,
    grid_ref: libghostty_vt::screen::GridRef<'terminal>,
    event: SelectionGestureEvent,
    gesture: &mut Gesture<'_>,
) -> Result<Option<Selection<'terminal>>, EngineError> {
    if event.phase == 0 {
        apply_gesture_press(terminal, grid_ref, event, gesture)
    } else {
        apply_gesture_drag(terminal, grid_ref, event, gesture)
    }
}

#[cfg(feature = "engine")]
fn apply_gesture_press<'terminal>(
    terminal: &'terminal libghostty_vt::Terminal<'_, '_>,
    grid_ref: libghostty_vt::screen::GridRef<'terminal>,
    event: SelectionGestureEvent,
    gesture: &mut Gesture<'_>,
) -> Result<Option<Selection<'terminal>>, EngineError> {
    let behavior = match event.clicks {
        2 => Behavior::Word,
        3 => Behavior::Line,
        _ => Behavior::Cell,
    };
    let mut press = PressEvent::new().map_err(|error| engine_error(error.to_string()))?;
    press
        .set_behaviors(&Behaviors::new().with_single_click_behavior(behavior))
        .map_err(|error| engine_error(error.to_string()))?;
    press
        .set_position(event.x, event.y)
        .map_err(|error| engine_error(error.to_string()))?;
    press
        .apply(gesture, terminal, grid_ref)
        .map_err(|error| engine_error(error.to_string()))
}

#[cfg(feature = "engine")]
fn apply_gesture_drag<'terminal>(
    terminal: &'terminal libghostty_vt::Terminal<'_, '_>,
    grid_ref: libghostty_vt::screen::GridRef<'terminal>,
    event: SelectionGestureEvent,
    gesture: &mut Gesture<'_>,
) -> Result<Option<Selection<'terminal>>, EngineError> {
    let mut drag = DragEvent::new().map_err(|error| engine_error(error.to_string()))?;
    drag.set_position(event.x, event.y)
        .map_err(|error| engine_error(error.to_string()))?;
    drag.set_rectangle(event.rectangle)
        .map_err(|error| engine_error(error.to_string()))?;
    drag.apply(
        gesture,
        terminal,
        grid_ref,
        Geometry {
            columns: event.columns,
            cell_width: event.cell_width,
            padding_left: event.padding_left,
            screen_height: event.screen_height,
        },
    )
    .map_err(|error| engine_error(error.to_string()))
}

#[cfg(feature = "engine")]
fn gesture_document_points(
    terminal: &libghostty_vt::Terminal<'_, '_>,
    selected: &Selection<'_>,
) -> Result<(EngineDocumentPoint, EngineDocumentPoint), EngineError> {
    let start = terminal
        .point_from_grid_ref(&selected.start(), PointSpace::History)
        .map_err(|error| engine_error(error.to_string()))?
        .ok_or_else(|| engine_error("lost gesture start"))?;
    let end = terminal
        .point_from_grid_ref(&selected.end(), PointSpace::History)
        .map_err(|error| engine_error(error.to_string()))?
        .ok_or_else(|| engine_error("lost gesture end"))?;
    Ok((
        EngineDocumentPoint {
            space: 0,
            column: start.x,
            row: start.y,
        },
        EngineDocumentPoint {
            space: 0,
            column: end.x,
            row: end.y,
        },
    ))
}

#[cfg(feature = "engine")]
fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;

    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(feature = "engine")]
fn validate_gesture(event: SelectionGestureEvent) -> Result<(), EngineError> {
    if event.phase > 2 || !event.x.is_finite() || !event.y.is_finite() {
        return Err(engine_error("invalid gesture event"));
    }
    if event.phase < 2 && (event.columns == 0 || event.cell_width == 0 || event.screen_height == 0)
    {
        return Err(engine_error("invalid gesture geometry"));
    }
    if event.phase == 0 && !(1..=3).contains(&event.clicks) {
        return Err(engine_error("invalid gesture clicks"));
    }
    Ok(())
}
