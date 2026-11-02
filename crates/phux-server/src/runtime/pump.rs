//! Shared per-generation state for the pane output pumps (the ATTACH pump in
//! [`crate::runtime::apps::terminal::attach`] and the `ATTACH_RESOURCE` pump in
//! [`crate::runtime::commands`]).
//!
//! [`PumpGeneration`] keeps its flags private so no pump can put a live delta
//! on the wire without asking [`PumpGeneration::forwards`].

use std::time::Duration;

use phux_protocol::ids::BootstrapId;
use phux_protocol::wire::frame::FrameKind;
use tokio::sync::mpsc::error::TrySendError;

use crate::resource::{PaneOutput, ResyncAudience, ResyncTarget};
use crate::state::Outbound;

/// Outcome of offering one live frame to a consumer mailbox without parking.
pub(super) enum MailboxForward {
    /// The frame is on the mailbox.
    Sent,
    /// The consumer went away.
    Closed,
    /// The mailbox is full: parking would keep the pump off the broadcast
    /// and could lose a fenced resync.
    Full,
}

/// Offer `frame` to `out_tx` without waiting for capacity.
pub(super) fn try_send_frame(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    frame: FrameKind,
) -> MailboxForward {
    match out_tx.try_send(Outbound::Frame(frame)) {
        Ok(()) => MailboxForward::Sent,
        Err(TrySendError::Closed(_)) => MailboxForward::Closed,
        Err(TrySendError::Full(_)) => MailboxForward::Full,
    }
}

/// Spawn an owned output task from any subscription path. Completion guards
/// are captured before spawning, so even abort-before-first-poll resolves the
/// detach fence. A `JoinSet` owner additionally retains whole-session teardown.
pub(super) fn spawn_tracked(
    state: &crate::state::SharedState,
    client: crate::state::ClientId,
    terminal: phux_core::ResourceId,
    tasks: Option<&mut tokio::task::JoinSet<()>>,
    drain: Option<tokio_util::sync::CancellationToken>,
    future: impl std::future::Future<Output = ()> + 'static,
) {
    let done = tokio_util::sync::CancellationToken::new();
    let guard = done.clone().drop_guard();
    let task = async move {
        let _guard = guard;
        future.await;
    };
    let abort = match tasks {
        Some(tasks) => {
            // Completed SPAWN pumps must not accumulate in the connection's
            // JoinSet during create/detach churn.
            while tasks.try_join_next().is_some() {}
            tasks.spawn_local(task)
        }
        None => tokio::task::spawn_local(task).abort_handle(),
    };
    state.with_mut(|s| s.track_terminal_output_pump(client, terminal, abort, done, drain));
}

/// Stop all output tasks for a subscription, including tasks blocked inside
/// bootstrap publication or a send to a full consumer mailbox.
pub(super) async fn stop_output(
    state: &crate::state::SharedState,
    client: crate::state::ClientId,
    terminal: phux_core::ResourceId,
) {
    let completions = state.with_mut(|s| s.stop_terminal_output_pumps(client, terminal));
    for done in completions {
        done.cancelled().await;
    }
}

/// First wait for a replacement generation before asking again: well above
/// the actor's resync debounce, so a coalescing resync never looks lost.
const GAP_RESYNC_RETRY: Duration = Duration::from_millis(500);
const _: () = assert!(
    GAP_RESYNC_RETRY.as_millis() >= crate::resource::RESIZE_RESYNC_DEBOUNCE.as_millis() * 4
);

/// Ceiling on the doubling retry backoff, which keeps many fenced pumps on
/// one pane from hammering the actor's debounce in lockstep.
const GAP_RESYNC_MAX_BACKOFF: Duration = Duration::from_secs(4);

/// Resync requests one gap gets before it is unrecoverable (~11.5s): an actor
/// that never answers ends in a terminal `ERROR`, not a frozen screen.
const GAP_RESYNC_MAX_ATTEMPTS: u32 = 5;

/// How old (since the later of the PTY read and the generation's publication)
/// a chunk may be and still be forwarded. The broadcast's own `Lagged` fires
/// only hundreds of kilobytes behind; a consumer slower than this gets one
/// fresh screen instead of the backlog.
pub(super) const STALE_OUTPUT_BUDGET: Duration = Duration::from_millis(250);

