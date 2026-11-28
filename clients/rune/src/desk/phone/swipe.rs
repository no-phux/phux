//! A sideways finger on the terminal: it pages to the neighbouring tab, and
//! past a session's last (or first) tab over to the next session, or,
//! landing at the left edge, pulls Sessions in over a blur. Paging follows
//! the finger one to one; crossing into another session is heavier, giving
//! less and asking for more travel with a firmer detent, so it is chosen
//! rather than slid into. Nowhere to go resists like a rubber band. A quick
//! flick does for a long drag, as a phone's pager and its edge drawers do.

use super::super::Desk;
use super::touch::Feedback;

/// Points from the left edge where a sideways drag pulls the drawer (or,
/// in Settings, goes back): a thumb's reach past a case's lip and a little
/// over, so a thumb that lands just inside the screen still finds it. The
/// rest of the stage still pages tabs.
pub const EDGE: f32 = 32.0;
/// How much of the stage a drag crosses to commit when lifted slowly.
pub const PAGE_COMMIT: f32 = 0.33;
/// How much of the stage a drag crosses to step over into another session.
pub const SESSION_COMMIT: f32 = 0.5;
/// How much of the finger's travel the page takes while crossing into
/// another session: the weight that tells it apart from a tab.
pub const SESSION_GIVE: f32 = 0.6;
pub const PULL_COMMIT: f32 = 0.3;
/// A lift at least this fast (points a second) along the drag commits it.
const FLICK: f32 = 450.0;
/// A flick still travels this far, so a nudge never pages.
pub(super) const FLICK_MIN: f32 = 32.0;
/// A finger still for this long before lifting throws nothing.
const SETTLE: f64 = 0.1;
/// Stage width when none is laid out yet.
const FALLBACK_W: f32 = 390.0;
/// How long a paged-away session waits off the stage for the next one.
const LANDING: f64 = 1.5;

/// What the sideways gesture shows, read by the stage and the drawer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Swipe {
    /// The terminal's sideways offset while paging; 0 at rest.
    pub page: f32,
    /// A finger is on the page offset: it follows that, not a spring.
    pub paging: bool,
    /// How far the drawer is pulled in while the finger holds it.
    pub pull: Option<f32>,
    /// Which way the last committed page went: the incoming session
    /// slides in from the opposite side. -1 forward, 1 back.
    pub arrival: Option<f32>,
    /// The page paged away from ([`Desk::page_key`]), parked off the stage
    /// until the next one shows, and when: a switch that never lands brings
    /// it back.
    pub leaving: Option<(String, f64)>,
}

/// Where a sideways drag goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Beside {
    /// This session's tab at this index.
    Tab(usize),
    /// Past this session's last or first tab, the next session over.
    Session(String),
}

impl Beside {
    /// The share of the stage a slow drag crosses to commit.
    const fn commit(&self) -> f32 {
        match self {
            Self::Tab(_) => PAGE_COMMIT,
            Self::Session(_) => SESSION_COMMIT,
        }
    }
}

/// Sideways travel and its speed, sampled on the app clock.
#[derive(Debug, Clone, Copy)]
pub struct Across {
    dx: f32,
    /// Position paired with `sampled`, independent of later same-frame moves.
    sampled_dx: f32,
    velocity: f32,
    sampled: f64,
    /// Past the commit point, so the detent ticks once per crossing.
    past: bool,
}

impl Across {
    pub(super) const fn new(clock: f64) -> Self {
        Self {
            dx: 0.0,
            sampled_dx: 0.0,
            velocity: 0.0,
            sampled: clock,
            past: false,
        }
    }

    pub(super) fn sample(&mut self, dx: f32, clock: f64) {
        let dt = (clock - self.sampled) as f32;
        // A finger that rested has no speed left to throw with, whatever
        // the roll-off of its lift adds.
        if f64::from(dt) > SETTLE {
            self.velocity = 0.0;
        }
        if dt > 0.0 {
            let now = (dx - self.sampled_dx) / dt;
            self.velocity = f32::midpoint(self.velocity, now);
            self.sampled = clock;
            self.sampled_dx = dx;
        }
        // A frame can deliver several moves before the lift. Distance always
        // follows the last one, even when no further time can sample velocity.
        self.dx = dx;
    }

