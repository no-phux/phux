//! The monotone repaint accumulator (ADR-0029 §2) and the frame pacer.
//!
//! Loop-level triggers RAISE a level during an iteration and the loop DRAINS
//! it exactly once, so N triggers (a burst of agent-metadata broadcasts, say)
//! collapse into one paint at the highest requested level instead of N
//! full-screen clears. [`RepaintLevel`] orders by declaration, so a raise is a
//! monotone `max`. [`PaintPacer`] is the same idea across iterations: paints
//! landing inside one frame interval collapse into one.

use std::time::Duration;

/// How much of the frame a drained repaint must redraw, cheapest first.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RepaintLevel {
    /// No trigger fired this iteration.
    #[default]
    None,
    /// In-place chrome only (sidebar strip + status bar): no `ED2`, no pane
    /// interior, and unchanged painters emit nothing.
    Chrome,
    /// `ED2` + every pane + dividers + chrome, for when pane rects moved.
    Full,
}

/// One iteration's accumulated repaint intent. Triggers raise; the loop
/// drains once, so "two triggers paint twice" is not representable.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct RepaintAccumulator {
    /// The highest level any trigger raised.
    pub(super) level: RepaintLevel,
    /// An open agent-fleet dashboard must re-project. Its refresh repaints
    /// over a full frame, so it obeys the same raise-then-drain-once rule.
    pub(super) fleet_dirty: bool,
}

impl RepaintAccumulator {
    /// Request an in-place chrome repaint.
    pub(super) const fn raise_chrome(&mut self) {
        if matches!(self.level, RepaintLevel::None) {
            self.level = RepaintLevel::Chrome;
        }
    }

    /// Request a full-viewport repaint; always wins over chrome.
    pub(super) const fn raise_full(&mut self) {
        self.level = RepaintLevel::Full;
    }

    /// Request a re-projection of the agent-fleet dashboard.
    pub(super) const fn raise_fleet(&mut self) {
        self.fleet_dirty = true;
    }

    /// The accumulated work, resetting to default. Called once per iteration.
    pub(super) fn drain(&mut self) -> Self {
        std::mem::take(self)
    }
}

/// Default minimum interval between composited frames: one frame at 60Hz.
/// A floor on the gap between paints, never a delay after a lull.
const DEFAULT_FRAME_INTERVAL_MS: u64 = 16;

/// `PHUX_FRAME_INTERVAL_MS` overrides the pacing floor; `0` disables pacing.
fn frame_interval() -> Duration {
    static CACHED: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let ms = std::env::var("PHUX_FRAME_INTERVAL_MS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_FRAME_INTERVAL_MS);
        Duration::from_millis(ms)
    })
}

/// The frame-rate governor for pane output.
///
/// A producer whose lines arrive one wake-up apart would otherwise cost one
/// full repaint per line. The first frame after a lull paints immediately (no
/// added latency); frames inside the window still reach the mirrors, but
/// their panes are remembered and settled in ONE composited frame when the
/// window expires. Output answering the user's own input is never paced.
#[derive(Debug, Default)]
pub(super) struct PaintPacer {
    /// Earliest instant the next frame may be emitted; `None` before any paint.
    next_allowed: Option<tokio::time::Instant>,
    /// Panes owing a settle paint, deduplicated (one entry per visible pane).
    pending: Vec<phux_protocol::ids::ResourceId>,
    /// The pane the user's last input went to. Pane-keyed on purpose: typing
    /// in one pane must not un-pace a flood in another.
    last_input: Option<InputMark>,
    /// Smoothed input-to-first-output latency the grace has to cover.
    reply_rtt: Duration,
}

/// One input batch, and the reply still owed to it.
#[derive(Debug, Clone)]
struct InputMark {
    /// The focused pane after dispatch.
    pane: phux_protocol::ids::ResourceId,
    /// When the batch went out.
    at: tokio::time::Instant,
    /// Whether this batch's reply was already sampled (a multi-frame reply
    /// counts once).
    sampled: bool,
}

