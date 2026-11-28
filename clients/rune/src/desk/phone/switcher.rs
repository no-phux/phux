//! Browse sessions without attaching or sending any input. The one visible
//! preview is a read-only snapshot; choosing the card commits navigation.
use phux_client_runtime::control::SessionDescriptor;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};
use rune::{Drag, Key, Live, Phase, Tx};

use super::super::Desk;
use super::navigation::Page;
use super::swipe::{Across, FLICK_MIN, PAGE_COMMIT, rubber, tick};
use super::touch::Feedback;

#[derive(Clone, Debug)]
pub(crate) struct Switcher {
    sessions: Vec<u32>,
    index: usize,
    namespace: u64,
    epoch: u64,
    request: Option<u32>,
    pub(crate) preview: Option<String>,
    pub(crate) caption: String,
    pub(crate) slide: Slide,
}

/// Where a finger has the previewed card.
#[derive(Clone)]
pub(crate) struct Slide {
    /// The card's sideways shift: zero at rest.
    pub(crate) shift: Live<[f32; 2]>,
    finger: Option<Finger>,
    /// A paged card is drawn beside the stage, to settle next frame.
    arriving: bool,
}

impl Default for Slide {
    fn default() -> Self {
        Self {
            shift: Live::new([0.0; 2]),
            finger: None,
            arriving: false,
        }
    }
}

impl std::fmt::Debug for Slide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slide")
            .field("shift", &self.shift.get())
            .field("finger", &self.finger)
            .finish_non_exhaustive()
    }
}

/// The card under a finger: the stage it pages across, the frame's clock,
/// and which of how many sessions it previews.
#[derive(Debug, Clone, Copy)]
struct Card {
    w: f32,
    clock: f64,
    index: usize,
    len: usize,
}

impl Card {
    /// A session lies beyond the card the way `dx` pulls it.
    const fn beyond(self, dx: f32) -> bool {
        if dx < 0.0 {
            self.index + 1 < self.len
        } else {
            dx > 0.0 && self.index > 0
        }
    }

    /// A move: the card where the finger has it, or a rubber band's give
    /// where nothing lies beyond, ticking once as it crosses its point. A
    /// finger that went upright first is not the card's.
    fn follow(self, slide: &mut Slide, drag: &Drag, feedback: &mut Option<Feedback>) {
        let finger = slide.finger.get_or_insert(Finger {
            across: Across::new(self.clock),
            sideways: drag.dx.abs() > drag.dy.abs(),
            past: false,
        });
        if !finger.sideways {
            return;
        }
        finger.across.sample(drag.dx, self.clock);
        let (dx, w) = (drag.dx, self.w);
        let shown = if self.beyond(dx) {
            dx.clamp(-w, w)
        } else {
            rubber(dx, w)
        };
        slide.shift.snap([shown, 0.0]);
        let past = self.beyond(dx) && dx.abs() >= w * PAGE_COMMIT;
        tick(&mut finger.past, past, Feedback::Detent, feedback);
    }

    /// The lift: past its point or flicked, the step to page and whether
    /// it still owes its detent; else the card springs home.
    fn release(self, slide: &mut Slide, drag: &Drag) -> Option<(isize, bool)> {
        let mut finger = slide.finger.take().filter(|f| f.sideways)?;
        finger.across.sample(drag.dx, self.clock);
        let (dx, w) = (drag.dx, self.w);
        let pages = !drag.cancelled()
            && self.beyond(dx)
            && (finger.across).commits(dx, w, PAGE_COMMIT, FLICK_MIN, self.clock);
        if !pages {
            slide.shift.set([0.0; 2]);
            return None;
        }
        // The next card carries on the finger's way: it is drawn where the
        // old one would have had it beside, on the side that one left by,
        // and settles from there next frame.
        let step = if dx < 0.0 { 1 } else { -1 };
        slide.shift.snap([dx + w * step as f32, 0.0]);
        slide.arriving = true;
        // A flick short of the point has not ticked yet.
        Some((step, !finger.past))
    }
}

