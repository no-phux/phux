//! The visual bell's flash safety.
//!
//! A BEL washes the canvas in `status.bell` for `motion.curve.fade`, at most
//! once per second however fast a program rings, well under WCAG 2.3.1's
//! three flashes per second. Under reduce motion there is no wash at all:
//! the bell shows as an unread dot instead.

use phux_design::tokens;

/// The least time between two washes, in milliseconds.
pub const MIN_INTERVAL_MS: f64 = 1_000.0;

/// How long one wash shows, in milliseconds: `motion.curve.fade`.
#[must_use]
pub fn wash_ms() -> u32 {
    let ms = (f64::from(tokens::MOTION.curve.fade.duration) * 1_000.0).round();
    // A token duration is a fraction of a second.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = ms.clamp(1.0, MIN_INTERVAL_MS) as u32;
    ms
}

/// What one ring shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ring {
    /// Flash the wash now.
    Wash,
    /// Reduce motion: mark the bell unread instead of flashing.
    Dot,
    /// A wash showed less than [`MIN_INTERVAL_MS`] ago: announce only.
    Coalesced,
}

/// Coalesces rings into washes for one surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct BellGate {
    last_wash_ms: Option<f64>,
}

impl BellGate {
    /// The program rang at `now_ms` (monotonic).
    pub fn ring(&mut self, now_ms: f64, reduced_motion: bool) -> Ring {
        if reduced_motion {
            return Ring::Dot;
        }
        if self
            .last_wash_ms
            .is_some_and(|last| now_ms - last < MIN_INTERVAL_MS)
        {
            return Ring::Coalesced;
        }
        self.last_wash_ms = Some(now_ms);
        Ring::Wash
    }

    /// Whether a wash is on screen at `now_ms`.
    #[must_use]
    pub fn washing(&self, now_ms: f64) -> bool {
        self.last_wash_ms
            .is_some_and(|last| now_ms - last < f64::from(wash_ms()))
    }
}