/// Has a chunk read `age` ago fallen past [`STALE_OUTPUT_BUDGET`]?
pub(super) fn is_stale(age: Duration) -> bool {
    age > STALE_OUTPUT_BUDGET
}

/// Where one pump has got to inside the generation it is publishing, and
/// whether that generation may still carry live output.
#[derive(Debug)]
pub(super) struct PumpGeneration {
    /// Highest raw sequence already covered by the published bootstrap.
    published_cut: u64,
    /// Highest raw sequence actually forwarded to the consumer.
    last_forwarded_seq: u64,
    /// Generation every frame this pump emits is labelled with.
    bootstrap_id: BootstrapId,
    /// Cleared by a tombstone and set again once a replacement bootstrap is
    /// published; nothing may be forwarded in between.
    generation_active: bool,
    /// Set when the broadcast drops a window under this pump, cleared when
    /// the replacement generation is published. While set nothing is
    /// forwarded: a skipped `seq` is a `SequenceGap` the client detaches on,
    /// and draining at memory speed is what lets the requested resync arrive.
    gap_pending: bool,
    /// Resync requests already spent on the current gap; reset when a
    /// replacement generation lands. Bounded by [`GAP_RESYNC_MAX_ATTEMPTS`].
    gap_attempts: u32,
    /// Absolute response deadline. Unrelated broadcast traffic must not restart
    /// the retry clock and keep a fenced consumer frozen indefinitely.
    gap_deadline: Option<tokio::time::Instant>,
    /// When the current generation was published (opened or republished):
    /// the earliest instant [`Self::chunk_age`] measures from.
    published_at: std::time::Instant,
}

impl PumpGeneration {
    /// Start at the cut the publication gate handed over.
    pub(super) fn opened_at(published_cut: u64, bootstrap_id: BootstrapId) -> Self {
        Self {
            published_cut,
            last_forwarded_seq: published_cut,
            bootstrap_id,
            generation_active: true,
            gap_pending: false,
            gap_attempts: 0,
            gap_deadline: None,
            published_at: std::time::Instant::now(),
        }
    }

    /// How stale a chunk read at `read_at` is: the time since the later of
    /// that read and this generation's publication, since waiting behind the
    /// bootstrap is not the consumer's fault.
    pub(super) fn chunk_age(&self, read_at: std::time::Instant) -> Duration {
        std::time::Instant::now().saturating_duration_since(read_at.max(self.published_at))
    }

    /// The generation label every frame this pump emits carries.
    pub(super) const fn bootstrap_id(&self) -> BootstrapId {
        self.bootstrap_id
    }

    /// Adopt the next generation label ahead of republishing.
    pub(super) const fn set_bootstrap_id(&mut self, bootstrap_id: BootstrapId) {
        self.bootstrap_id = bootstrap_id;
    }

    /// Highest raw sequence actually forwarded, for a tombstone's
    /// `last_valid_seq`.
    pub(super) const fn last_forwarded_seq(&self) -> u64 {
        self.last_forwarded_seq
    }

    /// Is a generation currently published and unretired?
    pub(super) const fn is_active(&self) -> bool {
        self.generation_active
    }

    /// Is this pump waiting on a replacement generation after a gap?
    pub(super) const fn is_fenced(&self) -> bool {
        self.gap_pending
    }

    /// May this live delta go on the wire? Not when retired, fenced, or
    /// already covered by the published bootstrap.
    pub(super) const fn forwards(&self, seq: u64) -> bool {
        self.generation_active && !self.gap_pending && seq > self.published_cut
    }

    /// Record a delta that reached the consumer.
    pub(super) const fn note_forwarded(&mut self, seq: u64) {
        self.last_forwarded_seq = seq;
    }

    /// A tombstone retired the published generation; nothing may be forwarded
    /// until a replacement is published.
    pub(super) const fn retire(&mut self) {
        self.generation_active = false;
    }

    /// Fence the generation before asking for a resync; returns whether one
    /// was already in flight.
    pub(super) fn fence_for_gap(&mut self) -> bool {
        let already_pending = self.gap_pending;
        self.gap_pending = true;
        if !already_pending {
            self.gap_deadline = Some(tokio::time::Instant::now() + GAP_RESYNC_RETRY);
        }
        already_pending
    }

