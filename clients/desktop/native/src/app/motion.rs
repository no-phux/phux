//! Pane motion. A placement that appears in a tab already on screen grows in
//! from its split edge; a closing placement folds away before it is released.
//! Only painted geometry moves: the PTY size settles once the motion ends
//! (`input::FIT_SETTLE`), so a split never sends a resize per frame.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::settings::skin::{GLASS, span_progress as progress};

const ENTER: Duration = GLASS.motion.pane_enter();
const LEAVE: Duration = GLASS.motion.pane_leave();

pub struct Motion {
    /// How long a placement takes to grow in and to fold away.
    enter: Duration,
    leave: Duration,
    /// The tab the last frame painted, and its placements.
    tab: Option<String>,
    seen: HashSet<String>,
    entering: HashMap<String, Instant>,
    leaving: HashMap<String, Instant>,
    /// How far back each agent-driven placement sits, moving toward its target.
    depths: HashMap<String, Depth>,
    /// The placement the pointer is anywhere over; a recessed one comes forward.
    pub hover: Option<String>,
}

/// One placement's depth, 0 (forward) to 1 (recessed), easing from `from`
/// toward `to` since `start`.
#[derive(Debug, Clone, Copy)]
struct Depth {
    from: f32,
    to: f32,
    start: Instant,
}

impl Depth {
    fn at(&self, span: Duration, now: Instant) -> f32 {
        let t = progress(self.start, span, now);
        if t >= 1. {
            return self.to;
        }
        self.from + (self.to - self.from) * ease_out(t)
    }
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            enter: ENTER,
            leave: LEAVE,
            tab: None,
            seen: HashSet::new(),
            entering: HashMap::new(),
            leaving: HashMap::new(),
            depths: HashMap::new(),
            hover: None,
        }
    }
}

impl Motion {
    /// Set the enter and leave spans, from the skin.
    pub fn set_spans(&mut self, enter: Duration, leave: Duration) {
        self.enter = enter;
        self.leave = leave;
    }

    /// How long a fold takes before the placement is released.
    pub fn leave_span(&self) -> Duration {
        self.leave
    }

    /// Record this frame's tab and placements. A placement new to the tab the
    /// previous frame also painted enters; switching tabs animates nothing.
    pub fn observe(&mut self, tab: Option<&str>, placements: &[&str], now: Instant) {
        let same_tab = tab.is_some() && self.tab.as_deref() == tab;
        let unchanged = same_tab
            && placements.len() == self.seen.len()
            && placements.iter().all(|id| self.seen.contains(*id));
        if !unchanged {
            if same_tab {
                for id in placements {
                    if !self.seen.contains(*id) {
                        self.entering.insert((*id).to_owned(), now);
                    }
                }
            }
            self.tab = tab.map(str::to_owned);
            self.seen = placements.iter().map(|id| (*id).to_owned()).collect();
            self.leaving.retain(|id, _| self.seen.contains(id));
            self.depths.retain(|id, _| self.seen.contains(id));
        }
        let enter = self.enter;
        self.entering
            .retain(|id, at| self.seen.contains(id) && now.duration_since(*at) < enter);
    }

    /// Start folding `id` away. False when it is already leaving.
    pub fn leave(&mut self, id: &str, now: Instant) -> bool {
        if self.leaving.contains_key(id) {
            return false;
        }
        self.entering.remove(id);
        self.leaving.insert(id.to_owned(), now);
        true
    }

    /// The fold ended, whether or not the placement was released.
    pub fn left(&mut self, id: &str) {
        self.leaving.remove(id);
    }

    pub fn is_leaving(&self, id: &str) -> bool {
        self.leaving.contains_key(id)
    }

    /// How present `id` is, 0 (absent) to 1 (settled), eased.
    pub fn presence(&self, id: &str, now: Instant) -> f32 {
        if let Some(at) = self.leaving.get(id) {
            return 1. - ease_in(progress(*at, self.leave, now));
        }
        if let Some(at) = self.entering.get(id) {
            return ease_out(progress(*at, self.enter, now));
        }
        1.
    }

    /// How far back `id` sits now, 0 (forward) to 1 (recessed), as it eases
    /// toward `recessed` over the enter span. A placement first seen, or any
    /// change while `instant` (reduced motion), lands at once.
    pub fn depth(&mut self, id: &str, recessed: bool, instant: bool, now: Instant) -> f32 {
        let to = if recessed { 1. } else { 0. };
        let span = self.enter;
        let depth = self.depths.entry(id.to_owned()).or_insert(Depth {
            from: to,
            to,
            start: now,
        });
        if instant {
            *depth = Depth {
                from: to,
                to,
                start: now,
            };
        } else if depth.to != to {
            *depth = Depth {
                from: depth.at(span, now),
                to,
                start: now,
            };
        }
        depth.at(span, now)
    }

    /// Whether a frame is still moving.
    pub fn active(&self, now: Instant) -> bool {
        self.entering
            .values()
            .any(|at| now.duration_since(*at) < self.enter)
            || !self.leaving.is_empty()
            || self
                .depths
                .values()
                .any(|depth| depth.at(self.enter, now) != depth.to)
    }
}