    /// The lift's speed: none after the finger rested.
    fn velocity(&self, clock: f64) -> f32 {
        if clock - self.sampled > SETTLE {
            0.0
        } else {
            self.velocity
        }
    }

    /// A slow drag past `fraction` of `w`, or a flick along it at least
    /// `flick_min` points long.
    pub(super) fn commits(
        &self,
        dx: f32,
        w: f32,
        fraction: f32,
        flick_min: f32,
        clock: f64,
    ) -> bool {
        let v = self.velocity(clock);
        let flicked = v.abs() >= FLICK && v.signum() == dx.signum() && dx.abs() >= flick_min;
        // A flick back against the drag cancels it, however far it went.
        let reversed = v.abs() >= FLICK && v.signum() != dx.signum();
        flicked || (dx.abs() >= w * fraction && !reversed)
    }
}

/// Past the end, the page gives less and less: a system scroll view's
/// rubber band, `(1 − 1 / (x·c / w + 1)) · w`.
pub(super) fn rubber(dx: f32, w: f32) -> f32 {
    const C: f32 = 0.55;
    dx.signum() * (1.0 - 1.0 / (dx.abs() * C / w + 1.0)) * w
}

/// A finger moving left pages forward.
fn step_of(dx: f32) -> isize {
    if dx < 0.0 { 1 } else { -1 }
}

impl Desk {
    pub(super) fn stage_w(&self) -> f32 {
        self.phone
            .as_ref()
            .map(|phone| phone.stage[0])
            .filter(|w| *w > 1.0)
            .unwrap_or(FALLBACK_W)
    }

    /// The session `step` places from the selected one in the sidebar's
    /// order, without wrapping: a swipe stops at either end.
    pub fn session_beside(&self, step: isize) -> Option<String> {
        let current = self.selected_session_name()?;
        let names = self.ordered_sessions();
        let at = names.iter().position(|s| s.name == current)?;
        let next = at.checked_add_signed(step)?;
        names.get(next).map(|s| s.name.clone())
    }

    /// A finger is pulling the Sessions drawer in from the left edge.
    pub fn phone_pulling(&self) -> bool {
        self.phone
            .as_ref()
            .is_some_and(|phone| phone.swipe.pull.is_some())
    }

    /// The tab `step` places from the active one in this session.
    fn tab_beside(&self, step: isize) -> Option<usize> {
        let tabs = &self.connection.tabs.list;
        let at = tabs.iter().position(|t| t.active)?;
        let next = at.checked_add_signed(step)?;
        (next < tabs.len()).then_some(next)
    }

    /// Where a sideways drag of `dx` would page to: the tab beside, else
    /// the session beside.
    pub fn page_target(&self, dx: f32) -> Option<Beside> {
        if dx == 0.0 {
            return None;
        }
        let step = step_of(dx);
        (self.tab_beside(step).map(Beside::Tab))
            .or_else(|| self.session_beside(step).map(Beside::Session))
    }

    /// What the stage's page is: its session and tab, so a page to either
    /// slides the old one off as the new one comes in.
    pub fn page_key(&self) -> String {
        let session = self.selected_session_name().unwrap_or_default();
        let tab = self.connection.tabs.list.iter().position(|t| t.active);
        let tab = tab.map(|at| self.connection.tabs.list[at].id);
        let tab: String = tab.map_or_else(String::new, |id| {
            id.iter().map(|b| format!("{b:02x}")).collect()
        });
        format!("{session}/{tab}")
    }

