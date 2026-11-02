//! What the window says about its connection: where it stands, and what goes
//! over panes that have stopped updating.

use phux_client_runtime::control::Status;
use phux_client_runtime::reconnect::is_fatal_refusal_detail;

use super::{BANNER_AFTER, Desk, incompatible};

/// A failure in words, without the transport's.
pub fn plainly(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if let Some(words) = incompatible(error) {
        words
    } else if is_fatal_refusal_detail(error) {
        "This machine no longer accepts this computer. Set it up again from Machines.".to_owned()
    } else if lower.contains("refused") || lower.contains("no such file") {
        "The server isn't answering.".to_owned()
    } else if lower.contains("no answer") || lower.contains("timed out") {
        "No answer from the machine.".to_owned()
    } else {
        error.to_owned()
    }
}

/// Where the window stands when there is no terminal to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// `phux server --ensure` is running.
    Starting,
    /// No CLI to start a server with.
    NoCli,
    /// The server speaks another protocol major.
    Incompatible(String),
    /// Nothing answered, and this keeps trying unless retrying can't help.
    Unreachable(String),
    /// The runtime is dialing, with the last thing it heard.
    Connecting(String),
    /// The connection ended on purpose.
    Closed(String),
    /// A server with no sessions at all.
    NoSessions,
    /// The selected session holds no terminals.
    NoTerminals,
    /// Nothing is wrong; a terminal is on its way.
    Opening,
}

/// What goes over panes that have stopped updating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Banner {
    /// Still trying, with the last thing heard.
    Reconnecting(String),
    /// Trying stopped: someone has to act.
    Stuck(Standing),
}

impl Desk {
    pub fn standing(&self) -> Standing {
        if let Some(words) = &self.link.incompatible {
            return Standing::Incompatible(words.clone());
        }
        let why = || {
            let last = self.client.as_ref().and_then(|client| client.last_error());
            let error = self.error.clone().or(last).unwrap_or_default();
            plainly(&error)
        };
        match (&self.client, self.status) {
            _ if self.link.handing_off() && !self.link_is_up() => {
                Standing::Connecting("The server is updating.".to_owned())
            }
            (None, _) if self.link.finding => Standing::Starting,
            (None, _) if self.machines.current.is_none() && self.launch.missing_cli() => {
                Standing::NoCli
            }
            (None, _) | (Some(_), Status::Failed) => Standing::Unreachable(why()),
            (Some(_), Status::Closed) => {
                Standing::Closed(self.link.ended.clone().unwrap_or_default())
            }
            (Some(_), Status::Attached) if self.link.ended.is_none() => self.inventory(),
            (Some(_), _) => Standing::Connecting(why()),
        }
    }

    /// Connected, with nothing on screen: is there anything to show?
    fn inventory(&self) -> Standing {
        let selected = self.client.as_ref().and_then(|c| c.selected_session());
        let Some(topology) = &self.topology else {
            return Standing::Opening;
        };
        let session = topology.sessions.iter().find(|s| Some(s.id) == selected);
        let waiting = self.pending_session.is_some() || !self.pending_spawns.is_empty();
        match session {
            _ if waiting => Standing::Opening,
            None if topology.sessions.is_empty() => Standing::NoSessions,
            Some(s) if !topology.panes.iter().any(|p| p.session_id == s.id) => {
                Standing::NoTerminals
            }
            _ => Standing::Opening,
        }
    }

    /// What to say over panes that have stopped updating. Silent for the
    /// first moment of any drop.
    pub fn banner(&self) -> Option<Banner> {
        if self.shown.is_none() || self.link_is_up() || !self.link.is_down_for(BANNER_AFTER) {
            return None;
        }
        if self.stuck() {
            return Some(Banner::Stuck(self.standing()));
        }
        let last = self.client.as_ref().and_then(|client| client.last_error());
        let why = self.link.ended.clone().or(last).map(|why| plainly(&why));
        Some(Banner::Reconnecting(why.unwrap_or_else(|| {
            "Your sessions keep running on the server.".to_owned()
        })))
    }

    /// A word for the current machine's row.
    pub fn link_word(&self) -> &'static str {
        match self.status {
            _ if self.link_is_up() => "connected",
            Status::Failed | Status::Closed => "not connected",
            _ if self.client.is_none() && !self.link.finding => "not connected",
            _ => "connecting",
        }
    }
}
