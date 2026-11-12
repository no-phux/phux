//! The changes panel: what Git says changed in the focused terminal's
//! checkout, and one file's diff. Reads only; nothing here stages or writes.

use phux_client_runtime::Client;
use phux_client_runtime::control::{CheckoutResponse, Status};
use phux_protocol::checkout::{CheckoutArea, CheckoutRequest};
use phux_protocol::ids::ResourceId;
use rune::{Key, KeyEvent, Tx};

use super::{Desk, NoticeKind};
use crate::changes::{self, Diff, Listing, Row};

/// The panel, while open, and what the server has answered so far.
#[derive(Default)]
pub struct Changes {
    pub open: bool,
    /// The terminal whose checkout this shows.
    terminal: Option<ResourceId>,
    /// The requests whose answers count; zero when none is out.
    status_request: u32,
    diff_request: u32,
    /// The files, or why there are none. `None` while the status loads.
    pub listing: Option<Result<Listing, String>>,
    pub picked: usize,
    /// The file a refresh keeps picked, if the new status still lists it.
    keep: Option<(String, CheckoutArea)>,
    /// The picked file's diff, or why there is none. `None` while it loads.
    pub diff: Option<Result<Diff, String>>,
}

impl Changes {
    pub fn rows(&self) -> &[Row] {
        match &self.listing {
            Some(Ok(listing)) => &listing.rows,
            _ => &[],
        }
    }
}

impl Desk {
    /// Open the panel on the focused terminal's checkout and read it once.
    pub(super) fn open_changes(&mut self) {
        let shown = self.shown.as_ref().map(|s| s.resource.clone());
        let Some(terminal) = shown.filter(|id| !self.is_document(id)) else {
            return self.say(NoticeKind::Error, "Focus a terminal to see its changes.");
        };
        self.close_inbox();
        self.close_settings();
        self.changes = Changes {
            open: true,
            terminal: Some(terminal),
            ..Changes::default()
        };
        self.refresh_changes();
    }

    pub fn close_changes(&mut self) {
        self.changes.open = false;
    }

    /// Read the status again; the diff follows its answer. Never on a timer.
    pub fn refresh_changes(&mut self) {
        let picked = self.changes.rows().get(self.changes.picked);
        self.changes.keep = picked.map(|row| (row.path.clone(), row.area));
        self.changes.diff_request = 0;
        self.changes.diff = None;
        self.changes.status_request = self.ask_checkout(CheckoutRequest::Status);
        self.changes.listing = (self.changes.status_request == 0).then(|| Err(self.unsent()));
    }

    fn ask_checkout(&self, request: CheckoutRequest) -> u32 {
        match (&self.client, &self.changes.terminal) {
            (Some(client), Some(terminal)) => client.checkout(terminal.clone(), request),
            _ => 0,
        }
    }

    fn unsent(&self) -> String {
        changes::unsent(self.status == Status::Attached).to_owned()
    }

    /// Ask for the picked file's diff.
    fn ask_diff(&mut self) {
        let Some(row) = self.changes.rows().get(self.changes.picked) else {
            self.changes.diff_request = 0;
            self.changes.diff = None;
            return;
        };
        let (path, area) = (row.path.clone(), row.area);
        self.changes.diff_request = self.ask_checkout(CheckoutRequest::Diff { path, area });
        self.changes.diff = match (self.changes.diff_request, area) {
            (0, CheckoutArea::Unknown) => {
                Some(Err("This version can't show that kind of change.".into()))
            }
            (0, _) => Some(Err(self.unsent())),
            _ => None,
        };
    }

    /// Answers to the panel's requests. True when what it shows changed.
    pub(super) fn take_changes(&mut self, client: &Client) -> bool {
        let answers = client.take_checkout_answers();
        let shown = answers.into_iter().filter(|a| self.changes_answered(a));
        shown.count() > 0
    }

    /// One answer: the status picks a file and asks for its diff. An answer
    /// to anything but the request now out is dropped.
    fn changes_answered(&mut self, answer: &CheckoutResponse) -> bool {
        // Zero is "nothing out", never an answer's id.
        if !self.changes.open || answer.request_id == 0 {
            return false;
        }
        if answer.request_id == self.changes.status_request {
            self.changes.status_request = 0;
            self.changes.listing = Some(changes::status_of(&answer.outcome));
            let keep = self.changes.keep.take();
            let mut rows = self.changes.rows().iter();
            let kept = keep
                .and_then(|(path, area)| rows.position(|row| row.path == path && row.area == area));
            self.changes.picked = kept.unwrap_or(0);
            self.ask_diff();
        } else if answer.request_id == self.changes.diff_request {
            self.changes.diff_request = 0;
            self.changes.diff = Some(changes::diff_of(&answer.outcome));
        } else {
            return false;
        }
        true
    }

    /// Pick a file and ask for its diff.
    pub fn pick_change(&mut self, index: usize, tx: &mut Tx) {
        if index != self.changes.picked && index < self.changes.rows().len() {
            self.changes.picked = index;
            self.ask_diff();
        }
        tx.scroll_into_view(format!("changes-row-{index}"));
    }

    /// The panel's keys: arrows move through the files, `r` reads again.
    pub(super) fn changes_key(&mut self, event: &KeyEvent, tx: &mut Tx) {
        let (count, at) = (self.changes.rows().len(), self.changes.picked);
        match &event.key {
            Key::Escape => self.close_changes(),
            Key::Down | Key::Tab if count > 0 => self.pick_change((at + 1) % count, tx),
            Key::Up if count > 0 => self.pick_change((at + count - 1) % count, tx),
            Key::Text(key) if key == "r" => self.refresh_changes(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
