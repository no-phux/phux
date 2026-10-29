//! Replica lifetime, projection state, and input-mode queries.

#[cfg(feature = "engine")]
use super::{
    CanonicalGeometry, ClosedReplica, EngineError, GhosttyReplica, GridProjector, InputEligibility,
    MouseMode, ProjectorSlot, ReplicaInfo, engine_error, images, monotonic_ms,
};
use super::{Owner, ResourceId};

impl Owner {
    #[cfg(feature = "engine")]
    pub(super) fn has_replica(&self, id: &ResourceId) -> bool {
        self.replica(id).is_some()
    }

    #[cfg(not(feature = "engine"))]
    pub(super) fn has_replica(&self, id: &ResourceId) -> bool {
        self.visible.contains(id) && self.kernel.published_engine(id).is_some()
    }

    #[cfg(feature = "engine")]
    pub(super) fn require_projection(&self, id: &ResourceId) -> Result<(), EngineError> {
        if !self.has_replica(id) {
            return Err(EngineError::ProjectionUnavailable);
        }
        Ok(())
    }

    #[cfg(feature = "engine")]
    pub(super) fn capture_closed(&mut self, id: ResourceId) {
        if let Some(replica) = self.kernel.take_closed_replica(&id) {
            self.closed.insert(id, replica);
        } else {
            self.projectors.remove(&id);
            self.publication.remove(&id);
        }
    }

    #[cfg(feature = "engine")]
    pub(super) fn release_closed(&mut self, id: &ResourceId) {
        if self.kernel.detach_terminal(id) {
            self.closed.remove(id);
            self.pending_releases.remove(id);
        }
    }

    #[cfg(feature = "engine")]
    pub(super) fn release_pending(&mut self) {
        for id in self.pending_releases.clone() {
            self.release_closed(&id);
        }
    }

    #[cfg(feature = "engine")]
    pub(super) fn replica(&self, id: &ResourceId) -> Option<&GhosttyReplica> {
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
    pub(super) fn replica_identity(
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
    pub(super) fn predict_text(&mut self, id: &ResourceId, text: &str) -> Result<(), EngineError> {
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
    pub(super) fn ensure_projector(
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
        if let (Some((width, height)), Some(replica)) = (
            self.cell_pixels.get(id),
            self.kernel.published_engine_mut(id),
        ) {
            replica
                .set_cell_pixels(*width, *height)
                .map_err(|error| engine_error(error.to_string()))?;
        }
        let projector = GridProjector::new().map_err(|error| {
            EngineError::Engine(format!("render state allocation failed: {error}"))
        })?;
        self.reset_generation_presentations(id, geometry);
        self.projectors.insert(
            id.clone(),
            ProjectorSlot {
                token,
                projector,
                images: images::Images::default(),
                reader: None,
                valid: false,
            },
        );
        Ok(())
    }

    #[cfg(feature = "engine")]
    pub(super) fn replica_info(&self, id: &ResourceId) -> Result<ReplicaInfo, EngineError> {
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
            input_ready: matches!(
                self.kernel.input_eligibility(id),
                InputEligibility::Eligible { .. }
            ),
        })
    }

    #[cfg(feature = "engine")]
    pub(super) fn mouse_mode(&self, id: &ResourceId) -> Result<MouseMode, EngineError> {
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
    pub(super) fn bracketed_paste(&self, id: &ResourceId) -> Result<bool, EngineError> {
        self.terminal(id)?
            .mode(libghostty_vt::terminal::Mode::BRACKETED_PASTE)
            .map_err(|error| engine_error(error.to_string()))
    }
}
