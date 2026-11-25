//! `phux://pane/@N` (`phux://pane/HOST/@N` for a hub's satellite pane):
//! the running window shows that exact pane. The link reaches this
//! process the way pairing links do, through `open`/`xdg-open` and the
//! single-instance hand-off, so no second window opens. A pane that isn't
//! there is an error, never a nearby pane.

use phux_client_runtime::control::Topology;
use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind};

const PREFIX: &str = "phux://pane/";

pub(super) struct Reveal {
    pane: ResourceId,
    intent: super::view_intent::ViewIntent,
    origin: super::connection::CallbackFence,
    choice: u64,
}

/// The pane a reveal link names, `None` when `url` isn't one.
pub fn target(url: &str) -> Option<Result<ResourceId, String>> {
    let url = url.trim();
    let head = url.get(..PREFIX.len())?;
    if !head.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let rest = &url[PREFIX.len()..];
    Some(
        parse(rest.trim_end_matches('/')).ok_or_else(|| {
            format!("This pane link can't be used: {rest:?} isn't a pane id like @3.")
        }),
    )
}

fn parse(text: &str) -> Option<ResourceId> {
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

/// The session showing `pane`, or why it can't be revealed.
fn locate(topology: &Topology, pane: &ResourceId) -> Result<String, String> {
    topology
        .pane(pane)
        .map(|p| p.session_name.clone())
        .ok_or_else(|| format!("No pane {pane} on this server, so nothing was revealed."))
}

impl Desk {
    /// A link the system opened this app with: a pane to reveal, else one
    /// to pair from.
    pub fn open_system_link(&mut self, url: &str) {
        match target(url) {
            None => self.pair_from_link(url),
            Some(Err(why)) => self.say(NoticeKind::Error, why),
            Some(Ok(pane)) => {
                let Some(client) = self.connection.client.as_ref() else {
                    self.say(NoticeKind::Error, "Connect to the server before opening this pane link.");
                    return;
                };
                let Some(origin) = self.connection.callback_fence() else { return; };
                let intent = super::view_intent::ViewIntent::new(
                    client, &pane, self.connection.view_server.clone(), self.connection.view_epoch,
                );
                self.connection.view_choice += 1;
                self.connection.reveal = Some(Reveal { pane, intent, origin, choice: self.connection.view_choice });
                self.follow_reveal();
            }
        }
    }

    /// Show the pane a link asked for once the server's panes are known.
    pub(super) fn follow_reveal(&mut self) {
        if self.connection.reveal.is_none() {
            return;
        }
        if let Some(machine) = &self.machines.current {
            let why = format!("This window is on {machine}; pane links name this machine's panes.");
            self.connection.reveal = None;
            self.say(NoticeKind::Error, why);
            return;
        }
        let current = self.connection.reveal.as_ref().is_some_and(|reveal| {
            reveal.choice == self.connection.view_choice
                && if reveal.intent.has_source() {
                    reveal.origin.same_namespace(&self.connection)
                } else {
                    reveal.origin.is_current(&self.connection)
                }
        });
        if !current {
            self.connection.reveal = None;
            self.say(NoticeKind::Info, "Pane link cancelled: its original view scope is no longer current.");
            return;
        }
        let Some(topology) = &self.connection.topology else {
            return;
        };
        let Some(Reveal { pane, intent, .. }) = self.connection.reveal.take() else {
            return;
        };
        if let Some(window) = &self.machines.window {
            window.open();
        }
        match locate(topology, &pane) {
            Ok(_) => {
                let Some(client) = self.connection.client.clone() else { return; };
                intent.select_terminal_session(&client, &pane);
                self.connection.view_intents.insert(pane.clone(), intent);
                self.connection.pending_session = None;
                self.want_automatic(pane);
                self.open_wanted(&client);
            },
            Err(why) => self.say(NoticeKind::Error, why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;
    use phux_client_runtime::control::PaneDescriptor;

    fn pane(n: u32, session: &str) -> PaneDescriptor {
        PaneDescriptor {
            terminal_id: ResourceId::local(n),
            session_id: 1,
            session_name: session.to_owned(),
            cols: 80,
            rows: 24,
            title: None,
            cwd: None,
            is_focused: false,
            key: None,
            revision: None,
            name: None,
            activation: None,
            exec: None,
            foreground: None,
            icon: None,
        }
    }

    #[test]
    fn only_pane_links_are_reveals() {
        assert_eq!(target("phux://pane/@3"), Some(Ok(ResourceId::local(3))));
        assert_eq!(target("PHUX://pane/@3/"), Some(Ok(ResourceId::local(3))));
        let satellite = ResourceId::satellite("box:2222", 7);
        assert_eq!(target("phux://pane/box:2222/@7"), Some(Ok(satellite)));
        for bad in [
            "phux://pane/",
            "phux://pane/3",
            "phux://pane/@x",
            "phux://pane//@3",
        ] {
            assert!(matches!(target(bad), Some(Err(_))), "{bad}");
        }
        assert_eq!(target("phux://connect?quic=quic://mini:8788"), None);
    }

    #[test]
    fn a_pane_is_found_exactly_or_refused() {
        let graph = crate::desk::topology(Vec::new(), vec![pane(1, "work"), pane(2, "api")]);
        assert_eq!(locate(&graph, &ResourceId::local(2)), Ok("api".to_owned()));
        let gone = locate(&graph, &ResourceId::local(9)).unwrap_err();
        assert!(gone.contains("@9"), "{gone}");
    }

    #[test]
    fn a_link_waits_for_the_panes_then_fails_closed() {
        let mut desk = Desk::with_preferences(
            &Resolved::offline("reveal"),
            None,
            rune::prefs::Prefs::memory(),
        );
        desk.connection.client = Some(std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
            phux_client_runtime::control::ControlOptions::default(),
        )));
        desk.open_system_link("phux://pane/@9");
        assert_eq!(desk.connection.reveal.as_ref().map(|reveal| &reveal.pane), Some(&ResourceId::local(9)), "held with its original owner");
        assert!(desk.notice.is_none());
        desk.connection.topology = Some(crate::desk::topology(Vec::new(), vec![pane(1, "w")]));
        desk.follow_reveal();
        assert!(desk.connection.reveal.is_none());
        let notice = desk.notice.as_ref().map(|n| n.text.as_str());
        assert_eq!(
            notice,
            Some("No pane @9 on this server, so nothing was revealed.")
        );
        assert!(desk.connection.shown.is_none(), "no fallback pane");

        desk.machines.current = Some("mini".to_owned());
        desk.open_system_link("phux://pane/@1");
        assert!(desk.connection.reveal.is_none());
        let notice = desk.notice.as_ref().map(|n| n.text.as_str());
        assert!(notice.is_some_and(|n| n.contains("mini")), "{notice:?}");
    }
}

#[cfg(test)]
mod source_admission {
    use super::*;
    use crate::test_support::terminal::Replica;

    #[test]
    fn delayed_links_keep_their_original_source_and_first_source_epoch() {
        for pending in [false, true] {
            let replica = if pending { Replica::pending(80, 24, 2) } else { Replica::new(80, 24, 2) };
            let client = &replica.client;
            let mut desk = replica.window();
            let topology = desk.connection.topology.take();
            desk.open_system_link("phux://pane/@2");
            assert!(desk.connection.reveal.is_some());
            replica.replace_server(2);
            desk.connection.topology = topology;
            let _ = client.take_outbound();
            desk.follow_reveal();
            assert!(desk.connection.reveal.is_none());
            assert!(desk.connection.wanted.is_none());
            assert!(desk.connection.shown.is_none());
            assert!(client.take_outbound().is_empty());
        }
    }

    #[test]
    fn a_delayed_link_cannot_supersede_a_newer_focus_gesture() {
        let replica = Replica::pending(80, 24, 2);
        let mut desk = replica.window();
        let topology = desk.connection.topology.take();
        desk.open_system_link("phux://pane/@2");
        desk.want(ResourceId::local(1));
        desk.connection.topology = topology;
        desk.follow_reveal();
        assert!(desk.open_wanted(&replica.client));
        assert_eq!(desk.connection.shown.as_ref().map(|shown| &shown.resource), Some(&ResourceId::local(1)));
    }

    #[test]
    fn a_link_with_an_existing_source_can_open_the_retained_offline_replica() {
        let replica = Replica::new(80, 24, 1);
        let mut desk = replica.window();
        let topology = desk.connection.topology.take();
        desk.open_system_link("phux://pane/@1");
        replica.client.with_control(|control| control.connection_lost(None));
        desk.connection.topology = topology;
        desk.follow_reveal();
        let view = desk.connection.shown.as_ref().unwrap().view.unwrap();
        assert!(replica.client.acquire_view(view).is_some());
    }
}
