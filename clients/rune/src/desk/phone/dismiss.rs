//! A page goes the way it came: a swipe takes it away. Sessions came in
//! from the left edge, so a leftward swipe sends it back; sheets rose from
//! below, so a downward one lowers them; Settings went a level in, so a
//! rightward swipe from the left edge comes back out. The page follows the
//! finger one to one and commits as the terminal's sideways swipe does
//! ([`Across`]: a third of the way, or a flick, with a detent on the way
//! in), so every swipe on the phone has one feel.
//!
//! Rune gives a touch to the element under it: a row takes its press and
//! the list under it the pan, and an element that drags owns every drag.
//! So a page cannot hear a swipe that starts on one of its rows. Instead the
//! host's touches pass through [`touch`] first, as a system gesture
//! recogniser sees them before the view does: within the slop Rune has them
//! as well (a tap still taps, a hold still holds), and once the finger
//! picks the page's way out the swipe claims it and Rune's press is
//! cancelled. Any other first movement (scrolling the list, a sideways drag
//! on a sheet) is Rune's alone until the finger lifts. A downward swipe on
//! a list defers to the list unless it is scrolled to its top, and no swipe
//! starts on a text field, whose drags select.
//!
//! The page reads where the finger has it through [`Dismiss`]'s live
//! values, so a move paints without rebuilding or laying out the view.

use rune::{Live, Runtime, TouchPhase};

use super::super::Desk;
use super::navigation::Page;
use super::swipe::{Across, EDGE, FLICK_MIN, PULL_COMMIT, rubber, tick};
use super::touch::Feedback;
use crate::desk::input::Modal;

/// Points a finger travels before it picks an axis: Rune's own touch slop,
/// so the swipe decides before Rune pans or drags anything.
const SLOP: f32 = 8.0;
/// A finger still this long is Rune's hold (a row's menu), not a swipe.
const HOLD: f64 = 0.5;
/// The share of its way out a slow swipe crosses to commit: the edge
/// pull's, mirrored.
const COMMIT: f32 = PULL_COMMIT;
/// A list scrolled less than this is at its top.
const AT_TOP: f32 = 0.5;

/// Which way a page leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// Leftward, back to the edge a drawer came from.
    Left,
    /// Downward, as a sheet lowers.
    Down,
    /// Rightward from the left edge, a level back out.
    Back,
}

impl Way {
    /// Travel toward leaving, from the finger's travel.
    fn along(self, [dx, dy]: [f32; 2]) -> f32 {
        match self {
            Self::Left => -dx,
            Self::Down => dy,
            Self::Back => dx,
        }
    }

    /// Travel on the other axis.
    fn across(self, [dx, dy]: [f32; 2]) -> f32 {
        match self {
            Self::Left | Self::Back => dy,
            Self::Down => dx,
        }
    }

    /// The page's shift for `travel` toward leaving.
    fn shift(self, travel: f32) -> [f32; 2] {
        match self {
            Self::Left => [-travel, 0.0],
            Self::Down => [0.0, travel],
            Self::Back => [travel, 0.0],
        }
    }
}

/// A sheet resting on the bottom edge above the key bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bottom {
    /// The desktop-hosted phone's Machines.
    Machines,
    /// What's new.
    News,
    /// Renaming a tab or session.
    Rename,
}

impl Bottom {
    /// The sheet's key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Machines => "machines",
            Self::News => "news",
            Self::Rename => "rename",
        }
    }

    /// Its scrolling body's key.
    pub const fn body(self) -> &'static str {
        match self {
            Self::Machines => "machines-body",
            Self::News => "news-body",
            Self::Rename => "rename-body",
        }
    }

    const fn field(self) -> &'static [&'static str] {
        match self {
            Self::Machines => &["machine-field"],
            Self::News => &[],
            Self::Rename => &["rename-field"],
        }
    }
}

/// The page on top that a swipe can take away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Away {
    /// The Sessions drawer.
    Sessions,
    /// Terminal actions or the session switcher.
    Sheet(Page),
    Inbox,
    Palette,
    Settings,
    Bottom(Bottom),
}

impl Away {
    pub const fn way(self) -> Way {
        match self {
            Self::Sessions => Way::Left,
            Self::Sheet(_) | Self::Inbox | Self::Palette | Self::Bottom(_) => Way::Down,
            Self::Settings => Way::Back,
        }
    }