/// Floor for the input grace: covers a fragmented reply on a local socket.
const BASE_INPUT_GRACE: Duration = Duration::from_millis(20);

/// Ceiling for the input grace however slow the link.
const MAX_INPUT_GRACE: Duration = Duration::from_millis(250);

/// Largest reply latency still treated as a round trip rather than a slow
/// command.
const MAX_RTT_SAMPLE: Duration = Duration::from_millis(500);

/// `PHUX_INPUT_GRACE_MS` pins the input grace; `0` paces every frame.
fn input_grace_override() -> Option<Duration> {
    static CACHED: std::sync::OnceLock<Option<Duration>> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("PHUX_INPUT_GRACE_MS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
    })
}

impl PaintPacer {
    /// Whether a composited frame may be emitted at `now`, arming the next
    /// window if so. Refused callers [`Self::withhold`] their panes. A reply
    /// (from [`Self::observe_reply`]) bypasses the window and restarts it.
    pub(super) fn admit(&mut self, now: tokio::time::Instant, is_reply: bool) -> bool {
        if frame_interval().is_zero() {
            return true;
        }
        if is_reply {
            super::render_prof::note_paced_replies(1);
            self.next_allowed = Some(now + frame_interval());
            return true;
        }
        if self.next_allowed.is_some_and(|at| now < at) {
            super::render_prof::note_paced_waits(1);
            return false;
        }
        self.next_allowed = Some(now + frame_interval());
        true
    }