    /// The finger's sideways travel since it landed, paging.
    pub(super) fn page_move(&mut self, dx: f32) {
        let w = self.stage_w();
        let next = self.page_target(dx);
        let shown = match &next {
            Some(Beside::Tab(_)) => dx.clamp(-w, w),
            Some(Beside::Session(_)) => (dx * SESSION_GIVE).clamp(-w, w),
            None => rubber(dx, w),
        };
        let past = next.as_ref().is_some_and(|n| dx.abs() >= w * n.commit());
        let detent = match next {
            Some(Beside::Session(_)) => Feedback::Threshold,
            _ => Feedback::Detent,
        };
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        phone.swipe.page = shown;
        phone.swipe.paging = true;
        phone.swipe.arrival = None;
        phone.swipe.leaving = None;
        if let Some(touch) = phone.touch.as_mut() {
            tick(&mut touch.across.past, past, detent, &mut phone.feedback);
        }
    }

    /// The finger's sideways travel from the edge, pulling the drawer.
    pub(super) fn pull_move(&mut self, dx: f32) {
        let w = self.stage_w();
        let pulled = dx.clamp(0.0, w);
        let past = pulled >= w * PULL_COMMIT;
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        phone.swipe.pull = Some(pulled);
        if let Some(touch) = phone.touch.as_mut() {
            let detent = Feedback::Detent;
            tick(&mut touch.across.past, past, detent, &mut phone.feedback);
        }
    }

    /// Lift while paging: on to the neighbour, or back where it was. A
    /// flick into another session must still have crossed a tab's worth.
    pub(super) fn page_release(&mut self, across: &Across) {
        let dx = across.dx;
        let w = self.stage_w();
        let next = self.page_target(dx).filter(|next| {
            let flick_min = match next {
                Beside::Tab(_) => FLICK_MIN,
                Beside::Session(_) => w * PAGE_COMMIT,
            };
            across.commits(dx, w, next.commit(), flick_min, self.clock)
        });
        self.settle_swipe();
        let Some(next) = next else { return };
        let leaving = self.page_key();
        let clock = self.clock;
        if let Some(phone) = self.phone.as_mut() {
            phone.swipe.arrival = Some(dx.signum());
            phone.swipe.leaving = Some((leaving, clock));
        }
        match next {
            Beside::Tab(at) => self.select_tab(at),
            Beside::Session(name) => self.show_session(&name),
        }
    }

    /// A paged-away page still shown after [`LANDING`] did not switch:
    /// bring it back rather than leave the stage empty.
    pub(in crate::desk) fn land_swipe(&mut self, tx: &mut rune::Tx) -> bool {
        let clock = self.clock;
        let selected = Some(self.page_key());
        let Some(phone) = self.phone.as_mut() else {
            return false;
        };
        let Some((name, since)) = &phone.swipe.leaving else {
            return false;
        };
        if selected.as_ref() != Some(name) {
            phone.swipe.leaving = None;
            true
        } else if clock - since >= LANDING {
            phone.swipe.leaving = None;
            phone.swipe.arrival = None;
            true
        } else {
            tx.wake_after(std::time::Duration::from_secs_f64(
                LANDING - (clock - since),
            ));
            false
        }
    }

    /// Lift while pulling: the drawer opens, or slides back out.
    pub(super) fn pull_release(&mut self, across: &Across) {
        let (w, clock) = (self.stage_w(), self.clock);
        let open = across.commits(across.dx, w, PULL_COMMIT, FLICK_MIN, clock) && across.dx > 0.0;
        self.settle_swipe();
        if open {
            self.open_phone_sessions();
        }
    }

    /// Let the page spring home and the drawer go.
    pub(in crate::desk) fn settle_swipe(&mut self) {
        if let Some(phone) = self.phone.as_mut() {
            phone.swipe.page = 0.0;
            phone.swipe.paging = false;
            phone.swipe.pull = None;
        }
    }
}

