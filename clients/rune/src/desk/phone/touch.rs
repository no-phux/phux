//! One finger on a phone's terminal, as phone terminals answer it: a drag
//! scrolls and coasts on, a hold selects (or hands the drag to a program
//! that tracks the mouse), and a tap clicks, opens a link or brings the
//! keyboard back. Scrolling goes through the wheel's policy, so history,
//! alternate-screen pagers and programs with their own scrolling all follow
//! the finger.
//!
//! Rune's drag carries no time. Every clock here is the app clock `poll`
//! was last given, so a gesture is timed from the first poll after it began.

use std::time::Duration;

use phux_client_runtime::engine::MouseMode;
use phux_protocol::ids::ResourceId;
use rune::{Mods, Phase, PointerButton, Tx};

use super::super::input::selection::Selected;
use super::super::mouse::Press;
use super::super::{Desk, NoticeKind};

/// How far a finger travels, in points, before its touch is a drag.
const SLOP: f32 = 8.0;
/// A finger held still this long, in seconds, selects instead of scrolling.
const HOLD: f64 = 0.35;
/// A finger still for this long before lifting throws nothing.
const SETTLE: f64 = 0.1;
/// The slowest lift, in points a second, that coasts on.
const FLING_MIN: f32 = 80.0;
/// A coast slower than this has stopped.
const FLING_STOP: f32 = 24.0;
/// How fast a coast decays, per second: a system scroll view's rate over
/// history, and a quicker stop where each row is a key or a wheel event
/// sent to a program.
const FRICTION: f32 = 2.0;
const FRICTION_PROGRAM: f32 = 5.0;
/// The longest step one tick coasts, in seconds, after a stalled frame.
const MAX_STEP: f64 = 0.05;
const FRAME: Duration = Duration::from_millis(8);

/// Feedback a native host plays: a phone has haptics, Rune has none yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    /// A hold was recognised: a selection or a program's drag began.
    Hold,
    /// Something was copied.
    Copied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Within the slop and the hold: lifting here is a tap.
    Pending,
    Scroll,
    /// A local selection, by words.
    Select,
    /// A program that tracks the mouse has the press.
    Report,
}

#[derive(Debug, Clone)]
pub struct Touch {
    terminal: ResourceId,
    /// Where the finger landed and where it is, in the grid's points.
    origin: [f32; 2],
    at: [f32; 2],
    /// When the first poll after the press ran.
    since: Option<f64>,
    mode: Mode,
    /// Vertical travel already scrolled, and what the last poll hasn't timed.
    scrolled: f32,
    untimed: f32,
    /// Points a second, smoothed over polls, and when it last moved.
    velocity: f32,
    sampled: f64,
    moved: f64,
    /// The finger lifted mid-scroll; the next poll decides its coast.
    lifted: bool,
}

#[derive(Debug, Clone)]
pub struct Fling {
    terminal: ResourceId,
    at: [f32; 2],
    velocity: f32,
    stamp: f64,
}

/// A tap or a hold's own click: selection by words suits a fingertip.
const WORDS: u32 = 2;

fn press(phase: Phase, [x, y]: [f32; 2], clicks: u32) -> Press {
    Press {
        phase,
        x,
        y,
        clicks,
        mods: Mods::default(),
        button: PointerButton::Primary,
    }
}

impl Desk {
    fn tracks_mouse(&self, terminal: &ResourceId) -> bool {
        self.mouse_mode(terminal) != MouseMode::None
    }

    /// One step of the primary finger over the focused terminal: `step` in
    /// its grid's points, `travel` since the press in window points.
    pub fn phone_touch(
        &mut self,
        terminal: &ResourceId,
        step: &Press,
        travel: [f32; 2],
        cancelled: bool,
        tx: &mut Tx,
    ) {
        let at = [step.x, step.y];
        match step.phase {
            Phase::Down => self.touch_down(terminal, at, tx),
            Phase::Move => self.touch_move(terminal, at, travel, tx),
            Phase::Up => self.touch_up(terminal, at, cancelled, tx),
        }
    }

    fn touch_down(&mut self, terminal: &ResourceId, at: [f32; 2], tx: &mut Tx) {
        self.focus_pane(terminal);
        // Records a link under the finger; a phone opens it on a tap.
        self.press_link(terminal, Phase::Down, at[0], at[1], false, tx);
        let clock = self.clock;
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        // A finger that lands on a coast catches it; that is not a tap.
        let caught = phone.fling.take().is_some();
        phone.touch = Some(Touch {
            terminal: terminal.clone(),
            origin: at,
            at,
            since: None,
            mode: if caught { Mode::Scroll } else { Mode::Pending },
            scrolled: 0.0,
            untimed: 0.0,
            velocity: 0.0,
            sampled: clock,
            moved: clock,
            lifted: false,
        });
        tx.wake_after(Duration::from_secs_f64(HOLD));
    }

