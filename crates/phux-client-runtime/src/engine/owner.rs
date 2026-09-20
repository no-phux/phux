//! Kernel ownership, queries, projection, and publication.

use super::apply::apply_event;
#[cfg(feature = "engine")]
use super::apply::frame_colors;
use super::{
    Adapter, Command, EffectBuffer, EngineEvent, EngineOutcome, HashSet, InputBlockReason,
    InputEligibility, KernelDamageKind, KernelEffect, Lifecycle, Query, ResourceId, SessionKernel,
    mpsc,
};
#[cfg(feature = "engine")]
use super::{
    Arc, CanonicalGeometry, ClosedReplica, EngineError, GhosttyAdapter, GhosttyReplica, GridBuffer,
    GridFrame, GridProjector, HashMap, Publication, Scroll, ScrollViewport, Scrollbar,
};

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
            Lifecycle::Detach(id) => {
                let _ = self.kernel.detach_terminal(&id);
                self.visible.remove(&id);
                #[cfg(feature = "engine")]
                {
                    self.closed.remove(&id);
                    self.pending_releases.remove(&id);
                    self.projectors.remove(&id);
                    self.publication.remove(&id);
                }
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

    fn query(&mut self, query: Query) {
        match query {
            Query::HasProjection(id, reply) => {
                let _ = reply.send(self.has_replica(&id));
            }
            Query::IsClosed(id, reply) => {
                let _ = reply.send(self.is_closed(&id));
            }
            Query::InputReady(id, reply) => {
                let ready = matches!(
                    self.kernel.input_eligibility(&id),
                    InputEligibility::Eligible { .. }
                );
                let _ = reply.send(ready);
            }
            #[cfg(feature = "engine")]
            Query::Scroll(id, scroll, reply) => {
                let _ = reply.send(self.scroll(&id, scroll));
            }
            #[cfg(feature = "engine")]
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
            #[cfg(feature = "engine")]
            Query::Republish(id, reply) => {
                let _ = reply.send(self.render_and_publish(&id));
            }
            #[cfg(not(feature = "engine"))]
            Query::TakeOutput(id, reply) => {
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
            error: result.err().map(|error| error.to_string()),
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
        let replace = self
            .projectors
            .get(id)
            .is_none_or(|slot| slot.token != token);
        if replace {
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
        }
        let Some(mut slot) = self.projectors.remove(id) else {
            return Ok(false);
        };
        let Some(terminal) = self.replica(id).and_then(GhosttyReplica::terminal) else {
            self.projectors.insert(id.clone(), slot);
            return Ok(false);
        };
        let projected = slot
            .projector
            .project(terminal)
            .map(|snapshot| {
                (
                    snapshot.cols,
                    snapshot.rows,
                    snapshot.cursor,
                    frame_colors(&snapshot.colors),
                    snapshot.damage,
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
    fn scroll(&mut self, id: &ResourceId, scroll: Scroll) -> Result<(), EngineError> {
        let viewport = match scroll {
            Scroll::Top => ScrollViewport::Top,
            Scroll::Bottom => ScrollViewport::Bottom,
            Scroll::Delta(delta) => ScrollViewport::Delta(
                isize::try_from(delta)
                    .map_err(|_| EngineError::Engine("scroll delta exceeds isize".to_owned()))?,
            ),
            Scroll::Row(row) => ScrollViewport::Row(
                usize::try_from(row)
                    .map_err(|_| EngineError::Engine("scroll row exceeds usize".to_owned()))?,
            ),
        };
        let scrolled = if let Some(replica) = self.kernel.published_engine_mut(id) {
            replica.scroll_viewport(viewport)
        } else if let Some(replica) = self.closed.get_mut(id) {
            replica.engine_mut().scroll_viewport(viewport)
        } else {
            return Err(EngineError::Engine(
                "terminal has no scrollable replica".to_owned(),
            ));
        };
        scrolled.map_err(|error| EngineError::Engine(error.to_string()))?;
        self.render_and_publish(id).map(|_| ())
    }
}