/// The split ratio painted while its children are partly present: a child
/// growing in or folding away takes a share of its settled space in
/// proportion to its presence.
pub fn ratio(settled: f32, first: f32, second: f32) -> f32 {
    let left = settled * first;
    let right = (1. - settled) * second;
    if left + right <= f32::EPSILON {
        settled
    } else {
        left / (left + right)
    }
}

/// Fast start, long soft landing.
fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(4)
}

fn ease_in(t: f32) -> f32 {
    t * t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids<'a>(names: &[&'a str]) -> Vec<&'a str> {
        names.to_vec()
    }

    #[test]
    fn a_new_placement_in_the_same_tab_enters() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.observe(Some("t"), &ids(&["a"]), start);
        motion.observe(Some("t"), &ids(&["a", "b"]), start);
        assert_eq!(motion.presence("a", start), 1.);
        assert_eq!(motion.presence("b", start), 0.);
        assert!(motion.active(start));
        let later = start + ENTER;
        motion.observe(Some("t"), &ids(&["a", "b"]), later);
        assert_eq!(motion.presence("b", later), 1.);
        assert!(!motion.active(later));
    }

    #[test]
    fn an_unchanged_frame_keeps_entering_placements_moving() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.observe(Some("t"), &ids(&["a"]), start);
        motion.observe(Some("t"), &ids(&["a", "b"]), start);
        let mid = start + ENTER / 2;
        motion.observe(Some("t"), &ids(&["b", "a"]), mid);
        assert!(motion.presence("b", mid) < 1.);
        assert!(motion.active(mid));
        let later = start + ENTER;
        motion.observe(Some("t"), &ids(&["a", "b"]), later);
        assert!(!motion.active(later));
    }

    #[test]
    fn switching_tabs_or_first_paint_animates_nothing() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.observe(Some("t"), &ids(&["a", "b"]), now);
        motion.observe(Some("u"), &ids(&["c"]), now);
        assert_eq!(motion.presence("c", now), 1.);
        assert!(!motion.active(now));
    }

    #[test]
    fn leaving_folds_once_and_ends_when_the_placement_is_gone() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.observe(Some("t"), &ids(&["a", "b"]), now);
        assert!(motion.leave("b", now));
        assert!(!motion.leave("b", now));
        assert_eq!(motion.presence("b", now), 1.);
        assert_eq!(motion.presence("b", now + LEAVE), 0.);
        motion.observe(Some("t"), &ids(&["a"]), now + LEAVE);
        assert!(!motion.is_leaving("b"));
        assert!(!motion.active(now + LEAVE));
    }

    #[test]
    fn spans_follow_the_skin_and_an_empty_span_is_settled() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.set_spans(Duration::from_millis(100), Duration::ZERO);
        assert_eq!(motion.leave_span(), Duration::ZERO);
        motion.observe(Some("t"), &ids(&["a"]), now);
        motion.observe(Some("t"), &ids(&["a", "b"]), now);
        assert_eq!(motion.presence("b", now + Duration::from_millis(100)), 1.);
        assert!(motion.leave("a", now));
        assert_eq!(motion.presence("a", now), 0.);
    }

    #[test]
    fn depth_lands_at_first_sight_then_eases_between_targets() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.observe(Some("t"), &ids(&["a"]), start);
        assert_eq!(motion.depth("a", true, false, start), 1.);
        assert!(!motion.active(start));
        // Focus brings it forward over the enter span.
        assert_eq!(motion.depth("a", false, false, start), 1.);
        let mid = start + ENTER / 2;
        let halfway = motion.depth("a", false, false, mid);
        assert!(halfway > 0. && halfway < 1., "{halfway}");
        assert!(motion.active(mid));
        // Reversing mid-flight starts from where it is, not from an end.
        let back = motion.depth("a", true, false, mid);
        assert!((back - halfway).abs() < 1e-6);
        let later = mid + ENTER;
        assert_eq!(motion.depth("a", true, false, later), 1.);
        assert!(!motion.active(later));
    }

    #[test]
    fn reduced_motion_moves_depth_at_once_and_gone_placements_forget_it() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.observe(Some("t"), &ids(&["a", "b"]), now);
        assert_eq!(motion.depth("b", true, false, now), 1.);
        assert_eq!(motion.depth("b", false, true, now), 0.);
        assert!(!motion.active(now));
        motion.depth("b", true, false, now);
        motion.observe(Some("t"), &ids(&["a"]), now);
        assert!(!motion.active(now));
    }

    #[test]
    fn ratio_gives_an_absent_child_no_space() {
        assert_eq!(ratio(0.5, 1., 1.), 0.5);
        assert_eq!(ratio(0.5, 1., 0.), 1.);
        assert_eq!(ratio(0.5, 0., 1.), 0.);
        assert!((ratio(0.5, 1., 0.5) - 2. / 3.).abs() < 1e-6);
        assert_eq!(ratio(0.3, 0., 0.), 0.3);
    }
}
