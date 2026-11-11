//! Keep UIKit's keyboard/layout round-trips off the PTY. Drawing and input
//! use the current layout immediately; only a changed grid proposal waits.

use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::super::{Desk, Fitted};

/// Quiet time after the last *grid* change, not the last layout callback.
const SETTLE: f64 = 0.12;

#[derive(Debug, Clone)]
pub(super) struct Refit {
    terminal: ResourceId,
    size: Fitted,
    after: f64,
}

impl Desk {
    /// A native surface changed before Rune has laid it out. Forget the old
    /// measurement so poll cannot send it while the new layout is queued.
    pub(crate) fn phone_viewport_changed(&mut self) {
        self.areas.clear();
        if let Some(phone) = self.phone.as_mut() {
            phone.refit = None;
        }
    }

    /// First fits are immediate. A changed proposal waits until the phone's
    /// viewport settles; returning to the last sent size cancels the wait.
    pub(in crate::desk) fn defer_phone_fit(&mut self, terminal: &ResourceId, size: Fitted) -> bool {
        let Some(phone) = self.phone.as_mut() else {
            return false;
        };
        let previous = self.fitted.get(terminal);
        if previous.is_none() || previous == Some(&size) {
            phone.refit = None;
            return false;
        }
        if let Some(refit) = &phone.refit
            && &refit.terminal == terminal
            && refit.size == size
        {
            return self.clock < refit.after;
        }
        phone.refit = Some(Refit {
            terminal: terminal.clone(),
            size,
            after: self.clock + SETTLE,
        });
        // on_layout runs after poll and has no Tx. Post another frame so
        // poll arms the deadline even when nothing else is animating.
        if let Some(waker) = &self.waker {
            waker();
        }
        true
    }

    pub(in crate::desk) fn wake_phone_fit(&self, tx: &mut Tx) {
        let Some(refit) = self.phone.as_ref().and_then(|phone| phone.refit.as_ref()) else {
            return;
        };
        let left = refit.after - self.clock;
        if left > 0.0 {
            tx.wake_after(Duration::from_secs_f64(left));
        }
    }
}
