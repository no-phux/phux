//! `phux://` links that show something in the running window:
//!
//! - `phux://pane/@N` (`phux://pane/HOST/@N` for a hub's satellite pane)
//! - `phux://session/NAME`, that session's focused terminal
//! - `phux://tab/NAME/N`, the Nth tab of session NAME, counting from 1
//! - `phux://folder/PATH`, a new tab in that local folder
//!
//! A link reaches this process the way pairing links do, through
//! `open`/`xdg-open` and the single-instance hand-off, so no second window
//! opens. Anyone can open a link, so it is parsed strictly: names and paths
//! are percent-decoded, a folder must be a directory here, and its path only
//! becomes the new terminal's working directory, never shell text. A target
//! that isn't there is an error, never a nearby one.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use phux_client_runtime::control::{SessionDescriptor, Topology};
use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind};
use crate::links::open::percent_decode;

const SCHEME: &str = "phux://";
/// How long a tab link waits for its session's tabs to load.
const TAB_WAIT: Duration = Duration::from_secs(5);

/// What a link asks the window to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Pane(ResourceId),
    Session(String),
    /// A session's tab by its place in the strip, from 1; `since` is when
    /// it began waiting for the session's tabs.
    Tab {
        session: String,
        index: usize,
        since: Option<Instant>,
    },
    Folder(PathBuf),
}

/// A link held until it can be shown, with the view scope it was opened
/// in: a delayed link never lands on a replacement server or over a newer
/// focus gesture.
pub(super) struct Reveal {
    link: Link,
    /// A pane link's presentation source, captured when it was opened.
    intent: Option<super::view_intent::ViewIntent>,
    origin: super::connection::CallbackFence,
    choice: u64,
}

impl Reveal {
    /// Still the latest view choice, in the scope it was opened in. A pane
    /// whose exact source was captured may open its retained replica on the
    /// same client; anything else needs the original connection.
    fn current(&self, connection: &super::connection::Connection) -> bool {
        self.choice == connection.view_choice
            && if self
                .intent
                .as_ref()
                .is_some_and(super::view_intent::ViewIntent::has_source)
            {
                self.origin.same_namespace(connection)
            } else {
                self.origin.is_current(connection)
            }
    }
}

/// What a link resolved to on this server.
enum Step {
    Pane(ResourceId, String),
    Session(String),
    Folder(PathBuf),
    Wait(Link),
}

/// What a link asks for, `None` when `url` isn't one of these (a pairing
/// link, or another `phux://` kind).
pub fn target(url: &str) -> Option<Result<Link, String>> {
    let url = url.trim();
    let head = url.get(..SCHEME.len())?;
    if !head.eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let (kind, rest) = url[SCHEME.len()..].split_once('/')?;
    let trimmed = rest.trim_end_matches('/');
    Some(match kind.to_ascii_lowercase().as_str() {
        "pane" => pane(trimmed).map(Link::Pane).ok_or_else(|| {
            format!("This pane link can't be used: {rest:?} isn't a pane id like @3.")
        }),
        "session" => name(trimmed)
            .map(Link::Session)
            .ok_or_else(|| "This session link names no session.".to_owned()),
        "tab" => tab(trimmed).ok_or_else(|| {
            "This tab link can't be used: it needs a session and a tab number, like work/2."
                .to_owned()
        }),
        "folder" => folder(rest),
        _ => return None,
    })
}

fn pane(text: &str) -> Option<ResourceId> {
    let (host, id) = match text.rsplit_once('/') {
        Some((host, id)) => (Some(host), id),
        None => (None, text),
    };
    let n = id.strip_prefix('@')?.parse::<u32>().ok()?;
    match host {
        None => Some(ResourceId::local(n)),
        Some(host)
            if !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')) =>
        {
            Some(ResourceId::satellite(host, n))
        }
        Some(_) => None,
    }
}

/// A session name: percent-decoded, not empty, no control characters.
fn name(text: &str) -> Option<String> {
    let name = percent_decode(text)?;
    (!name.is_empty() && !name.chars().any(char::is_control)).then_some(name)
}

fn tab(text: &str) -> Option<Link> {
    let (session, index) = text.rsplit_once('/')?;
    let index = index.parse::<usize>().ok().filter(|&n| n >= 1)?;
    Some(Link::Tab {
        session: name(session)?,
        index,
        since: None,
    })
}

/// An existing local directory, from the path after `phux://folder`.
fn folder(rest: &str) -> Result<Link, String> {
    let path = percent_decode(rest)
        .filter(|path| !path.contains('\0'))
        .map(|path| PathBuf::from(format!("/{path}")));
    match path {
        Some(path) if path.is_dir() => Ok(Link::Folder(path)),
        _ => Err(
            "This folder link doesn't name a folder on this machine, so nothing was opened."
                .to_owned(),
        ),
    }
}

