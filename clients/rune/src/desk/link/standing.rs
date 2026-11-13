//! What the window says about its connection: where it stands, and what goes
//! over panes that have stopped updating.

use phux_client_runtime::control::Status;
use phux_client_runtime::reconnect::{AUTHORITY_CHANGED, is_fatal_refusal_detail};

use super::{BANNER_AFTER, Desk, incompatible};

/// How the QUIC lane words a refused pairing token, which the runtime's own
/// detail rule doesn't read back.
const QUIC_TOKEN_REFUSED: &str = "refused the pairing token";

/// What the phone app says once its computer has refused its credential.
pub(super) const UNPAIRED: &str = "This computer no longer accepts this phone. Pair again.";

/// Retrying can't change this refusal: the pairing has to be made again.
pub(super) fn refused_for_good(detail: &str) -> bool {
    is_fatal_refusal_detail(detail) || detail.contains(QUIC_TOKEN_REFUSED)
}

/// A failure in words, without the transport's.
pub fn plainly(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if let Some(words) = incompatible(error) {
        words
    } else if refused_for_good(error) {
        "This machine no longer accepts this computer. Set it up again from Machines.".to_owned()
    } else if lower.contains("refused") || lower.contains("no such file") {
        "The server isn't answering.".to_owned()
    } else if lower.contains("no answer") || lower.contains("timed out") {
        "No answer from the machine.".to_owned()
    } else {
        error.to_owned()
    }
}

/// The first things to check when the phone's computer, `name`, is silent.
pub fn reach_hint(name: &str) -> String {
    format!("Check that Tailscale is on and {name} is awake.")
}

/// The same failure for the phone app, which has no CLI to send anyone to
/// and knows its computer as `name`. Transport text never reaches the
/// screen: what isn't recognised is only a connection problem.
pub fn plainly_on_phone(error: &str, name: &str) -> String {
    let lower = error.to_ascii_lowercase();
    let says = |phrases: &[&str]| phrases.iter().any(|phrase| lower.contains(phrase));
    if error.is_empty() {
        String::new()
    } else if let Some(words) = incompatible(error) {
        words
    } else if says(&["fingerprint mismatch", AUTHORITY_CHANGED]) {
        // Before the refusals: a fingerprint's digits can spell one.
        format!(
            "{name} isn't the computer this phone paired with. Pair again if you set it up again."
        )
    } else if refused_for_good(error) {
        UNPAIRED.to_owned()
    } else if says(&[
        "could not resolve",
        "failed to lookup address",
        "resolved to no addresses",
    ]) {
        format!("Can't find {name}. Check that Tailscale is on.")
    } else if says(&[
        "did not answer",
        "no answer",
        "timed out",
        "stopped answering",
        "connection was lost",
    ]) {
        reach_hint(name)
    } else if says(&["refused", "no such file"]) {
        format!("{name} is awake, but phux isn't answering on it.")
    } else {
        "Connection problem.".to_owned()
    }
}

/// Where the window stands when there is no terminal to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// `phux server --ensure` is running.
    Starting,
    /// phux is being installed from this window.
    Installing,
    /// No CLI to start a server with, and why installing one failed.
    NoCli(Option<String>),
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
    /// A server restart ended this many of the selected session's terminals.
    Ended(usize),
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
            self.in_words(&error)
        };
        match (&self.client, self.status) {
            _ if self.link.handing_off() && !self.link_is_up() => {
                Standing::Connecting("The server is updating.".to_owned())
            }
            (None, _) if self.link.installing => Standing::Installing,
            (None, _) if self.link.finding => Standing::Starting,
            (None, _) if self.machines.current.is_none() && self.launch.missing_cli() => {
                Standing::NoCli(self.link.install_failed.clone())
            }
            (None, _) | (Some(_), Status::Failed) => {
                Standing::Unreachable(self.hinted(why(), true))
            }
            (Some(_), Status::Closed) => {
                Standing::Closed(self.link.ended.clone().unwrap_or_default())
            }
            (Some(_), Status::Attached) if self.link.ended.is_none() => self.inventory(),
            (Some(_), _) => {
                Standing::Connecting(self.hinted(why(), self.link.is_down_for(BANNER_AFTER)))
            }
        }
    }

    /// A failure in this window's words: a desktop's, or the phone app's.
    fn in_words(&self, error: &str) -> String {
        match &self.native {
            Some(_) => plainly_on_phone(error, self.machines.label()),
            None => plainly(error),
        }
    }

    /// The phone app never waits or fails wordlessly: once `due`, it says
    /// what to check. A dial is due when it outlasts a healthy one.
    fn hinted(&self, why: String, due: bool) -> String {
        if due && why.is_empty() && self.native.is_some() {
            reach_hint(self.machines.label())
        } else {
            why
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
                match self.ended_by_restart().len() {
                    0 => Standing::NoTerminals,
                    ended => Standing::Ended(ended),
                }
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
        let ended = self.link.ended.clone();
        let (why, place) = match &self.native {
            // The server's own parting words are already plain.
            Some(_) => (
                ended.or_else(|| last.map(|why| self.in_words(&why))),
                self.machines.label(),
            ),
            None => (ended.or(last).map(|why| plainly(&why)), "the server"),
        };
        let why = why.filter(|why| !why.is_empty());
        Some(Banner::Reconnecting(why.unwrap_or_else(|| {
            format!("Your sessions keep running on {place}.")
        })))
    }

    /// The machine refused this window's credential: pairing again is the
    /// only way back.
    pub fn unpaired(&self) -> bool {
        self.error.as_deref().is_some_and(refused_for_good)
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
