//! Kernel ownership, queries, projection, and publication.

use super::apply::apply_event;
#[cfg(feature = "engine")]
use super::apply::frame_colors;
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
struct PointerGesture {
    token: u128,
    handle: u64,
    gesture: Gesture<'static>,
}

#[cfg(feature = "engine")]
struct ProjectorSlot {
    token: u128,
    projector: GridProjector,
    /// The buffer recycled from the frame the last publish replaced.
    spare: Option<GridBuffer>,
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
    anchors: HashMap<u64, (ResourceId, DocumentAnchorId)>,
    #[cfg(feature = "engine")]
    next_anchor: u64,
    #[cfg(feature = "engine")]
    selections: HashMap<ResourceId, EngineDocumentSelection>,
    #[cfg(feature = "engine")]
    gestures: HashMap<ResourceId, PointerGesture>,
    #[cfg(feature = "engine")]
    next_gesture: u64,
    #[cfg(feature = "engine")]
    viewport_anchors: HashMap<ResourceId, DocumentAnchorId>,
    #[cfg(feature = "engine")]
    document_revisions: HashMap<ResourceId, u64>,
    #[cfg(feature = "engine")]
    next_document_revision: u64,
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
            next_anchor: 1,
            #[cfg(feature = "engine")]
            selections: HashMap::new(),
            #[cfg(feature = "engine")]
            gestures: HashMap::new(),
            #[cfg(feature = "engine")]
            next_gesture: 1,
            #[cfg(feature = "engine")]
            viewport_anchors: HashMap::new(),
            #[cfg(feature = "engine")]
            document_revisions: HashMap::new(),
            #[cfg(feature = "engine")]
            next_document_revision: 1,
        }
    }

    pub(super) fn run(mut self, commands: &mpsc::Receiver<Command>) {
        while let Ok(command) = commands.recv() {
            match command {
                Command::Apply(event, reply) => {
                    let _ = reply.send(self.apply(event));
                }
                Command::Lifecycle(lifecycle) => self.lifecycle(lifecycle),
                Command::Query(query) => self.query(query),
            }
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
                }
                self.projectors.remove(&id);
                self.publication.remove(&id);
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

    fn apply(&mut self, event: EngineEvent) -> EngineOutcome {
        if let EngineEvent::Closed { terminal_id, .. } = &event
            && self.is_closed(terminal_id)
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
        let result = apply_event(&mut self.kernel, event, &mut self.effects);
        let effects = self.effects.take();
        // A retained close moves the final replica aside before the damage
        // walk sees the kernel's `Removed`, so the walk re-publishes it
        // from there instead of dropping its slot.
        #[cfg(feature = "engine")]
        if let Some(id) = closing {
            self.capture_closed(id);
        }
        let damaged = self.note_damage(&effects);
        #[cfg(feature = "engine")]
        {
            self.release_pending();
            for id in damaged {
                if let Err(error) = self.render_and_publish(&id) {
                    tracing::warn!(terminal = %id, %error, "grid projection failed");
                }
            }
        }
        #[cfg(not(feature = "engine"))]
        drop(damaged);
        EngineOutcome {
            effects,
            error: result.as_ref().err().map(EngineApplyError::from_kernel),
        }
    }

    /// Record which terminals gained or lost a projection, and return the
    /// ones to re-project, in effect order without duplicates.
    fn note_damage(&mut self, effects: &[KernelEffect]) -> Vec<ResourceId> {
        let mut damaged = Vec::new();
        for effect in effects {
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
        damaged
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

    /// Project `id`'s replica into the back buffer and publish it. `Ok(false)`
    /// means there is nothing renderable yet (a native replica before READY).
    #[cfg(feature = "engine")]
    fn render_and_publish(&mut self, id: &ResourceId) -> Result<bool, EngineError> {
        let Some((token, _geometry, stream_id, bootstrap_id, last_seq)) = self.replica_identity(id)
        else {
            return Ok(false);
        };
        self.ensure_projector(id, token)?;
        let Some(mut slot) = self.projectors.remove(id) else {
            return Ok(false);
        };
        let Some(terminal) = self.replica(id).and_then(GhosttyReplica::terminal) else {
            self.projectors.insert(id.clone(), slot);
            return Ok(false);
        };
        let defaults = terminal_defaults(terminal)?;
        let projected = slot
            .projector
            .project(terminal)
            .map(|snapshot| {
                let mut colors = frame_colors(&snapshot.colors);
                colors.has_foreground = defaults.0.is_some();
                colors.has_background = defaults.1.is_some();
                colors.reversed = defaults.2;
                if let Some(foreground) = defaults.0 {
                    colors.foreground = foreground;
                }
                if let Some(background) = defaults.1 {
                    colors.background = background;
                }
                let damage = match snapshot.damage {
                    // Publication has no separate metadata-damage channel.
                    // Mode-only updates (for example DECSCNM) must therefore
                    // conservatively repaint instead of disappearing as Clean.
                    GridDamage::Clean => GridDamage::Full,
                    damage => damage,
                };
                (
                    snapshot.cols,
                    snapshot.rows,
                    snapshot.cursor,
                    colors,
                    damage,
                )
            })
            .map_err(|error| EngineError::Engine(error.to_string()));
        let scrollbar = terminal.scrollbar().map(|bar| Scrollbar {
            total: bar.total,
            offset: bar.offset,
            len: bar.len,
        });
        let outcome = projected.and_then(|(cols, rows, cursor, colors, damage)| {
            let scrollbar =
                scrollbar.map_err(|error| EngineError::Engine(format!("scrollbar: {error}")))?;
            let mut buffer = slot.spare.take().unwrap_or_default();
            slot.projector.swap_buffer(&mut buffer);
            let frame = GridFrame {
                terminal_id: id.clone(),
                generation: 0,
                stream_id,
                bootstrap_id,
                last_seq,
                cols,
                rows,
                cursor,
                scrollbar,
                colors,
                damage,
                buffer,
            };
            if let Some(previous) = self.publication.publish(id, frame)
                && let Ok(previous) = Arc::try_unwrap(previous)
            {
                slot.spare = Some(previous.buffer);
            }
            Ok(true)
        });
        self.projectors.insert(id.clone(), slot);
        outcome
    }

    #[cfg(feature = "engine")]
    fn ensure_projector(&mut self, id: &ResourceId, token: u128) -> Result<(), EngineError> {
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
        self.projectors.insert(
            id.clone(),
            ProjectorSlot {
                token,
                projector,
                spare: None,
            },
        );
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
            history: self
                .kernel
                .history_cache(id)
                .map(phux_client_core::history::HistoryCache::status),
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
        let handle = self.next_anchor;
        self.next_anchor = handle
            .checked_add(1)
            .ok_or_else(|| engine_error("document anchor handle space exhausted"))?;
        self.anchors.insert(handle, (id.clone(), anchor));
        Ok(handle)
    }

    #[cfg(feature = "engine")]
    fn resolve_anchor(
        &self,
        id: &ResourceId,
        handle: u64,
    ) -> Result<DocumentAnchorId, EngineError> {
        let (owner, anchor) = self
            .anchors
            .get(&handle)
            .ok_or_else(|| engine_error("document anchor is stale or unknown"))?;
        if owner != id {
            return Err(engine_error("document anchor belongs to another terminal"));
        }
        Ok(*anchor)
    }

    #[cfg(feature = "engine")]
    fn release_anchor(&mut self, id: &ResourceId, handle: u64) -> Result<(), EngineError> {
        let anchor = self.resolve_anchor(id, handle)?;
        self.kernel
            .release_document_anchor(id, anchor)
            .map_err(|error| engine_error(error.to_string()))?;
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
        self.bump_document_revision(id)?;
        self.render_and_publish(id).map(|_| ())
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
        self.selections.insert(
            id.clone(),
            EngineDocumentSelection {
                start,
                end,
                rectangle,
            },
        );
        self.render_and_publish(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    fn clear_selection(&mut self, id: &ResourceId) -> Result<(), EngineError> {
        self.reset_gesture(id);
        self.terminal(id)?
            .set_selection(None)
            .map_err(|error| engine_error(error.to_string()))?;
        self.selections.remove(id);
        self.render_and_publish(id).map(|_| ())
    }

    #[cfg(feature = "engine")]
    fn selection_text(&self, id: &ResourceId) -> Result<Vec<u8>, EngineError> {
        let selection = self
            .selections
            .get(id)
            .copied()
            .ok_or_else(|| engine_error("terminal has no active selection"))?;
        self.kernel
            .format_document_selection(id, selection)
            .map(|text| text.unwrap_or_default().into_bytes())
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
            let handle = self.next_gesture;
            self.next_gesture = handle
                .checked_add(1)
                .ok_or_else(|| engine_error("gesture handles exhausted"))?;
            return Ok(PointerGesture {
                token,
                handle,
                gesture: Gesture::new().map_err(|error| engine_error(error.to_string()))?,
            });
        }
        let valid = self
            .gestures
            .get(id)
            .is_some_and(|state| state.token == token && state.handle == event.handle);
        if !valid {
            return Err(engine_error("stale selection gesture"));
        }
        self.gestures
            .remove(id)
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
        } else {
            self.gestures.insert(id.clone(), state);
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
        let Some(mut state) = self.gestures.remove(id) else {
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
        self.anchors.retain(|_, (owner, _)| owner != id);
        self.selections.remove(id);
        self.viewport_anchors.remove(id);
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
        let viewport = viewport_scroll(scroll)?;
        let active = self.kernel.published_engine_mut(id).is_some();
        let scrolled = if let Some(replica) = self.kernel.published_engine_mut(id) {
            replica.scroll_viewport(viewport)
        } else if let Some(replica) = self.closed.get_mut(id) {
            replica.engine_mut().scroll_viewport(viewport)
        } else {
            return Err(engine_error("terminal has no scrollable replica"));
        };
        scrolled.map_err(|error| engine_error(error.to_string()))?;
        if active {
            self.update_history_viewport(id)?;
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
        self.kernel
            .follow_history_tail(id)
            .map_err(|error| engine_error(error.to_string()))?;
        if let Some(old) = self.viewport_anchors.remove(id) {
            self.kernel
                .release_document_anchor(id, old)
                .map_err(|error| engine_error(error.to_string()))?;
        }
        Ok(())
    }

    #[cfg(feature = "engine")]
    fn pin_history_viewport(&mut self, id: &ResourceId) -> Result<(), EngineError> {
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
        self.kernel
            .pin_history_viewport(id, anchor)
            .map_err(|error| engine_error(error.to_string()))?;
        if let Some(old) = self.viewport_anchors.insert(id.clone(), anchor) {
            self.kernel
                .release_document_anchor(id, old)
                .map_err(|error| engine_error(error.to_string()))?;
        }
        Ok(())
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