/// The session showing `pane`, or why it can't be revealed.
fn locate(topology: &Topology, pane: &ResourceId) -> Result<String, String> {
    topology
        .pane(pane)
        .map(|p| p.session_name.clone())
        .ok_or_else(|| format!("No pane {pane} on this server, so nothing was revealed."))
}

fn session<'a>(topology: &'a Topology, name: &str) -> Result<&'a SessionDescriptor, String> {
    (topology.sessions.iter())
        .find(|s| s.name == name)
        .ok_or_else(|| format!("No session {name:?} on this server, so nothing was revealed."))
}

impl Desk {
    /// A link the system opened this app with: something to show, else a
    /// machine to pair from.
    pub fn open_system_link(&mut self, url: &str) {
        match target(url) {
            None => self.pair_from_link(url),
            Some(Err(why)) => self.say(NoticeKind::Error, why),
            Some(Ok(link)) => {
                let Some(client) = self.connection.client.as_ref() else {
                    self.say(NoticeKind::Error, "Connect to the server before opening this link.");
                    return;
                };
                let Some(origin) = self.connection.callback_fence() else {
                    return;
                };
                let intent = match &link {
                    Link::Pane(pane) => Some(super::view_intent::ViewIntent::new(
                        client,
                        pane,
                        self.connection.view_server.clone(),
                        self.connection.view_epoch,
                    )),
                    _ => None,
                };
                self.connection.view_choice += 1;
                self.connection.reveal = Some(Reveal {
                    link,
                    intent,
                    origin,
                    choice: self.connection.view_choice,
                });
                self.follow_reveal();
            }
        }
    }

    /// Show what a link asked for once the server's sessions are known.
    /// True while a tab link still waits for its session's tabs.
    pub(super) fn follow_reveal(&mut self) -> bool {
        let Some(reveal) = self.connection.reveal.take() else {
            return false;
        };
        if let Some(machine) = &self.machines.current {
            let why = format!(
                "This window is on {machine}; phux links name this machine's panes, sessions, \
                 tabs and folders."
            );
            self.say(NoticeKind::Error, why);
            return false;
        }
        if !reveal.current(&self.connection) {
            self.say(
                NoticeKind::Info,
                "Link cancelled: its original view scope is no longer current.",
            );
            return false;
        }
        let Some(topology) = &self.connection.topology else {
            self.connection.reveal = Some(reveal);
            return false;
        };
        let Reveal {
            link,
            intent,
            origin,
            choice,
        } = reveal;
        let step = match self.step(topology, link) {
            Ok(Step::Wait(link)) => {
                self.connection.reveal = Some(Reveal {
                    link,
                    intent,
                    origin,
                    choice,
                });
                return true;
            }
            Ok(step) => step,
            Err(why) => {
                self.say(NoticeKind::Error, why);
                return false;
            }
        };
        if let Some(window) = &self.machines.window {
            window.open();
        }
        match step {
            Step::Pane(pane, session) => self.show_revealed(pane, &session, intent),
            Step::Session(name) => self.show_session(&name),
            Step::Folder(path) => self.terminal_at_folder(path),
            Step::Wait(_) => {}
        }
        false
    }

    /// A pane link opens through the source it captured when it was opened;
    /// a tab link's pane is chosen now, in the scope already confirmed.
    fn show_revealed(
        &mut self,
        pane: ResourceId,
        session: &str,
        intent: Option<super::view_intent::ViewIntent>,
    ) {
        let Some(intent) = intent else {
            return self.show(pane, session);
        };
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        intent.select_terminal_session(&client, &pane);
        self.connection.view_intents.insert(pane.clone(), intent);
        self.connection.pending_session = None;
        self.want_automatic(pane);
        self.open_wanted(&client);
    }

    fn step(&self, topology: &Topology, link: Link) -> Result<Step, String> {
        match link {
            Link::Pane(pane) => locate(topology, &pane).map(|s| Step::Pane(pane, s)),
            Link::Session(name) => session(topology, &name).map(|_| Step::Session(name)),
            Link::Folder(path) => Ok(Step::Folder(path)),
            Link::Tab {
                session: name,
                index,
                since,
            } => {
                let found = session(topology, &name)?;
                let Some(tabs) = self.connection.tabs.by_session.get(&found.id) else {
                    if found.arrangement.is_some() && since.is_none_or(|t| t.elapsed() < TAB_WAIT) {
                        let since = Some(since.unwrap_or_else(Instant::now));
                        return Ok(Step::Wait(Link::Tab {
                            session: name,
                            index,
                            since,
                        }));
                    }
                    return Err(format!(
                        "Session {name:?}'s tabs didn't load, so nothing was revealed."
                    ));
                };
                tabs.get(index - 1)
                    .and_then(|tab| self.tab_focus(tab).or(tab.resources.first()).cloned())
                    .map(|pane| Step::Pane(pane, name.clone()))
                    .ok_or_else(|| {
                        format!("Session {name:?} has no tab {index}, so nothing was revealed.")
                    })
            }
        }
    }
}

#[cfg(test)]
#[path = "reveal/tests.rs"]
mod tests;
