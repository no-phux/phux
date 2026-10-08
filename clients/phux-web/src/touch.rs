//! One-finger touch over the canvas, free of the DOM so it runs under the
//! node test harness.
//!
//! A finger means one of three things, decided by how it moves: a tap (it
//! lifts within [`TOUCH_SLOP_PX`] of where it landed, before
//! [`LONG_PRESS_MS`]), a scroll (it travels past the slop first), or a
//! selection (it rests for [`LONG_PRESS_MS`], then drags). Scrolling is the
//! default because it is what a thumb does most; selection stays reachable
//! behind the hold, as in a phone's own text views.

/// Finger travel, in CSS pixels, under which a touch is still a tap or a hold.
pub const TOUCH_SLOP_PX: f64 = 8.0;

/// How long a finger rests, in milliseconds, before a drag selects instead of
/// scrolling.
pub const LONG_PRESS_MS: f64 = 400.0;

/// What a touch has turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchMode {
    /// Still within the slop: a tap or a hold so far.
    Undecided,
    /// Travelled before the hold: the drag scrolls.
    Scroll,
    /// Moved after the hold: the drag selects.
    Select,
}

/// What one finger move does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchStep {
    /// Nothing yet: the finger has not left the slop, nor been held.
    Wait,
    /// Scroll by this much, in wheel-delta pixels (positive toward the live
    /// screen, as `WheelEvent.deltaY`): the finger moving up shows newer rows.
    Scroll(f64),
    /// The hold became a selection, anchored where the finger landed (in
    /// CSS pixels) and reaching to where it is now.
    StartSelect((f64, f64)),
    /// Move the selection's head under the finger.
    ExtendSelect,
}

/// One finger's gesture over the canvas, from the press that started it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchGesture {
    /// The `PointerEvent.pointerId` this gesture follows; other fingers are
    /// ignored.
    pub pointer_id: i32,
    start: (f64, f64),
    start_ms: f64,
    last_y: f64,
    mode: TouchMode,
}

impl TouchGesture {
    /// A finger landed at `(x, y)` CSS pixels at `now_ms`.
    #[must_use]
    pub const fn begin(pointer_id: i32, (x, y): (f64, f64), now_ms: f64) -> Self {
        Self {
            pointer_id,
            start: (x, y),
            start_ms: now_ms,
            last_y: y,
            mode: TouchMode::Undecided,
        }
    }

    /// What the gesture has turned out to be.
    #[must_use]
    pub const fn mode(&self) -> TouchMode {
        self.mode
    }

    /// The finger moved to `(x, y)` at `now_ms`.
    pub fn moved(&mut self, (x, y): (f64, f64), now_ms: f64) -> TouchStep {
        match self.mode {
            TouchMode::Undecided if self.held(now_ms) => {
                self.mode = TouchMode::Select;
                TouchStep::StartSelect(self.start)
            }
            TouchMode::Undecided if self.beyond_slop(x, y) => {
                self.mode = TouchMode::Scroll;
                self.scroll_to(y)
            }
            TouchMode::Undecided => TouchStep::Wait,
            TouchMode::Scroll => self.scroll_to(y),
            TouchMode::Select => TouchStep::ExtendSelect,
        }
    }

    /// Whether the finger lifting at `now_ms` makes this a tap: it never left
    /// the slop and was not held.
    #[must_use]
    pub fn is_tap(&self, now_ms: f64) -> bool {
        self.mode == TouchMode::Undecided && !self.held(now_ms)
    }

    fn held(&self, now_ms: f64) -> bool {
        now_ms - self.start_ms >= LONG_PRESS_MS
    }

    fn beyond_slop(&self, x: f64, y: f64) -> bool {
        (x - self.start.0).hypot(y - self.start.1) > TOUCH_SLOP_PX
    }

    /// Scroll by the travel since the last move, so the content follows the
    /// finger from where it landed.
    fn scroll_to(&mut self, y: f64) -> TouchStep {
        let delta = self.last_y - y;
        self.last_y = y;
        TouchStep::Scroll(delta)
    }
}