    /// Whether a burst touching `panes` answers the user's last input, taking
    /// the round-trip sample on the first matching burst. The mark survives a
    /// match (a reply may span frames); only time expires it.
    pub(super) fn observe_reply<'a>(
        &mut self,
        now: tokio::time::Instant,
        panes: impl IntoIterator<Item = &'a phux_protocol::ids::ResourceId>,
    ) -> bool {
        let Some(mark) = self.last_input.as_ref() else {
            return false;
        };
        let (pane, at, sampled) = (mark.pane.clone(), mark.at, mark.sampled);
        let elapsed = now.saturating_duration_since(at);
        if !panes.into_iter().any(|id| *id == pane) {
            // Drop a mark too old for its reply to still be coming.
            if elapsed > MAX_RTT_SAMPLE {
                self.last_input = None;
            }
            return false;
        }
        // Sample BEFORE checking the grace: a reply that missed the grace is
        // the evidence the grace is too small, and skipping it would pin a
        // cold estimator on a slow link at the floor forever.
        if !sampled {
            phux_client::perf::ECHO_RTT.record_duration(elapsed);
            self.note_reply_latency(elapsed);
            if let Some(mark) = self.last_input.as_mut() {
                mark.sampled = true;
            }
        }
        let graced = elapsed < self.grace();
        if !graced {
            self.last_input = None;
        }
        graced
    }

    /// Note input just went to `pane` (the focused pane after dispatch), so
    /// its output is a reply. `None` arms nothing.
    pub(super) fn note_input(
        &mut self,
        pane: Option<&phux_protocol::ids::ResourceId>,
        now: tokio::time::Instant,
    ) {
        let Some(pane) = pane else {
            return;
        };
        self.last_input = Some(InputMark {
            pane: pane.clone(),
            at: now,
            sampled: false,
        });
    }

    /// How long output from the marked pane counts as a reply:
    /// `2 x measured RTT`, clamped to `[20ms, 250ms]`. Measured, because a
    /// fixed 20ms stops covering any echo over a remote link.
    fn grace(&self) -> Duration {
        if let Some(pinned) = input_grace_override() {
            return pinned;
        }
        self.reply_rtt
            .saturating_mul(2)
            .clamp(BASE_INPUT_GRACE, MAX_INPUT_GRACE)
    }

    /// Fold one sample: fast attack, slow (7/8) decay; slow commands past
    /// [`MAX_RTT_SAMPLE`] are ignored.
    fn note_reply_latency(&mut self, sample: Duration) {
        if sample > MAX_RTT_SAMPLE {
            return;
        }
        self.reply_rtt = if sample > self.reply_rtt {
            sample
        } else {
            (self.reply_rtt.saturating_mul(7) + sample) / 8
        };
    }

    /// Start the pacing window at `now` (from every path that paints); the
    /// input grace is untouched.
    pub(super) fn rearm(&mut self, now: tokio::time::Instant) {
        self.next_allowed = Some(now + frame_interval());
    }

    /// Remember that `terminal_id` owes a settle paint.
    pub(super) fn withhold(&mut self, terminal_id: &phux_protocol::ids::ResourceId) {
        if !self.pending.iter().any(|id| id == terminal_id) {
            self.pending.push(terminal_id.clone());
        }
    }

    /// When the driver must wake to settle withheld panes; `None` (no timer)
    /// when nothing is owed.
    pub(super) const fn deadline(&self) -> Option<tokio::time::Instant> {
        if self.pending.is_empty() {
            None
        } else {
            self.next_allowed
        }
    }

    /// Take the panes owed a settle paint, clearing the debt.
    pub(super) fn take_pending(&mut self) -> Vec<phux_protocol::ids::ResourceId> {
        std::mem::take(&mut self.pending)
    }

    /// Forget every withheld pane: a full repaint just redrew them all.
    pub(super) fn clear_pending(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use phux_protocol::ids::ResourceId;

    fn pane(id: u32) -> ResourceId {
        ResourceId::Local { id }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The first frame after a lull paints immediately; frames inside the
    /// window are refused until it expires.
    #[test]
    fn the_window_admits_after_a_lull_and_refuses_inside() {
        let mut pacer = PaintPacer::default();
        let t0 = tokio::time::Instant::now();
        let interval = frame_interval();
        assert!(pacer.admit(t0, false));
        assert!(!pacer.admit(t0 + interval / 2, false));
        assert!(pacer.admit(t0 + interval, false));
        assert!(pacer.admit(t0 + Duration::from_secs(1), false));
    }

    /// Withheld panes deduplicate, arm the window's end as the deadline, and
    /// drain once; a full repaint discharges them; an idle pacer arms nothing.
    #[test]
    fn withheld_panes_deduplicate_arm_the_deadline_and_drain_once() {
        let mut pacer = PaintPacer::default();
        assert_eq!(pacer.deadline(), None);
        let t0 = tokio::time::Instant::now();
        assert!(pacer.admit(t0, false));
        assert_eq!(pacer.deadline(), None, "a frame that painted owes nothing");
        for _ in 0..40 {
            pacer.withhold(&pane(1));
        }
        pacer.withhold(&pane(2));
        assert_eq!(pacer.deadline(), Some(t0 + frame_interval()));
        assert_eq!(pacer.take_pending(), vec![pane(1), pane(2)]);
        assert!(pacer.take_pending().is_empty());

        pacer.withhold(&pane(1));
        pacer.clear_pending();
        assert_eq!(pacer.deadline(), None);
    }

    /// A reply bypasses a window that would refuse (p99 echo once sat at one
    /// frame interval); the grace is pane-keyed, so a flood elsewhere stays
    /// paced; with no focused pane nothing is armed.
    #[test]
    fn a_reply_bypasses_the_window_only_for_its_own_pane() {
        let (flood, typed) = (pane(1), pane(2));
        let mut pacer = PaintPacer::default();
        let t0 = tokio::time::Instant::now();
        assert!(pacer.admit(t0, false));
        let mid = t0 + frame_interval() / 2;
        pacer.note_input(Some(&typed), mid);
        assert!(!pacer.observe_reply(mid, [&flood]));
        assert!(!pacer.admit(mid, false), "a flood elsewhere keeps waiting");
        assert!(pacer.observe_reply(mid, [&typed]));
        assert!(pacer.admit(mid, true), "the user's own echo paints now");

        let mut pacer = PaintPacer::default();
        pacer.note_input(None, t0);
        assert!(!pacer.observe_reply(t0, [&pane(1)]));
    }

    /// The grace covers a reply split across frames (and survives a `rearm`),
    /// then expires, so one keystroke never buys a standing exemption.
    #[test]
    fn the_grace_spans_a_multi_frame_reply_then_expires() {
        let mut pacer = PaintPacer::default();
        let t0 = tokio::time::Instant::now();
        pacer.note_input(Some(&pane(1)), t0);
        pacer.rearm(t0);
        for delay in [0, 1, 5] {
            assert!(
                pacer.observe_reply(t0 + ms(delay), [&pane(1)]),
                "+{delay}ms"
            );
            assert!(pacer.admit(t0 + ms(delay), true));
        }
        let after = t0 + BASE_INPUT_GRACE + ms(1);
        assert!(!pacer.observe_reply(after, [&pane(1)]));
        assert!(pacer.admit(after, false));
        assert!(!pacer.admit(after + frame_interval() / 2, false));
    }

    /// The grace sizes itself to the link: the first (late) reply on a 60ms
    /// link is sampled and still counts, and later echoes are covered.
    #[test]
    fn the_grace_grows_to_a_measured_round_trip() {
        let mut pacer = PaintPacer::default();
        assert_eq!(pacer.grace(), BASE_INPUT_GRACE);
        let t0 = tokio::time::Instant::now();
        pacer.note_input(Some(&pane(1)), t0);
        let late = t0 + ms(60);
        assert!(pacer.observe_reply(late, [&pane(1)]));
        assert_eq!(pacer.grace(), ms(120));
        pacer.note_input(Some(&pane(1)), late);
        assert!(pacer.observe_reply(late + ms(60), [&pane(1)]));
    }

    /// The estimate is clamped at both ends, ignores slow commands, attacks
    /// fast, and decays slowly.
    #[test]
    fn the_rtt_estimate_is_clamped_robust_and_asymmetric() {
        let mut pacer = PaintPacer::default();
        pacer.note_reply_latency(Duration::from_micros(200));
        assert_eq!(pacer.grace(), BASE_INPUT_GRACE);
        pacer.note_reply_latency(ms(400));
        assert_eq!(pacer.grace(), MAX_INPUT_GRACE);

        let mut pacer = PaintPacer::default();
        pacer.note_reply_latency(ms(80));
        assert_eq!(pacer.grace(), ms(160), "attack is immediate");
        pacer.note_reply_latency(MAX_RTT_SAMPLE + ms(1));
        assert_eq!(pacer.grace(), ms(160), "a slow command is ignored");
        pacer.note_reply_latency(ms(0));
        assert!(
            pacer.grace() > ms(120),
            "decay is gradual: {:?}",
            pacer.grace()
        );
    }

    /// Raises are monotone, order-independent, and idempotent (twenty chrome
    /// raises drain as one chrome paint); a fleet raise composes with a level
    /// and survives exactly one drain.
    #[test]
    fn raises_collapse_into_one_drain() {
        assert!(
            RepaintLevel::None < RepaintLevel::Chrome && RepaintLevel::Chrome < RepaintLevel::Full
        );
        let (mut a, mut b) = (RepaintAccumulator::default(), RepaintAccumulator::default());
        a.raise_full();
        a.raise_chrome();
        b.raise_chrome();
        b.raise_full();
        assert_eq!(a.drain(), b.drain());

        let mut accum = RepaintAccumulator::default();
        for _ in 0..20 {
            accum.raise_chrome();
            accum.raise_fleet();
        }
        let drained = accum.drain();
        assert_eq!(
            (drained.level, drained.fleet_dirty),
            (RepaintLevel::Chrome, true)
        );
        assert_eq!(accum.drain(), RepaintAccumulator::default());
    }
}