/// Play `detent` as the drag crosses into committing, not back out.
pub(super) fn tick(past: &mut bool, now: bool, detent: Feedback, feedback: &mut Option<Feedback>) {
    if now && !*past {
        *feedback = Some(detent);
    }
    *past = now;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rubber_band_gives_less_the_farther_it_goes() {
        let w = 400.0;
        let near = rubber(40.0, w);
        let far = rubber(400.0, w);
        assert!(near > 0.0 && near < 40.0);
        assert!(far < 400.0 * 0.5);
        assert!(far - rubber(360.0, w) < near);
        assert_eq!(rubber(-40.0, w), -near);
    }

    #[test]
    fn same_clock_moves_keep_the_latest_distance() {
        let mut across = Across::new(0.0);
        across.sample(-60.0, 0.016);
        across.sample(-240.0, 0.016);
        assert_eq!(across.dx, -240.0);
        assert!(across.commits(across.dx, 400.0, SESSION_COMMIT, 132.0, 0.3));
        across.sample(-40.0, 0.016);
        assert_eq!(across.dx, -40.0);
        assert!(!across.commits(across.dx, 400.0, SESSION_COMMIT, 132.0, 0.3));
    }

    #[test]
    fn untimed_moves_preserve_the_velocity_baseline_without_double_counting() {
        let mut across = Across::new(0.0);
        across.sample(-20.0, 0.02);
        assert!((across.velocity + 500.0).abs() < 0.1);
        across.sample(-60.0, 0.02);
        assert!((across.velocity + 500.0).abs() < 0.1);
        across.sample(-100.0, 0.04);
        assert!((across.velocity + 2250.0).abs() < 0.1);
        across.sample(-100.0, 0.06);
        assert!((across.velocity + 1125.0).abs() < 0.1);
        across.sample(200.0, 0.06);
        assert_eq!(across.dx, 200.0, "a reversal updates distance, not time");
        assert_eq!(
            across.velocity(0.3),
            0.0,
            "untimed moves cannot revive a flick"
        );
    }

    #[test]
    fn an_entire_drag_at_the_initial_clock_has_distance_but_no_fabricated_flick() {
        let mut across = Across::new(1.0);
        across.sample(-20.0, 1.0);
        assert!(!across.commits(across.dx, 400.0, PAGE_COMMIT, FLICK_MIN, 1.0));
        across.sample(-240.0, 1.0);
        assert!(across.commits(across.dx, 400.0, SESSION_COMMIT, 132.0, 1.0));
        assert_eq!(across.velocity, 0.0);
    }

    #[test]
    fn a_slow_drag_commits_past_its_fraction_and_a_flick_short_of_it() {
        let mut across = Across::new(0.0);
        across.sample(-140.0, 1.0);
        assert!(across.commits(-140.0, 400.0, PAGE_COMMIT, FLICK_MIN, 1.0));
        assert!(!across.commits(-100.0, 400.0, PAGE_COMMIT, FLICK_MIN, 2.0));
        let mut flick = Across::new(0.0);
        flick.sample(-30.0, 0.016);
        flick.sample(-60.0, 0.032);
        assert!(flick.commits(-60.0, 400.0, PAGE_COMMIT, FLICK_MIN, 0.04));
        assert!(
            !flick.commits(-60.0, 400.0, PAGE_COMMIT, FLICK_MIN, 0.5),
            "a rest throws nothing"
        );
        flick.sample(-60.0, 0.5);
        assert!(
            !flick.commits(-60.0, 400.0, PAGE_COMMIT, FLICK_MIN, 0.5),
            "nor the lift's last still step after it"
        );
        let mut back = Across::new(0.0);
        back.sample(-200.0, 0.5);
        back.sample(-170.0, 0.516);
        back.sample(-140.0, 0.532);
        assert!(
            !back.commits(-140.0, 400.0, PAGE_COMMIT, FLICK_MIN, 0.54),
            "a flick back cancels"
        );
    }

    #[test]
    fn the_detent_ticks_once_on_the_way_in() {
        let (mut past, mut feedback) = (false, None);
        tick(&mut past, true, Feedback::Detent, &mut feedback);
        assert_eq!(feedback.take(), Some(Feedback::Detent));
        tick(&mut past, true, Feedback::Detent, &mut feedback);
        tick(&mut past, false, Feedback::Detent, &mut feedback);
        assert_eq!(feedback, None);
    }
}