    fn touch_move(&mut self, terminal: &ResourceId, at: [f32; 2], travel: [f32; 2], tx: &mut Tx) {
        let Some(touch) = self.phone.as_mut().and_then(|p| p.touch.as_mut()) else {
            return;
        };
        if &touch.terminal != terminal || touch.lifted {
            return;
        }
        touch.at = at;
        if touch.mode == Mode::Pending {
            if travel[0].hypot(travel[1]) <= SLOP {
                return;
            }
            touch.mode = Mode::Scroll;
        }
        match touch.mode {
            Mode::Scroll => {
                let dy = travel[1] - touch.scrolled;
                touch.scrolled = travel[1];
                touch.untimed += dy;
                self.wheel(terminal, at[0], at[1], dy, Mods::default());
            }
            Mode::Select => {
                let step = press(Phase::Move, at, WORDS);
                self.track_drag(terminal, &step, tx);
                self.select(terminal, Phase::Move, at[0], at[1], WORDS);
            }
            Mode::Report => self.pointer(terminal, &press(Phase::Move, at, 1), tx),
            Mode::Pending => {}
        }
    }

    fn touch_up(&mut self, terminal: &ResourceId, at: [f32; 2], cancelled: bool, tx: &mut Tx) {
        let Some(touch) = self.phone.as_mut().and_then(|p| p.touch.take()) else {
            return;
        };
        if &touch.terminal != terminal {
            return;
        }
        match (touch.mode, cancelled) {
            (Mode::Pending, false) => self.tap(terminal, at, tx),
            (Mode::Scroll, false) => {
                // The clock is the last poll's: let the next one time the lift.
                if let Some(phone) = self.phone.as_mut() {
                    phone.touch = Some(Touch {
                        at,
                        lifted: true,
                        ..touch
                    });
                }
                tx.wake_after(Duration::ZERO);
            }
            (Mode::Select, false) => {
                self.track_drag(terminal, &press(Phase::Up, at, WORDS), tx);
                self.select(terminal, Phase::Up, at[0], at[1], WORDS);
                self.copy_touched(tx);
            }
            (Mode::Report, false) => {
                self.pointer(terminal, &press(Phase::Up, at, 1), tx);
            }
            (Mode::Select, true) => self.end_drag(),
            (Mode::Report, true) => self.release_capture(),
            (Mode::Pending | Mode::Scroll, true) => {}
        }
        if touch.mode != Mode::Pending || cancelled {
            // Whatever else the gesture became, it was not a link's tap.
            self.forget_link_tap();
        }
    }

    /// A tap: the link under it, else a click for a program that tracks the
    /// mouse, else a plain click that clears the selection and brings the
    /// keyboard back.
    fn tap(&mut self, terminal: &ResourceId, at: [f32; 2], tx: &mut Tx) {
        if self.press_link(terminal, Phase::Up, at[0], at[1], false, tx) {
            return;
        }
        let tracked = self.tracks_mouse(terminal);
        self.pointer(terminal, &press(Phase::Down, at, 1), tx);
        self.pointer(terminal, &press(Phase::Up, at, 1), tx);
        if !tracked && let Some(phone) = self.phone.as_mut() {
            phone.keyboard = true;
        }
    }

    /// A finished selection goes to the clipboard: a phone has no ⌘C, and a
    /// menu would sit under the finger that just lifted.
    fn copy_touched(&mut self, tx: &mut Tx) {
        if !matches!(self.selected(), Selected::Text(_)) {
            return;
        }
        self.copy(tx);
        self.say(NoticeKind::Info, "Copied");
        self.phone_feedback(Feedback::Copied);
    }

    pub(in crate::desk) fn phone_feedback(&mut self, feedback: Feedback) {
        if let Some(phone) = self.phone.as_mut() {
            phone.feedback = Some(feedback);
        }
    }

    /// The gesture's clockwork, once a poll: a hold coming due, a scroll's
    /// speed, and a coast. True when the screen moved.
    pub(in crate::desk) fn phone_touch_tick(&mut self, tx: &mut Tx) -> bool {
        self.time_touch(tx);
        self.coast(tx)
    }

