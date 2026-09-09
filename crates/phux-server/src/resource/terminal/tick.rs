//! Submodule for terminal actor internals.

/// Default tick interval for the state-sync emission driver, used until a
/// consumer's RTT has been measured (phux-q0e.3, phux-q0e.5).
///
/// 30 ms ≈ 33 Hz; per ADR-0018 / `research/archive/2026-05-26-state-sync-algorithm.md`
/// §"tick scheduler" first-cut. Once a consumer's RTT is known the cadence
/// becomes RTT-adaptive (see [`adaptive_tick_interval`] and
/// [`RttEstimator`]); this value is the cold-start cadence before the first
/// `FRAME_ACK` round-trip lands, and the steady-state cadence on transports
/// (no-PTY test actors, never-acking peers) that produce no RTT samples.
pub const DEFAULT_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);

/// Lower clamp on the RTT-adaptive tick interval (phux-q0e.5).
///
/// 20 ms ≈ 50 Hz. Mosh (`research/archive/2026-05-26-state-sync-algorithm.md`
/// §"tick scheduler") clamps the `RTT/2` cadence to `[20 ms, 200 ms]`; we
/// adopt the same band. The floor is deliberately *below* the 30 ms cold-start
/// default so a near-zero local-UDS RTT clamps here (50 Hz) — snappier than,
/// and never slower than, today's fixed 33 Hz. High-RTT transports back off
/// toward the ceiling.
pub const MIN_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Upper clamp on the RTT-adaptive tick interval (phux-q0e.5).
///
/// 200 ms = 5 Hz. The Mosh ceiling: past this a high-RTT/satellite link is
/// shipping state nobody can ack in time, so we stop spending CPU + bandwidth
/// synthesizing diffs faster than the link can drain them.
pub const MAX_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// EMA smoothing factor for the per-consumer smoothed RTT (phux-q0e.5).
///
/// `srtt = (1 - α)·srtt + α·sample`. `α = 1/8 = 0.125` is TCP's RTO
/// estimator constant (RFC 6298 §2); it weights ~8 recent samples, so a
/// single spurious RTT spike nudges the cadence rather than yanking it.
/// "Adjust slowly" (Mosh §3) is exactly this slow convergence. The factor is
/// a documented default; real-traffic tuning (the ticket's deferred data) can
/// revisit it without touching the surrounding machinery.
pub const RTT_EMA_ALPHA: f64 = 0.125;

/// Smallest tick-interval change (in either direction) that triggers a
/// rebuild of the shared `tokio::time::Interval` (phux-q0e.5).
///
/// Rebuilding the shared timer on every sub-millisecond EMA wobble would churn
/// the scheduler for no observable benefit. A 5 ms deadband means the cadence
/// only re-arms on a meaningful RTT shift, and the steady state is stable.
pub(crate) const TICK_RESET_DEADBAND: std::time::Duration = std::time::Duration::from_millis(5);

/// Per-consumer smoothed round-trip-time estimator (phux-q0e.5, Mosh §3).
///
/// Feeds one RTT sample per `FRAME_ACK` (measured server-side as
/// `now − emit_instant` for the acked `seq`; no wire change — `seq` already
/// round-trips on `FRAME_ACK`) into a TCP-RTO-style EMA. The smoothed value
/// drives the adaptive tick cadence via [`adaptive_tick_interval`].
///
/// `None` smoothed value means "no sample yet": the consumer runs at the
/// [`DEFAULT_TICK_INTERVAL`] cold-start cadence and contributes that to the
/// shared-tick minimum (the actor takes the per-consumer minimum to drive
/// one shared timer).
#[derive(Debug, Clone, Copy, Default)]
pub struct RttEstimator {
    /// Smoothed RTT (`srtt`). `None` until the first sample lands.
    srtt: Option<std::time::Duration>,
}

impl RttEstimator {
    /// Fold one RTT `sample` into the smoothed estimate.
    ///
    /// First sample seeds `srtt` directly (RFC 6298 §2.2 initial assignment);
    /// later samples blend via `srtt = (1 − α)·srtt + α·sample` with
    /// `α` is [`RTT_EMA_ALPHA`]. Saturating, f64-internal math: a wild sample
    /// can only move `srtt` toward it, never panic or overflow.
    pub fn observe(&mut self, sample: std::time::Duration) {
        let sample_s = sample.as_secs_f64();
        let next = self.srtt.map_or(sample_s, |prev| {
            let prev_s = prev.as_secs_f64();
            RTT_EMA_ALPHA.mul_add(sample_s, (1.0 - RTT_EMA_ALPHA) * prev_s)
        });
        // `from_secs_f64` panics on negative/NaN/overflow; clamp the input to
        // a sane non-negative range first so a degenerate sample is inert.
        self.srtt = Some(std::time::Duration::from_secs_f64(next.clamp(0.0, 3600.0)));
    }