#[derive(Debug, Clone, Copy)]
struct Finger {
    across: Across,
    /// Its first movement was sideways: the card's to follow.
    sideways: bool,
    /// Past the commit point, so the detent ticks once per crossing.
    past: bool,
}

#[derive(serde::Deserialize)]
struct ScreenRows {
    lines: Vec<String>,
}

impl Desk {
    pub(crate) fn open_phone_switcher(&mut self) {
        if self.modal().is_some() {
            return;
        }
        let sessions: Vec<_> = self.ordered_sessions().into_iter().map(|s| s.id).collect();
        if sessions.is_empty() {
            self.open_phone_sessions();
            return;
        }
        let current = self.selected_session_name();
        let index = current
            .as_deref()
            .and_then(|name| {
                let selected = self.connection.topology.as_ref()?.session_named(name)?.id;
                sessions.iter().position(|id| *id == selected)
            })
            .unwrap_or(0);
        self.open_phone_navigation(Page::Switcher);
        let Some(phone) = &mut self.phone else { return };
        phone.switcher = Some(Switcher {
            sessions,
            index,
            namespace: self.connection.namespace(),
            epoch: self.connection.view_epoch,
            request: None,
            preview: None,
            caption: String::new(),
            slide: Slide::default(),
        });
        self.request_session_preview();
    }

    pub(crate) fn switcher_selection(&self) -> Option<(&SessionDescriptor, usize, usize)> {
        let page = self.phone.as_ref()?.switcher.as_ref()?;
        if page.namespace != self.connection.namespace() || page.epoch != self.connection.view_epoch
        {
            return None;
        }
        let id = page.sessions[page.index];
        let session = self.connection.topology.as_ref()?.session(id)?;
        Some((session, page.index, page.sessions.len()))
    }

    fn request_session_preview(&mut self) {
        let Some((session, ..)) = self.switcher_selection() else {
            return;
        };
        let resources = self.addressable();
        let mut terminals = resources
            .iter()
            .filter(|r| r.session_id == session.id && r.kind.is_terminal());
        let pane = self
            .connection
            .shown
            .as_ref()
            .and_then(|shown| terminals.clone().find(|r| r.id == shown.resource))
            .or_else(|| terminals.next());
        let request = pane.and_then(|pane| {
            let client = self.connection.client.as_ref()?;
            let epoch = self.connection.view_epoch;
            let server = self.connection.view_server.as_deref();
            client.with_control(|control| {
                if control.connection_epoch() != epoch
                    || control.server().map(|info| info.id.as_slice()) != server
                {
                    return None;
                }
                Some(control.send_command(Command::GetScreen {
                    terminal_id: pane.id.clone(),
                    request_scrollback: None,
                    cells: false,
                    format: 0,
                }))
            })
        });
        if let Some(page) = self.phone.as_mut().and_then(|p| p.switcher.as_mut()) {
            page.request = request;
            page.preview = None;
            page.caption =
                pane.map_or_else(|| "No terminal panes".into(), |pane| pane.label.clone());
        }
    }

    pub(in crate::desk) fn session_preview_result(
        &mut self,
        id: u32,
        result: &CommandResult,
    ) -> bool {
        if self.switcher_selection().is_none() {
            return false;
        }
        let Some(page) = self.phone.as_mut().and_then(|p| p.switcher.as_mut()) else {
            return false;
        };
        if page.request != Some(id) {
            return false;
        }
        page.request = None;
        page.preview = match result {
            CommandResult::OkWith(CommandValue::Json(json)) => {
                serde_json::from_str::<ScreenRows>(json).ok().map(|screen| {
                    // Ignore empty rows under the prompt, preserve indentation and
                    // the row breaks Ghostty published, and keep recent content.
                    let end = screen
                        .lines
                        .iter()
                        .rposition(|line| !line.trim().is_empty())
                        .map_or(0, |at| at + 1);
                    screen.lines[end.saturating_sub(24)..end].join("\n")
                })
            }
            _ => None,
        };
        true
    }