    fn time_touch(&mut self, tx: &mut Tx) {
        let clock = self.clock;
        let covered = self.modal().is_some();
        let Some(touch) = self.phone.as_mut().and_then(|p| p.touch.as_mut()) else {
            return;
        };
        let since = *touch.since.get_or_insert(clock);
        match touch.mode {
            Mode::Pending if covered => {}
            Mode::Pending if clock - since >= HOLD => self.hold(tx),
            Mode::Pending => tx.wake_after(Duration::from_secs_f64(HOLD - (clock - since))),
            Mode::Scroll => {
                let dt = (clock - touch.sampled) as f32;
                if touch.untimed != 0.0 && dt > 0.0 {
                    let now = touch.untimed / dt;
                    touch.velocity = f32::midpoint(touch.velocity, now);
                    touch.untimed = 0.0;
                    touch.moved = clock;
                }
                touch.sampled = clock;
                if touch.lifted {
                    self.release_scroll();
                }
            }
            Mode::Select | Mode::Report => {}
        }
    }

    /// The finger stayed put: a program that tracks the mouse gets the
    /// press (⇧ keeps it local, as on a desktop); otherwise a selection
    /// begins on the word under it.
    fn hold(&mut self, tx: &mut Tx) {
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        let Some(touch) = phone.touch.as_mut() else {
            return;
        };
        let (terminal, at) = (touch.terminal.clone(), touch.origin);
        let local = phone.shift || !self.tracks_mouse(&terminal);
        let mode = if local { Mode::Select } else { Mode::Report };
        if let Some(touch) = self.phone.as_mut().and_then(|p| p.touch.as_mut()) {
            touch.mode = mode;
        }
        self.forget_link_tap();
        if local {
            let step = press(Phase::Down, at, WORDS);
            self.track_drag(&terminal, &step, tx);
            self.select(&terminal, Phase::Down, at[0], at[1], WORDS);
        } else {
            self.pointer(&terminal, &press(Phase::Down, at, 1), tx);
        }
        self.phone_feedback(Feedback::Hold);
    }

    fn release_scroll(&mut self) {
        let clock = self.clock;
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        let Some(touch) = phone.touch.take() else {
            return;
        };
        let still = clock - touch.moved > SETTLE;
        if still || touch.velocity.abs() < FLING_MIN {
            return;
        }
        phone.fling = Some(Fling {
            terminal: touch.terminal,
            at: touch.at,
            velocity: touch.velocity,
            stamp: clock,
        });
    }

    fn coast(&mut self, tx: &mut Tx) -> bool {
        let clock = self.clock;
        let Some(fling) = self.phone.as_ref().and_then(|p| p.fling.clone()) else {
            return false;
        };
        let focused = self
            .shown
            .as_ref()
            .is_some_and(|s| s.terminal == fling.terminal);
        if !focused || self.modal().is_some() || self.coast_ended(&fling) {
            self.stop_coast();
            return false;
        }
        let dt = (clock - fling.stamp).clamp(0.0, MAX_STEP) as f32;
        let [x, y] = fling.at;
        self.wheel(&fling.terminal, x, y, fling.velocity * dt, Mods::default());
        let friction = if self.scrolls_history(&fling.terminal) {
            FRICTION
        } else {
            FRICTION_PROGRAM
        };
        let velocity = fling.velocity * (-friction * dt).exp();
        if velocity.abs() < FLING_STOP {
            self.stop_coast();
        } else if let Some(phone) = self.phone.as_mut() {
            phone.fling = Some(Fling {
                velocity,
                stamp: clock,
                ..fling
            });
            tx.wake_after(FRAME);
        }
        dt > 0.0
    }

    /// Typing, a new gesture or a covered terminal ends a coast.
    pub(in crate::desk) fn stop_coast(&mut self) {
        if let Some(phone) = self.phone.as_mut() {
            phone.fling = None;
        }
    }

    /// Local history, not a pager's arrows or a program's own wheel.
    fn scrolls_history(&self, terminal: &ResourceId) -> bool {
        !self.tracks_mouse(terminal)
            && self
                .client
                .as_ref()
                .is_none_or(|client| !client.is_alt_screen(terminal))
    }

    /// History has an end to stop at; a program's scrolling does not say.
    fn coast_ended(&self, fling: &Fling) -> bool {
        if !self.scrolls_history(&fling.terminal) {
            return false;
        }
        let frame = self
            .view_of(&fling.terminal)
            .zip(self.client.as_ref())
            .and_then(|(view, client)| client.acquire_view(view));
        frame.is_none_or(|frame| {
            if fling.velocity > 0.0 {
                frame.scrollbar.offset == 0
            } else {
                frame.scrollbar.at_tail()
            }
        })
    }
}
