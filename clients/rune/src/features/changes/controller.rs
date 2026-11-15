//! The changes panel's state and checkout request correlation. The concrete
//! client is shared with the window; this feature never opens a connection.

use phux_client_runtime::Client;
use phux_client_runtime::control::CheckoutResponse;
use phux_protocol::checkout::{CheckoutArea, CheckoutRequest};
use phux_protocol::ids::ResourceId;

use super::presentation::{self, Diff, Listing, Row};

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
    pub fn open(&mut self, terminal: ResourceId) {
        *self = Self {
            open: true,
            terminal: Some(terminal),
            ..Self::default()
        };
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn rows(&self) -> &[Row] {
        match &self.listing {
            Some(Ok(listing)) => &listing.rows,
            _ => &[],
        }
    }

    /// Read once; the diff follows the status answer, never a timer.
    pub fn refresh(&mut self, client: Option<&Client>, connected: bool) {
        self.keep = self
            .rows()
            .get(self.picked)
            .map(|row| (row.path.clone(), row.area));
        self.diff_request = 0;
        self.diff = None;
        self.status_request = self.ask(client, CheckoutRequest::Status);
        self.listing =
            (self.status_request == 0).then(|| Err(presentation::unsent(connected).to_owned()));
    }

    fn ask(&self, client: Option<&Client>, request: CheckoutRequest) -> u32 {
        match (client, &self.terminal) {
            (Some(client), Some(terminal)) => client.checkout(terminal.clone(), request),
            _ => 0,
        }
    }

    fn ask_diff(&mut self, client: Option<&Client>, connected: bool) {
        let Some(row) = self.rows().get(self.picked) else {
            self.diff_request = 0;
            self.diff = None;
            return;
        };
        let (path, area) = (row.path.clone(), row.area);
        self.diff_request = self.ask(client, CheckoutRequest::Diff { path, area });
        self.diff = match (self.diff_request, area) {
            (0, CheckoutArea::Unknown) => {
                Some(Err("This version can't show that kind of change.".into()))
            }
            (0, _) => Some(Err(presentation::unsent(connected).to_owned())),
            _ => None,
        };
    }

    /// Drain the runtime's replies; stale or closed-panel requests don't count.
    pub fn take_answers(&mut self, client: &Client, connected: bool) -> bool {
        client
            .take_checkout_answers()
            .iter()
            .filter(|answer| self.answered(answer, client, connected))
            .count()
            > 0
    }

    fn answered(&mut self, answer: &CheckoutResponse, client: &Client, connected: bool) -> bool {
        // Zero means "nothing out", never an answer's id.
        if !self.open || answer.request_id == 0 {
            return false;
        }
        if answer.request_id == self.status_request {
            self.status_request = 0;
            self.listing = Some(presentation::status_of(&answer.outcome));
            self.restore_picked();
            self.ask_diff(Some(client), connected);
        } else if answer.request_id == self.diff_request {
            self.diff_request = 0;
            self.diff = Some(presentation::diff_of(&answer.outcome));
        } else {
            return false;
        }
        true
    }

    fn restore_picked(&mut self) {
        let keep = self.keep.take();
        let mut rows = self.rows().iter();
        self.picked = keep
            .and_then(|(path, area)| rows.position(|row| row.path == path && row.area == area))
            .unwrap_or(0);
    }

    /// Pick a file, ignoring repeated or out-of-bounds selections.
    pub fn pick(&mut self, index: usize, client: Option<&Client>, connected: bool) {
        if index == self.picked || index >= self.rows().len() {
            return;
        }
        self.picked = index;
        self.ask_diff(client, connected);
    }
}

#[cfg(test)]
mod tests;
