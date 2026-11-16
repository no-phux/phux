//! DEC 2026 transactions hold every presentation, not just output-driven paints.

use std::time::{Duration, Instant};

use super::{EngineError, Owner, ResourceId};

const WATCHDOG: Duration = Duration::from_secs(1);

pub(super) struct Hold {
    token: u128,
    deadline: Instant,
}

impl Owner {
    /// Observe bootstrap/replacement holds before unread-slot pacing can
    /// defer projection. End/start pairs inside a batch keep the deadline:
    /// without a committed frame, continuous redraws must not postpone it.
    pub(super) fn track_sync_output(&mut self, id: &ResourceId) {
        if let Some((token, ..)) = self.replica_identity(id)
            && let Err(error) = self.hold_output(id, token)
        {
            tracing::warn!(terminal = %id, %error, "synchronized output mode query failed");
        }
    }

    pub(super) fn hold_output(
        &mut self,
        id: &ResourceId,
        token: u128,
    ) -> Result<bool, EngineError> {
        let synchronized = self
            .terminal(id)?
            .mode(libghostty_vt::mode::Mode::SYNC_OUTPUT)
            .map_err(|error| super::engine_error(error.to_string()))?;
        if !synchronized || self.is_closed(id) {
            self.sync_output.remove(id);
            return Ok(false);
        }
        let now = Instant::now();
        let hold = self.sync_output.entry(id.clone()).or_insert(Hold {
            token,
            deadline: now + WATCHDOG,
        });
        if hold.token != token {
            *hold = Hold {
                token,
                deadline: now + WATCHDOG,
            };
        }
        Ok(true)
    }

    pub(super) fn sync_output_deadline(&self) -> Option<Instant> {
        self.sync_output.values().map(|hold| hold.deadline).min()
    }

    /// The connection driver's existing deadline lane wakes an otherwise idle
    /// consumer too. A missing end marker must never strand a held publication.
    pub(super) fn expire_sync_output(&mut self, now: Instant) -> Vec<ResourceId> {
        let expired: Vec<_> = self
            .sync_output
            .iter()
            .filter_map(|(id, hold)| {
                if now < hold.deadline {
                    return None;
                }
                Some(id.clone())
            })
            .collect();
        for id in &expired {
            self.sync_output.remove(id);
            // Reset the mode, not just our latch: the next begin must start a
            // fresh hold even when its end/start pair shares one payload.
            if let Some(replica) = self.kernel.published_engine_mut(id)
                && let Err(error) = replica.release_render_hold()
            {
                tracing::warn!(terminal = %id, %error, "synchronized output watchdog release failed");
            }
            if let Err(error) = self.render_views(id) {
                tracing::warn!(terminal = %id, %error, "synchronized output watchdog projection failed");
            }
        }
        expired
    }
}