    /// The current smoothed RTT, or `None` if no sample has landed yet.
    #[must_use]
    pub const fn smoothed(&self) -> Option<std::time::Duration> {
        self.srtt
    }

    /// This consumer's desired tick interval: `clamp(srtt/2, MIN, MAX)`, or
    /// [`DEFAULT_TICK_INTERVAL`] while no sample exists. See
    /// [`adaptive_tick_interval`].
    #[must_use]
    pub fn desired_tick_interval(&self) -> std::time::Duration {
        self.srtt
            .map_or(DEFAULT_TICK_INTERVAL, adaptive_tick_interval)
    }
}

/// Map a smoothed RTT to a tick interval: `RTT/2` clamped to the
/// [`MIN_TICK_INTERVAL`]..=[`MAX_TICK_INTERVAL`] band (phux-q0e.5, Mosh §3).
///
/// Half-RTT is the Mosh target: a tick every half round-trip keeps the
/// emission cadence matched to how fast the consumer can actually ack. A
/// near-zero local RTT clamps to the 20 ms floor (50 Hz); a 400 ms satellite
/// RTT clamps to the 200 ms ceiling (5 Hz).
#[must_use]
pub fn adaptive_tick_interval(srtt: std::time::Duration) -> std::time::Duration {
    (srtt / 2).clamp(MIN_TICK_INTERVAL, MAX_TICK_INTERVAL)
}

/// Lower clamp on the loss-tolerant retransmit timeout (phux-v45.8).
///
/// A state-sync consumer on a lossy/forwarded leg (ADR-0042) advances its
/// reference on `FRAME_ACK`, not on emit, so an un-acked frame is retransmitted
/// (re-diffed against the last-acked reference) if no ack lands within a
/// retransmit window. This floors that window so a near-zero-RTT link does not
/// retransmit faster than roughly one idle tick.
pub const RETRANSMIT_MIN: std::time::Duration = std::time::Duration::from_millis(100);

/// Upper clamp on the loss-tolerant retransmit timeout (phux-v45.8).
///
/// Past this a high-RTT link is retransmitting state slower than useful; the
/// ceiling bounds how long a lost transition can sit unhealed on an otherwise
/// idle terminal.
pub const RETRANSMIT_MAX: std::time::Duration = std::time::Duration::from_millis(1000);

/// Cold-start retransmit timeout before any RTT sample exists (phux-v45.8).
pub const RETRANSMIT_DEFAULT: std::time::Duration = std::time::Duration::from_millis(250);

/// Retransmit-timeout (RTO) for a loss-tolerant consumer given its smoothed
/// RTT: `clamp(3·srtt, RETRANSMIT_MIN, RETRANSMIT_MAX)`, or
/// [`RETRANSMIT_DEFAULT`] while no sample exists (phux-v45.8, ADR-0042).
///
/// The `3·srtt` factor (vs the tick cadence's `srtt/2`) is deliberately
/// conservative: a retransmit is only for a *suspected loss*, so it waits
/// several RTTs to let a delayed ack land before re-spending bandwidth on a
/// re-diff. This is the "adjust slowly" discipline (Mosh §3) applied to the
/// loss path rather than the emission cadence.
#[must_use]
pub fn retransmit_timeout(srtt: Option<std::time::Duration>) -> std::time::Duration {
    srtt.map_or(RETRANSMIT_DEFAULT, |s| {
        (s * 3).clamp(RETRANSMIT_MIN, RETRANSMIT_MAX)
    })
}

/// Debounce window for the post-resize client resync (phux-8v1).
///
/// Dragging a terminal window fires a SIGWINCH storm — one
/// `VIEWPORT_RESIZE` (hence one resize) per step, many per second.
/// Broadcasting a full snapshot on each would flood the client with
/// snapshots synthesized at successive widths; one synthesized at width
/// N that lands on a mirror already resized to width M wraps/duplicates
/// rows. Instead we coalesce: arm a timer on each resync-requesting
/// resize and emit a single snapshot this long after the *last* one,
/// synthesized at the final settled size. 50 ms sits above per-SIGWINCH
/// cadence and below human settle perception.
pub const RESIZE_RESYNC_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(50);
