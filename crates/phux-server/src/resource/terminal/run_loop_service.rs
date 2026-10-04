//! Bounded service opportunities between ingress bursts. The ordinary select
//! arms still park/wake the idle actor; this walk never awaits or drains a queue.
//! Rotation prevents a busy snapshot client from starving ACKs or timers, while
//! the outer select retains cancellation/input/resize/bootstrap precedence.

use std::pin::Pin;

use futures_util::FutureExt;
use tokio::time::Sleep;

use super::{RunLoopState, TerminalActor};

const SERVICE_SLOTS: usize = 13;

#[derive(Default)]
pub(super) struct ServiceRotation {
    /// Ingress owes a service opportunity before its next turn.
    pub(super) due: bool,
    /// First slot inspected on the next opportunity.
    next: usize,
}

impl TerminalActor {
    pub(super) fn service_pending_turn(
        &mut self,
        state: &mut RunLoopState,
        mut resync_deadline: Pin<&mut Sleep>,
    ) {
        state.service.due = false;
        let bootstrap_pending = self.native_bootstrap_pending();
        for offset in 0..SERVICE_SLOTS {
            let slot = (state.service.next + offset) % SERVICE_SLOTS;
            let served = match slot {
                0..6 => !bootstrap_pending && self.try_snapshot_service(slot, state),
                6..10 => self.try_control_service(slot, state),
                _ => {
                    self.try_timer_service(slot, state, resync_deadline.as_mut(), bootstrap_pending)
                }
            };
            if served {
                state.service.next = (slot + 1) % SERVICE_SLOTS;
                return;
            }
        }
    }

    /// These requests access the canonical grid or its generation; a frozen
    /// native capture keeps the entire class gated, just like the idle select.
    fn try_snapshot_service(&mut self, slot: usize, state: &mut RunLoopState) -> bool {
        match slot {
            0 => self
                .snapshot_rx
                .try_recv()
                .map(|r| self.reply_bounded_snapshot(r))
                .is_ok(),
            1 => self
                .set_default_colors_rx
                .try_recv()
                .map(|r| self.install_client_default_colors(r))
                .is_ok(),
            2 => self
                .screen_rx
                .try_recv()
                .map(|r| self.reply_screen_state(r))
                .is_ok(),
            3 => self
                .upgrade_rx
                .try_recv()
                .map(|r| self.reply_upgrade_handle(r))
                .is_ok(),
            4 => self
                .consumer_attach_rx
                .try_recv()
                .map(|r| self.handle_consumer_attach(r))
                .is_ok(),
            5 => self
                .consumer_ack_rx
                .try_recv()
                .map(|r| self.service_frame_ack(&r, &mut state.tick, &mut state.tick_interval))
                .is_ok(),
            _ => unreachable!("snapshot service slot"),
        }
    }

    /// Metadata and supervisory requests remain usable during native capture.
    fn try_control_service(&mut self, slot: usize, state: &mut RunLoopState) -> bool {
        match slot {
            6 => self
                .pwd_rx
                .try_recv()
                .map(|r| self.reply_pane_cwd(r))
                .is_ok(),
            7 => self
                .process_rx
                .try_recv()
                .map(|r| self.reply_process_facet(r))
                .is_ok(),
            8 => self
                .consumer_detach_rx
                .try_recv()
                .map(|r| self.service_consumer_detach(r, &mut state.tick, &mut state.tick_interval))
                .is_ok(),
            9 => self
                .core
                .control_rx
                .try_recv()
                .map(|r| self.handle_control_request(r))
                .is_ok(),
            _ => unreachable!("control service slot"),
        }
    }

    fn try_timer_service(
        &mut self,
        slot: usize,
        state: &mut RunLoopState,
        deadline: Pin<&mut Sleep>,
        bootstrap_pending: bool,
    ) -> bool {
        if bootstrap_pending {
            return false;
        }
        match slot {
            10 if state.resync.pending => deadline
                .now_or_never()
                .map(|()| self.fire_owed_resync(&mut state.resync))
                .is_some(),
            11 if self.state_tick_armed() => state
                .tick
                .tick()
                .now_or_never()
                .map(|_| self.service_state_tick())
                .is_some(),
            12 if self.detector_tick_armed(false) => state
                .detect_tick
                .tick()
                .now_or_never()
                .map(|scheduled| self.service_detector_deadline(scheduled, state))
                .is_some(),
            _ => false,
        }
    }

    pub(super) fn service_detector_deadline(
        &mut self,
        scheduled: tokio::time::Instant,
        state: &mut RunLoopState,
    ) {
        if state.detect_deadline_valid {
            crate::perf::RUNTIME_DETECT_TICK_LATE.record_duration(scheduled.elapsed());
        }
        state.detect_deadline_valid = true;
        self.service_detect_tick(&mut state.detect_tick, &mut state.detect_interval);
    }
}