    /// The page's list, whose own scrolling a swipe down it defers to
    /// unless it is at its top.
    const fn list(self) -> Option<&'static str> {
        match self {
            Self::Sheet(Page::Actions) => Some("phone-actions-list"),
            Self::Inbox => Some("inbox-scroll"),
            Self::Palette => Some("palette-scroll"),
            Self::Bottom(sheet) => Some(sheet.body()),
            Self::Sessions | Self::Sheet(_) | Self::Settings => None,
        }
    }

    /// This page's mark on its list's report, so a report another page's
    /// list left behind never counts for this one.
    const fn tag(self) -> f32 {
        match self {
            Self::Sessions => 1.0,
            Self::Sheet(Page::Sessions) => 2.0,
            Self::Sheet(Page::Actions) => 3.0,
            Self::Sheet(Page::Switcher) => 4.0,
            Self::Inbox => 5.0,
            Self::Palette => 6.0,
            Self::Settings => 7.0,
            Self::Bottom(Bottom::Machines) => 8.0,
            Self::Bottom(Bottom::News) => 9.0,
            Self::Bottom(Bottom::Rename) => 10.0,
        }
    }

    /// The text fields a swipe never starts on: editing owns their drags.
    const fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Sessions => &["phone-session-search"],
            Self::Inbox => &["inbox-reply"],
            Self::Palette => &["palette-search"],
            Self::Settings => &["settings-query"],
            Self::Bottom(sheet) => sheet.field(),
            Self::Sheet(_) => &[],
        }
    }
}

/// Where a finger has the page, as live values the page binds, and the
/// finger itself.
#[derive(Clone)]
pub struct Dismiss {
    /// The page's shift along its way out: zero at rest.
    pub shift: Live<[f32; 2]>,
    /// How much of the page still shows, from 1 to 0, for its opacity
    /// and the drawer's frost.
    pub shown: Live<f32>,
    /// How far a page's list is scrolled, as its scroller reports it,
    /// and which page's: `[offset, tag]`.
    list_top: Live<[f32; 2]>,
    finger: Option<Finger>,
}

impl Default for Dismiss {
    fn default() -> Self {
        Self {
            shift: Live::new([0.0; 2]),
            shown: Live::new(1.0),
            list_top: Live::new([0.0; 2]),
            finger: None,
        }
    }
}

impl std::fmt::Debug for Dismiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dismiss")
            .field("shift", &self.shift.get())
            .field("shown", &self.shown.get())
            .field("finger", &self.finger)
            .finish()
    }
}

