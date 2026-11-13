//! ⌘-click (Ctrl-click elsewhere) opens a link; holding ⌘ over one
//! underlines it and shows where it goes. A phone has no ⌘: there a tap that
//! lifts where it landed opens the link (`phone/touch.rs`).

use std::sync::{Arc, Mutex};

use phux_protocol::ids::ResourceId;
use rune::{Mods, Phase, PointerPress, Tx};

use super::input::selection::Held;
use super::{Desk, NoticeKind};
use crate::links::{self, Link};

#[derive(Default)]
pub struct Pointer {
    /// The link ⌘ is over, and its pane.
    pub hover: Option<(ResourceId, Link)>,
    /// Where the pointer last moved over a pane, for a ⌘ press that comes
    /// without motion.
    last: Option<(ResourceId, f32, f32)>,
    /// A ⌘-press opened a link; its drag and release go nowhere else.
    opened: bool,
    /// A phone finger came down on a link: its pane, the link, and where.
    tap: Option<(ResourceId, String, f32, f32)>,
    /// ⌘ as the last pointer or modifier event left it.
    cmd: bool,
    /// Why the last opener gave up, from the thread waiting on it.
    pub(super) failed: Arc<Mutex<Option<String>>>,
    /// A selection drag autoscrolling past the grid.
    pub(super) held: Option<Held>,
    /// The context menu, while open, at its window point.
    pub menu: Option<PointerPress>,
    /// The open menu is a tab's, not a pane's.
    pub tab_menu: bool,
    /// The window's size in points, as the open menu last measured it.
    pub window: [f32; 2],
}

impl Desk {
    fn link_at(&self, terminal: &ResourceId, x: f32, y: f32) -> Option<Link> {
        let frame = self
            .client
            .as_ref()?
            .acquire_view(self.view_of(terminal)?)?;
        let m = self.metrics();
        // The padding, rail and pill are not the edge cell.
        let inside = x >= 0.0
            && y >= 0.0
            && x < f32::from(frame.cols) * m.cell_w
            && y < f32::from(frame.rows) * m.cell_h;
        if !inside {
            return None;
        }
        let (col, row) = m.cell_at(x, y, frame.cols, frame.rows);
        links::at(&frame, col, row)
    }

    fn set_hover(&mut self, hover: Option<(ResourceId, Link)>) {
        if self.pointer.hover != hover {
            self.pointer.hover = hover;
            self.generation += 1;
        }
    }

    /// The pointer moved over a pane, `x, y` in its grid's points.
    pub fn hover(&mut self, terminal: &ResourceId, x: f32, y: f32, mods: Mods) {
        let cmd = mods.cmd;
        self.pointer.last = Some((terminal.clone(), x, y));
        self.pointer.cmd = cmd;
        self.report_motion(terminal, x, y, mods);
        let link = cmd.then(|| self.link_at(terminal, x, y)).flatten();
        self.set_hover(link.map(|link| (terminal.clone(), link)));
    }

    /// The pointer left a pane: nothing there is under it any more.
    pub fn pointer_left(&mut self, terminal: &ResourceId) {
        self.forget_mouse_cell();
        if self
            .pointer
            .last
            .as_ref()
            .is_some_and(|(t, _, _)| t == terminal)
        {
            self.pointer.last = None;
            self.set_hover(None);
        }
    }

    /// A pane's output moved under a held ⌘: find the link again.
    pub(super) fn relink(&mut self, terminal: &ResourceId) {
        if self
            .pointer
            .last
            .as_ref()
            .is_some_and(|(t, _, _)| t == terminal)
        {
            self.modifiers_changed(self.pointer.cmd);
        }
    }

    /// ⌘ went down or up without the pointer moving.
    pub fn modifiers_changed(&mut self, cmd: bool) {
        self.pointer.cmd = cmd;
        let link = match (&self.pointer.last, cmd) {
            (Some((terminal, x, y)), true) => self
                .link_at(terminal, *x, *y)
                .map(|link| (terminal.clone(), link)),
            _ => None,
        };
        self.set_hover(link);
    }

    /// Whether `terminal`'s files are on another machine: this whole window's
    /// server is, or the pane is a federated satellite's in a local window.
    fn remote(&self, terminal: &ResourceId) -> bool {
        self.machines.current.is_some() || terminal.host().is_some()
    }

    /// Report an opener that failed after it started.
    pub(super) fn pump_links(&mut self) -> bool {
        let failed = self.pointer.failed.lock().ok().and_then(|mut f| f.take());
        let Some(why) = failed else {
            return false;
        };
        self.say(NoticeKind::Error, why);
        true
    }

    fn open_other(&self, url: &str) -> std::io::Result<()> {
        let failed = Arc::clone(&self.pointer.failed);
        let waker = self.waker.clone();
        links::open_other(url, move |why| {
            if let Ok(mut failed) = failed.lock() {
                *failed = Some(why);
            }
            if let Some(waker) = waker {
                waker();
            }
        })
    }

