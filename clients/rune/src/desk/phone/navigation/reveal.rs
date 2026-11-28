//! Where the Sessions list opens: where a kept search left it, else at its
//! top, unless the pane on screen would be wholly out of sight. Then it
//! opens on that pane's session, its card whole at the top, rather than a
//! minimal scroll that leaves the card above it cut under search.

use phux_design::tokens::SPACE;
use rune::Tx;

use super::Desk;

const LIST: &str = "phone-session-list";

/// Where the Sessions list opens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reveal {
    /// Where it was left, this far down: a search keeps its place among
    /// its results. Read as it opens, before the list reports again.
    Kept(f32),
    /// Around the pane on screen, once the list and that pane's row have
    /// each reported where their first layout put them, in either order.
    /// Both report in the layout that first shows the page; one that comes
    /// later (a page reopened while it was still leaving, then scrolled)
    /// is past `until` and moves nothing.
    ///
    /// `left` is where the last visit left the list, which Rune keeps by
    /// the scroller's key: it opens there, and goes back to its top. A
    /// scroll that something else asked for meanwhile (a focus reveal) is
    /// left alone.
    Current {
        list: Option<[f32; 4]>,
        row: Option<[f32; 4]>,
        until: f64,
        left: f32,
    },
}

/// How long after opening the first layout's reports are awaited.
const FIRST_LAYOUT: f64 = 0.25;

impl Reveal {
    /// Around the current pane, for a page opening at `clock` on a list
    /// the last visit `left` this far down.
    pub const fn current(clock: f64, left: f32) -> Self {
        Self::Current {
            list: None,
            row: None,
            until: clock + FIRST_LAYOUT,
            left,
        }
    }
}

/// The current row would be wholly out of sight with the list at its top:
/// it starts at or past the list's bottom edge, the list `scrolled` down.
fn out_of_sight(list: [f32; 4], row: [f32; 4], scrolled: f32) -> bool {
    row[1] + scrolled >= list[1] + list[3]
}

impl Desk {
    /// The Sessions list's layout. Its first after opening goes back where
    /// a kept search was. At the large text sizes search scrolls with the
    /// list, so it opens at its top, where search leads.
    pub fn reveal_phone_sessions(&mut self, large_text: bool, rect: [f32; 4], tx: &mut Tx) {
        let no_row = self.current_session_row().is_none();
        let Some(phone) = &mut self.phone else {
            return;
        };
        match &mut phone.reveal {
            Some(Reveal::Kept(kept)) => {
                tx.scroll_to_instant(LIST, 0.0, *kept);
                phone.reveal = None;
            }
            Some(Reveal::Current { left, .. }) if large_text || no_row => {
                let left = *left;
                phone.reveal = None;
                tx.live_only();
                if left > 0.0 && phone.sessions_scroll.get() == left {
                    tx.scroll_to_instant(LIST, 0.0, 0.0);
                }
            }
            Some(Reveal::Current { list, .. }) => {
                *list = Some(rect);
                self.reveal_current(tx);
            }
            // A later resize (the keyboard) changes nothing the view reads.
            None => tx.live_only(),
        }
    }

    /// The current pane's row (its session's header while folded) reports
    /// where layout put it: every frame of a scroll, so it moves nothing
    /// but the opening reveal.
    pub fn reveal_phone_current(&mut self, rect: [f32; 4], tx: &mut Tx) {
        let reveal = self.phone.as_mut().and_then(|p| p.reveal.as_mut());
        let Some(Reveal::Current { row, .. }) = reveal else {
            return tx.live_only();
        };
        *row = Some(rect);
        self.reveal_current(tx);
    }

    /// Both reported: open on the current session's card if its row is out
    /// of sight, else stay at the top, where Connections and Needs you lead.
    fn reveal_current(&mut self, tx: &mut Tx) {
        tx.live_only();
        let clock = self.clock;
        let Some(phone) = &mut self.phone else {
            return;
        };
        if let Some(Reveal::Current { until, .. }) = phone.reveal
            && clock > until
        {
            phone.reveal = None;
            return;
        }
        let Some(Reveal::Current {
            list: Some(list),
            row: Some(row),
            left,
            ..
        }) = phone.reveal
        else {
            return;
        };
        phone.reveal = None;
        let scrolled = phone.sessions_scroll.get();
        if scrolled != left {
            return;
        }
        if !out_of_sight(list, row, scrolled) {
            // At its top, Connections and Needs you whole.
            if scrolled > 0.0 {
                tx.scroll_to_instant(LIST, 0.0, 0.0);
            }
            return;
        }
        if let Some(section) = self.current_section() {
            // The list's own top inset, so the card sits as a first one does.
            tx.scroll_key_to(section, SPACE.s2);
        }
    }

    /// The current session's whole section, header and pane card.
    fn current_section(&self) -> Option<String> {
        let name = self.selected_session_name()?;
        let session = self.connection.topology.as_ref()?.session_named(&name)?.id;
        Some(format!("phone-session-section-{session}"))
    }

    /// The row that says where you are: the current pane, or its session's
    /// header while that session is folded.
    pub(crate) fn current_session_row(&self) -> Option<String> {
        let shown = self.connection.shown.as_ref()?;
        let name = self.selected_session_name()?;
        let session = self.connection.topology.as_ref()?.session_named(&name)?.id;
        let folded = self
            .phone
            .as_ref()
            .is_some_and(|p| p.folded.contains(&session));
        Some(if folded {
            format!("phone-session-{session}")
        } else {
            format!("phone-session-pane-{}", shown.resource)
        })
    }
}