impl Dismiss {
    /// The report `away`'s list makes in every frame that scrolls it: a
    /// live value, so scrolling runs no view.
    pub fn follow_list(&self, away: Away) -> impl Fn(rune::ScrollPos) + 'static {
        let top = self.list_top.clone();
        move |at: rune::ScrollPos| {
            top.set([at.offset[1], away.tag()]);
        }
    }

    /// How far `away`'s list is scrolled: at its top until it reports.
    pub fn list_offset(&self, away: Away) -> f32 {
        let [offset, tag] = self.list_top.get();
        if tag == away.tag() { offset } else { 0.0 }
    }

    /// Whether a finger is swiping the page away now.
    pub fn swiping(&self) -> bool {
        self.finger.is_some_and(|f| f.claim == Claim::Swiping)
    }

    /// Fresh values for the next page. A page leaving keeps the old ones,
    /// so its exit runs on from where the finger let it go.
    fn renew(&mut self) {
        self.shift = Live::new([0.0; 2]);
        self.shown = Live::new(1.0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    /// Within the slop: Rune has it as well, and it may yet be a tap.
    Pending,
    /// The swipe owns it; Rune's press was cancelled.
    Swiping,
    /// It moved some other way first: Rune's until it lifts.
    Passed,
}

#[derive(Debug, Clone, Copy)]
struct Finger {
    id: u64,
    origin: [f32; 2],
    since: f64,
    away: Away,
    /// The page's extent along its way out.
    extent: f32,
    /// It landed on the page's list, scrolled down from its top.
    scrolled: bool,
    claim: Claim,
    across: Across,
    /// Past the commit point, so the detent ticks once per crossing.
    past: bool,
}

impl Desk {
    /// The page on top a swipe can take away, if any.
    pub fn swipe_away(&self) -> Option<Away> {
        let phone = self.phone.as_ref()?;
        Some(match self.modal()? {
            Modal::PhoneNavigation => match phone.page? {
                Page::Sessions if phone.menu.is_none() => Away::Sessions,
                Page::Sessions => return None,
                page => Away::Sheet(page),
            },
            // A typed reply is never swiped away by accident: Close, which
            // discards it, is a decision.
            Modal::Inbox if self.inbox.reply.text.trim().is_empty() => Away::Inbox,
            Modal::Palette => Away::Palette,
            Modal::Settings => Away::Settings,
            Modal::Machines => Away::Bottom(Bottom::Machines),
            Modal::News => Away::Bottom(Bottom::News),
            Modal::Rename => Away::Bottom(Bottom::Rename),
            _ => return None,
        })
    }

    /// Whether `away` is the page a swipe would take: only it follows the
    /// finger, never a page under it.
    pub fn swipes(&self, away: Away) -> bool {
        self.swipe_away() == Some(away)
    }

    /// A committed swipe: the page goes as its own close control takes it.
    fn swiped_away(&mut self, away: Away, tx: &mut rune::Tx) {
        match away {
            Away::Sessions | Away::Sheet(_) => self.close_phone_navigation(),
            Away::Inbox => {
                // The same dismissal as Close and Escape, draft included.
                self.close_inbox();
                tx.blur();
            }
            Away::Palette => self.close_palette(),
            Away::Settings if self.settings_ui.category.is_some() => self.settings_back(),
            Away::Settings => self.close_settings(),
            Away::Bottom(sheet) => {
                // As its close does: a field it held lets the keyboard go.
                match sheet {
                    Bottom::Machines => self.close_machines(),
                    Bottom::News => self.close_news(),
                    Bottom::Rename => self.cancel_rename(),
                }
                tx.blur();
            }
        }
    }
}

/// One host touch, seen before Rune sees it: the phone's only touch path.
pub fn touch(rt: &mut Runtime<Desk>, id: u64, phase: TouchPhase, x: f32, y: f32, time: f64) {
    let at = [x, y];
    let claimed = match phase {
        TouchPhase::Started => {
            land(rt, id, at, time);
            false
        }
        TouchPhase::Moved => moved(rt, id, at, time),
        TouchPhase::Ended | TouchPhase::Cancelled => {
            lifted(rt, id, at, time, phase == TouchPhase::Cancelled)
        }
    };
    if !claimed {
        rt.touch(id, phase, x, y, time);
    }
}

fn finger(rt: &Runtime<Desk>) -> Option<Finger> {
    rt.state.phone.as_ref()?.dismiss.finger
}

fn set_finger(rt: &mut Runtime<Desk>, finger: Option<Finger>) {
    if let Some(phone) = rt.state.phone.as_mut() {
        phone.dismiss.finger = finger;
    }
}

/// A finger lands: note it if the page on top can be swiped away from here.
fn land(rt: &mut Runtime<Desk>, id: u64, at: [f32; 2], time: f64) {
    // Rune follows one finger; so does the swipe.
    if finger(rt).is_some() {
        return;
    }
    let Some(away) = rt.state.swipe_away() else {
        return;
    };
    let [x, y] = at;
    let on_field = away.fields().iter().any(|key| rt.contains(key, x, y));
    let from_edge = away.way() != Way::Back || x <= EDGE;
    if on_field || !from_edge {
        return;
    }
    // The phone's whole surface: a drawer crosses its width, a sheet its
    // height; one resting on the bottom edge, its own height.
    let Some(([_, _, w, h], _)) = rt.presented("phone") else {
        return;
    };
    let extent = match away {
        Away::Bottom(sheet) => rt.presented(sheet.key()).map_or(h, |(rect, _)| rect[3]),
        _ if away.way() == Way::Down => h,
        _ => w,
    };
    let list_top = rt
        .state
        .phone
        .as_ref()
        .map_or(0.0, |p| p.dismiss.list_offset(away));
    let scrolled = away
        .list()
        .is_some_and(|key| rt.contains(key, x, y) && list_top > AT_TOP);
    set_finger(
        rt,
        Some(Finger {
            id,
            origin: at,
            since: time,
            away,
            extent: extent.max(1.0),
            scrolled,
            claim: Claim::Pending,
            across: Across::new(time),
            past: false,
        }),
    );
}

/// A finger moves: true when the swipe has it, and Rune must not.
fn moved(rt: &mut Runtime<Desk>, id: u64, at: [f32; 2], time: f64) -> bool {
    let Some(mut f) = finger(rt).filter(|f| f.id == id) else {
        return false;
    };
    let travel = [at[0] - f.origin[0], at[1] - f.origin[1]];
    if f.claim == Claim::Pending {
        if travel[0].hypot(travel[1]) <= SLOP {
            return false;
        }
        f.claim = if claims(rt, &f, travel, time) {
            Claim::Swiping
        } else {
            Claim::Passed
        };
        set_finger(rt, Some(f));
        if f.claim == Claim::Swiping {
            // Rune lets go of the row's press and the list's pan.
            rt.touch(id, TouchPhase::Cancelled, f.origin[0], f.origin[1], time);
        }
    }
    if f.claim != Claim::Swiping {
        return false;
    }
    follow(rt, f, f.away.way().along(travel), time);
    true
}

/// Whether the finger's first deliberate movement is the page's way out.
fn claims(rt: &Runtime<Desk>, f: &Finger, travel: [f32; 2], time: f64) -> bool {
    let way = f.away.way();
    let along = way.along(travel);
    let menu = rt.state.phone.as_ref().is_some_and(|p| p.menu.is_some());
    let held = time - f.since >= HOLD || menu;
    along > 0.0 && along > way.across(travel).abs() && !held && !f.scrolled
}

/// The page where the finger has it, painted without a rebuild.
fn follow(rt: &mut Runtime<Desk>, mut f: Finger, along: f32, time: f64) {
    f.across.sample(along, time);
    // Back the other way it resists, as the terminal's swipe does at its ends.
    let travel = if along >= 0.0 {
        along.min(f.extent)
    } else {
        rubber(along, f.extent)
    };
    let past = along >= f.extent * COMMIT;
    let Some(phone) = rt.state.phone.as_mut() else {
        return;
    };
    phone.dismiss.shift.snap(f.away.way().shift(travel));
    phone
        .dismiss
        .shown
        .snap(1.0 - (travel / f.extent).clamp(0.0, 1.0));
    tick(&mut f.past, past, Feedback::Detent, &mut phone.feedback);
    phone.dismiss.finger = Some(f);
}

/// A finger lifts: a swipe past its point (or flicked) takes the page;
/// short of it the page springs home. True when the swipe had the finger.
fn lifted(rt: &mut Runtime<Desk>, id: u64, at: [f32; 2], time: f64, cancelled: bool) -> bool {
    let Some(f) = finger(rt).filter(|f| f.id == id) else {
        return false;
    };
    set_finger(rt, None);
    if f.claim != Claim::Swiping {
        return false;
    }
    let way = f.away.way();
    let along = way.along([at[0] - f.origin[0], at[1] - f.origin[1]]);
    let mut across = f.across;
    across.sample(along, time);
    let leaves =
        !cancelled && along > 0.0 && across.commits(along, f.extent, COMMIT, FLICK_MIN, time);
    if leaves {
        let still = rt.system_reduced_motion();
        rt.update(|d, tx| {
            // A finger caused it: the page's exit plays from where it is.
            // Under Reduce Motion it is gone at once, frost and all, as a
            // short swipe lands home at once, rather than fade a list
            // over a frost fading at the same pace.
            if still {
                tx.instant();
            } else {
                tx.animated();
            }
            d.swiped_away(f.away, tx);
            if let Some(phone) = d.phone.as_mut() {
                phone.dismiss.renew();
            }
        });
    } else {
        spring_home(rt);
    }
    true
}

/// Let go short: the page springs back where it was, or lands there at
/// once under Reduce Motion.
fn spring_home(rt: &mut Runtime<Desk>) {
    let still = rt.system_reduced_motion();
    let Some(phone) = rt.state.phone.as_ref() else {
        return;
    };
    let (shift, shown) = (&phone.dismiss.shift, &phone.dismiss.shown);
    if still {
        shift.snap([0.0; 2]);
        shown.snap(1.0);
    } else {
        shift.set([0.0; 2]);
        shown.set(1.0);
    }
}