    fn open_link(&mut self, terminal: &ResourceId, url: &str, tx: &mut Tx) {
        if let Some(why) = links::refusal(url, self.remote(terminal)) {
            self.say(NoticeKind::Error, why);
            return;
        }
        let opened = match tx.open_url(url) {
            Err(rune::OpenUrlError::UnsupportedScheme(_)) => {
                self.open_other(url).map_err(|error| error.to_string())
            }
            other => other.map_err(|error| error.to_string()),
        };
        if let Err(error) = opened {
            self.say(NoticeKind::Error, format!("Couldn't open {url}: {error}"));
        }
    }

    /// A ⌘-press on a link opens it. True: this gesture is the link's.
    pub fn press_link(
        &mut self,
        terminal: &ResourceId,
        phase: Phase,
        x: f32,
        y: f32,
        cmd: bool,
        tx: &mut Tx,
    ) -> bool {
        if phase != Phase::Down {
            if let Some(url) = self.tapped(terminal, phase, x, y) {
                self.open_link(terminal, &url, tx);
                return true;
            }
            let opened = self.pointer.opened;
            if phase == Phase::Up {
                self.pointer.opened = false;
            }
            return opened;
        }
        // A new press; a release lost to another app ends the old one.
        self.pointer.opened = false;
        self.pointer.tap = None;
        if self.phone.is_some() && !cmd {
            let link = self.link_at(terminal, x, y);
            self.pointer.tap = link.map(|link| (terminal.clone(), link.url, x, y));
            return false;
        }
        let Some(link) = cmd.then(|| self.link_at(terminal, x, y)).flatten() else {
            return false;
        };
        self.open_link(terminal, &link.url, tx);
        self.pointer.opened = true;
        true
    }
}

impl Desk {
    /// The finger's gesture became something other than a tap.
    pub(super) fn forget_link_tap(&mut self) {
        self.pointer.tap = None;
    }
}

/// How far a finger may travel, in points, and still be a tap.
const TAP_SLOP: f32 = 8.0;

impl Desk {
    /// The link a phone tap just lifted from: the finger stayed within
    /// [`TAP_SLOP`] of where it landed on it. Travelling further makes the
    /// gesture a scroll.
    fn tapped(&mut self, terminal: &ResourceId, phase: Phase, x: f32, y: f32) -> Option<String> {
        let (on, _, x0, y0) = self.pointer.tap.as_ref()?;
        let still = on == terminal && (x - x0).hypot(y - y0) <= TAP_SLOP;
        if phase != Phase::Up {
            if !still {
                self.pointer.tap = None;
            }
            return None;
        }
        let (_, url, _, _) = self.pointer.tap.take()?;
        still.then_some(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::new(&Resolved::offline("links"))
    }

    #[test]
    fn only_a_local_pane_in_a_local_window_opens_files_here() {
        let mut desk = desk();
        let local = ResourceId::local(1);
        let satellite = ResourceId::satellite("box:2222", 1);
        let file = "file:///tmp/nothing-here/notes.txt";
        assert_eq!(links::refusal(file, desk.remote(&local)), None);
        assert!(links::refusal(file, desk.remote(&satellite)).is_some());
        desk.machines.current = Some("mini".to_owned());
        assert!(links::refusal(file, desk.remote(&local)).is_some());
    }

    #[test]
    fn a_phone_tap_opens_its_link_and_a_drag_does_not() {
        let mut desk = desk();
        let pane = ResourceId::local(1);
        let url = "https://phux.sh".to_owned();
        let land =
            |desk: &mut Desk| desk.pointer.tap = Some((pane.clone(), url.clone(), 40.0, 20.0));

        land(&mut desk);
        assert_eq!(desk.tapped(&pane, Phase::Move, 43.0, 22.0), None);
        assert_eq!(desk.tapped(&pane, Phase::Up, 44.0, 23.0), Some(url.clone()));
        assert_eq!(desk.tapped(&pane, Phase::Up, 44.0, 23.0), None, "once");

        land(&mut desk);
        assert_eq!(desk.tapped(&pane, Phase::Move, 80.0, 20.0), None);
        assert_eq!(desk.tapped(&pane, Phase::Up, 40.0, 20.0), None, "a drag");

        land(&mut desk);
        let other = ResourceId::local(2);
        assert_eq!(desk.tapped(&other, Phase::Up, 40.0, 20.0), None);
    }

    #[test]
    fn an_opener_that_fails_later_says_so_once() {
        let mut desk = desk();
        assert!(!desk.pump_links());
        *desk.pointer.failed.lock().expect("the lock") = Some("no handler".to_owned());
        assert!(desk.pump_links());
        let notice = desk.notice.as_ref().map(|notice| notice.text.as_str());
        assert_eq!(notice, Some("no handler"));
        assert!(!desk.pump_links());
    }
}
