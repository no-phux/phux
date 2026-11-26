//! Erasing predictive echo the server never answered.
//!
//! A guess is confirmed or contradicted when output repaints its view. A
//! program that echoes nothing (a password prompt) sends no output, so the
//! connection driver's deadline lane wakes the owner to repaint instead.

use std::time::{Duration, Instant};

use super::{Owner, Predictor, ResourceId, monotonic_epoch};

impl Owner {
    /// When the earliest guess on any screen stops being shown.
    pub(super) fn prediction_deadline(&self) -> Option<Instant> {
        self.presentations
            .values()
            .map(|presentation| &presentation.predictor)
            .chain(self.views.values().map(super::View::predictor))
            .filter_map(Predictor::display_deadline_ms)
            .min()
            // One past the deadline: the guess is still shown at it.
            .map(|ms| monotonic_epoch() + Duration::from_millis(ms.saturating_add(1)))
    }

    /// Repaint every terminal with a guess past its deadline at `now`, which
    /// drops the guess. Returns the terminals repainted.
    pub(super) fn expire_predictions(&mut self, now: Instant) -> Vec<ResourceId> {
        let now_ms = u64::try_from(now.saturating_duration_since(monotonic_epoch()).as_millis())
            .unwrap_or(u64::MAX);
        let mut expired: Vec<ResourceId> = self
            .presentations
            .iter_mut()
            .filter_map(|(id, presentation)| {
                presentation.predictor.expire(now_ms).then(|| id.clone())
            })
            .collect();
        for view in self.views.values_mut() {
            if view.predictor_mut().expire(now_ms) && !expired.contains(&view.terminal) {
                expired.push(view.terminal.clone());
            }
        }
        for id in &expired {
            if let Err(error) = self.render_views(id) {
                tracing::warn!(terminal = %id, %error, "expired prediction repaint failed");
            }
        }
        expired
    }
}