    /// Record that a resync request went out for the current gap.
    pub(super) fn note_resync_requested(&mut self) {
        if self.gap_attempts >= GAP_RESYNC_MAX_ATTEMPTS {
            return;
        }
        if self.gap_attempts != 0
            && self
                .gap_deadline
                .is_some_and(|deadline| tokio::time::Instant::now() < deadline)
        {
            return;
        }
        self.gap_attempts = self.gap_attempts.saturating_add(1);
        self.gap_deadline = Some(tokio::time::Instant::now() + self.gap_retry_delay());
    }

    /// How long to wait for a response: doubles from [`GAP_RESYNC_RETRY`] to
    /// [`GAP_RESYNC_MAX_BACKOFF`].
    fn gap_retry_delay(&self) -> Duration {
        let step = self.gap_attempts.saturating_sub(1).min(u32::BITS - 1);
        GAP_RESYNC_RETRY
            .saturating_mul(1_u32 << step)
            .min(GAP_RESYNC_MAX_BACKOFF)
    }

    const fn gap_deadline_elapsed(&self) -> PumpWait {
        if self.gap_attempts >= GAP_RESYNC_MAX_ATTEMPTS {
            PumpWait::GapUnrecoverable
        } else {
            PumpWait::RetryResync
        }
    }

    /// Resync requests this gap has already cost.
    pub(super) const fn gap_attempts(&self) -> u32 {
        self.gap_attempts
    }

    /// Does a [`PaneOutput::Resync`] addressed to `audience` replace this
    /// pump's generation? One for everyone (a reflow) always does. One naming
    /// pumps replaces only a named pump that is still fenced or retired, so
    /// every other consumer of the pane keeps its generation; a pump retired
    /// by its own client's reattach must never be revived by someone else's.
    pub(super) fn takes_resync(&self, audience: &ResyncAudience, pump: ResyncTarget) -> bool {
        match audience {
            ResyncAudience::Everyone => true,
            ResyncAudience::Only(_) => {
                (self.gap_pending || !self.generation_active) && audience.includes(pump)
            }
        }
    }

    /// Restart the staleness clock once a bootstrap and replay are handed off.
    pub(super) fn restart_staleness_clock(&mut self) {
        self.published_at = std::time::Instant::now();
    }

    /// A replacement generation is published at `base_seq`: unfence, reactivate
    /// and re-anchor the sequence expectation.
    pub(super) fn republished_at(&mut self, base_seq: u64) {
        self.published_cut = base_seq;
        self.last_forwarded_seq = base_seq;
        self.generation_active = true;
        self.gap_pending = false;
        self.gap_attempts = 0;
        self.gap_deadline = None;
        self.published_at = std::time::Instant::now();
    }
}

/// What one turn of waiting on the pane's broadcast produced.
pub(super) enum PumpWait {
    /// Natural close requests a fresh final capture before ending the pump.
    FinalDrain,
    /// Dispatch this broadcast result.
    Event(Result<PaneOutput, tokio::sync::broadcast::error::RecvError>),
    /// A fenced pump's backoff elapsed with no replacement generation: ask
    /// again.
    RetryResync,
    /// The gap spent its whole request budget without an answer.
    GapUnrecoverable,
}

/// The next broadcast event for a pump; a fenced pump's wait is bounded by
/// its retry deadline. `recv` is cancel-safe, so the timeout drops nothing.
pub(super) async fn next_event(
    generation: &PumpGeneration,
    output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
) -> PumpWait {
    if !generation.is_fenced() {
        return PumpWait::Event(output_rx.recv().await);
    }
    let Some(deadline) = generation.gap_deadline else {
        return PumpWait::Event(output_rx.recv().await);
    };
    // timeout_at polls its inner future first; a perpetually ready broadcast
    // could otherwise win even after the absolute deadline has expired.
    if tokio::time::Instant::now() >= deadline {
        return generation.gap_deadline_elapsed();
    }
    let Ok(received) = tokio::time::timeout_at(deadline, output_rx.recv()).await else {
        return generation.gap_deadline_elapsed();
    };
    PumpWait::Event(received)
}

#[cfg(test)]
mod tests;