    pub(crate) fn switcher_loading(&self) -> bool {
        self.phone
            .as_ref()
            .and_then(|p| p.switcher.as_ref())
            .is_some_and(|p| p.request.is_some())
    }

    /// The arrows and keys: a step with its detent.
    pub(crate) fn step_phone_switcher(&mut self, step: isize) {
        self.step_switcher(step, true);
    }

    /// Preview the session `step` places over, ticking a detent unless a
    /// swipe already ticked its own crossing.
    fn step_switcher(&mut self, step: isize, detent: bool) {
        let Some((_, index, len)) = self.switcher_selection() else {
            return;
        };
        let Some(next) = index.checked_add_signed(step).filter(|next| *next < len) else {
            return;
        };
        if let Some(phone) = &mut self.phone
            && let Some(page) = &mut phone.switcher
        {
            page.index = next;
            if detent {
                phone.feedback = Some(Feedback::Detent);
            }
            self.request_session_preview();
        }
    }

    pub(crate) fn confirm_phone_switcher(&mut self) {
        if let Some((session, ..)) = self.switcher_selection() {
            let name = session.name.clone();
            self.close_phone_navigation();
            if self.selected_session_name().as_deref() != Some(name.as_str()) {
                self.show_session(&name);
            }
        }
    }

    /// A finger on the previewed card, as the terminal's sideways swipe
    /// takes one: the card follows it one to one (a live value, so a move
    /// runs no view), resists like a rubber band where there is no session
    /// beyond, ticks a detent at a third of the stage and pages there or
    /// on a flick. Let go short, it springs home. A drag that starts
    /// upright is the page's, not the card's.
    pub(crate) fn switcher_drag(&mut self, drag: &Drag, tx: &mut Tx) {
        let Some((_, index, len)) = self.switcher_selection() else {
            return;
        };
        let card = Card {
            w: self.stage_w(),
            // The frame's clock, which a handler runs on: what a flick is
            // timed by.
            clock: tx.clock(),
            index,
            len,
        };
        let paged = match drag.phase {
            Phase::Down => {
                self.switcher_slide(|slide, _| slide.finger = None);
                None
            }
            Phase::Move => {
                self.switcher_slide(|slide, feedback| card.follow(slide, drag, feedback));
                None
            }
            Phase::Up => self
                .switcher_slide(|slide, _| card.release(slide, drag))
                .flatten(),
        };
        let Some((step, detent)) = paged else {
            // Only live values changed: no view runs.
            tx.live_only();
            return;
        };
        tx.wake_after(std::time::Duration::ZERO);
        self.step_switcher(step, detent);
    }

    /// `f` on the switcher's slide and the phone's haptic, if it is open.
    fn switcher_slide<T>(
        &mut self,
        f: impl FnOnce(&mut Slide, &mut Option<Feedback>) -> T,
    ) -> Option<T> {
        let phone = self.phone.as_mut()?;
        let page = phone.switcher.as_mut()?;
        Some(f(&mut page.slide, &mut phone.feedback))
    }

    /// A paged card drawn beside the stage settles into place, a frame
    /// after it was drawn there.
    pub(in crate::desk) fn settle_switcher(&mut self) -> bool {
        let slide = self.phone.as_mut().and_then(|p| p.switcher.as_mut());
        if let Some(slide) = slide.map(|page| &mut page.slide)
            && std::mem::take(&mut slide.arriving)
        {
            slide.shift.set([0.0; 2]);
        }
        false
    }

    pub(super) fn switcher_key(&mut self, key: &Key) -> bool {
        match key {
            Key::Left => self.step_phone_switcher(-1),
            Key::Right => self.step_phone_switcher(1),
            Key::Enter => self.confirm_phone_switcher(),
            _ => return false,
        }
        true
    }
}
