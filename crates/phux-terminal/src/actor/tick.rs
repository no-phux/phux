//! Submodule for terminal actor internals.

/// Cold-start state-sync tick interval (~33 Hz), used until a consumer's RTT
/// is measured.
pub const DEFAULT_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);

/// Lower clamp on the adaptive tick interval (50 Hz); Mosh's band is
/// `[20 ms, 200 ms]`.
pub const MIN_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Upper clamp on the adaptive tick interval (5 Hz).
pub const MAX_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// RTT EMA factor, `srtt = (1 - α)·srtt + α·sample` (RFC 6298's 1/8), so one
/// spike nudges rather than yanks the cadence.
pub const RTT_EMA_ALPHA: f64 = 0.125;

/// Smallest cadence change that rebuilds the shared timer.
pub(crate) const TICK_RESET_DEADBAND: std::time::Duration = std::time::Duration::from_millis(5);

/// Per-consumer smoothed RTT (Mosh §3), one sample per `FRAME_ACK`. `None`
/// until the first sample, when the consumer runs at
/// [`DEFAULT_TICK_INTERVAL`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RttEstimator {
    /// Smoothed RTT (`srtt`). `None` until the first sample lands.
    srtt: Option<std::time::Duration>,
}

impl RttEstimator {
    /// Fold one sample in: the first seeds `srtt`, later ones blend by
    /// [`RTT_EMA_ALPHA`].
    pub fn observe(&mut self, sample: std::time::Duration) {
        let sample_s = sample.as_secs_f64();
        let next = self.srtt.map_or(sample_s, |prev| {
            let prev_s = prev.as_secs_f64();
            RTT_EMA_ALPHA.mul_add(sample_s, (1.0 - RTT_EMA_ALPHA) * prev_s)
        });
        // `from_secs_f64` panics on bad input; clamp first.
        self.srtt = Some(std::time::Duration::from_secs_f64(next.clamp(0.0, 3600.0)));
    }

    /// The current smoothed RTT, or `None` if no sample has landed yet.
    #[must_use]
    pub const fn smoothed(&self) -> Option<std::time::Duration> {
        self.srtt
    }

    /// Desired tick interval: `clamp(srtt/2, MIN, MAX)`, or the default
    /// without a sample.
    #[must_use]
    pub fn desired_tick_interval(&self) -> std::time::Duration {
        self.srtt
            .map_or(DEFAULT_TICK_INTERVAL, adaptive_tick_interval)
    }
}

/// `RTT/2` clamped to [`MIN_TICK_INTERVAL`]..=[`MAX_TICK_INTERVAL`].
#[must_use]
pub fn adaptive_tick_interval(srtt: std::time::Duration) -> std::time::Duration {
    (srtt / 2).clamp(MIN_TICK_INTERVAL, MAX_TICK_INTERVAL)
}

/// Lower clamp on the loss-tolerant retransmit timeout (ADR-0042).
pub const RETRANSMIT_MIN: std::time::Duration = std::time::Duration::from_millis(100);

/// Upper clamp on the loss-tolerant retransmit timeout.
pub const RETRANSMIT_MAX: std::time::Duration = std::time::Duration::from_millis(1000);

/// Cold-start retransmit timeout before any RTT sample exists (phux-v45.8).
pub const RETRANSMIT_DEFAULT: std::time::Duration = std::time::Duration::from_millis(250);

/// Retransmit timeout for a loss-tolerant consumer: `clamp(3·srtt, MIN,
/// MAX)`, or [`RETRANSMIT_DEFAULT`] without a sample. Several RTTs, so a
/// late ack can land before bandwidth is spent on a re-diff.
#[must_use]
pub fn retransmit_timeout(srtt: Option<std::time::Duration>) -> std::time::Duration {
    srtt.map_or(RETRANSMIT_DEFAULT, |s| {
        (s * 3).clamp(RETRANSMIT_MIN, RETRANSMIT_MAX)
    })
}

/// Debounce for the post-resize resync: a window drag fires many resizes,
/// and one snapshot at the settled size replaces per-step snapshots that
/// would land on mismatched mirrors.
pub const RESIZE_RESYNC_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(50);
