//! Which session the window selects. The client connects with no attach, so
//! a server with nothing to open still gives a working window; this picks
//! once the server's inventory is in, and again whenever the selected session
//! is gone.

use phux_client_runtime::Client;
use phux_client_runtime::control::{SessionDescriptor, Status, Topology};
use phux_protocol::ids::SessionKey;

use super::{Desk, link};

/// What the server offers to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// Select this existing session.
    Existing(String),
    /// The explicitly requested session does not exist yet.
    Missing(String),
    /// No session was named and the server has neither an Intake nor a session.
    Nothing,
}

/// The explicit request when it exists or is missing; else the Intake (named
/// by `inbox`); else the most recently created session that isn't the quick
/// terminal's.
pub fn choose(requested: Option<&str>, topology: &Topology, inbox: Option<&str>) -> Home {
    if let Some(name) = requested.map(str::trim).filter(|name| !name.is_empty()) {
        return if topology.session_named(name).is_some() {
            Home::Existing(name.to_owned())
        } else {
            Home::Missing(name.to_owned())
        };
    }
    // Only a listed session can be selected on a live connection.
    if let Some(inbox) = inbox.filter(|name| topology.session_named(name).is_some()) {
        return Home::Existing(inbox.to_owned());
    }
    topology
        .sessions
        .iter()
        .filter(|session| session.name != super::quick::SESSION)
        .max_by_key(|session| (session.created_at_unix_secs, session.id))
        .map_or(Home::Nothing, |session| {
            Home::Existing(session.name.clone())
        })
}

/// Where the phone app last was. It has no launch arguments to name a
/// session, and should reopen where it left off, not wherever is newest.
const LAST_SESSION: &str = "last_session";

/// The remembered session, while the server still has it: one that is gone
/// is never made again, and falls through to the usual choice.
fn remembered(last: Option<String>, topology: &Topology) -> Option<String> {
    last.filter(|name| topology.session_named(name).is_some())
}

/// The session a reconnect returns to. A persistent key wins over a reused
/// name, and a key that's gone never falls back to the name.
fn returning<'a>(
    topology: &'a Topology,
    name: &str,
    key: Option<SessionKey>,
) -> Option<&'a SessionDescriptor> {
    match key {
        Some(key) => topology.session_keyed(&key),
        None => topology.session_named(name),
    }
}

impl Desk {
    /// Select a session when none is selected. True when that changed
    /// anything on screen.
    pub(super) fn settle_home(&mut self, client: &Client) -> bool {
        if self.connection.fleet_handoff.is_some() {
            return self.settle_fleet_handoff(client);
        }
        if !self.connection.owns_client(client) {
            return false;
        }
        if self.connection.status != Status::Attached {
            return false;
        }
        if self.settle_bare(client) {
            return true;
        }
        let Some(topology) = &self.connection.topology else {
            return false;
        };
        if self.link.creating.is_some() || self.link.uncertain_session.is_some() {
            return false;
        }
        if let Some(key) = self.link.created {
            let Some(session) = topology
                .sessions
                .iter()
                .find(|session| session.key == Some(key))
            else {
                return false;
            };
            if client.selected_session() != Some(session.id) {
                return super::view_intent::select_keyed_session(
                    client,
                    self.connection.view_server.as_deref(),
                    self.connection.view_epoch,
                    &key,
                );
            }
        }
        let selected = client.selected_session();
        if let Some(session) = topology.sessions.iter().find(|s| Some(s.id) == selected) {
            let moved = self.link.home.as_ref().map(|(name, _)| name) != Some(&session.name);
            if moved && self.native.is_some() {
                let _ = self.prefs.set(LAST_SESSION, &session.name);
            }
            self.link.home = Some((session.name.clone(), session.key));
            return false;
        }
        if self.connection.pending_session.is_some()
            || !link::seeded(client, self.link.org_unavailable)
        {
            return false;
        }
        let back = self.link.home.as_ref().and_then(|(name, key)| {
            returning(topology, name, *key).map(|session| session.name.clone())
        });
        let home = back.map_or_else(
            || {
                // Asked for once: a killed session isn't made again.
                let last = self.native.as_ref().and(self.prefs.get(LAST_SESSION));
                // The quick terminal's session is made again whenever it ends.
                let requested = match self.quick {
                    Some(_) => Some(super::quick::SESSION.to_owned()),
                    None => self.launch.session.take(),
                };
                let requested = requested.or_else(|| remembered(last, topology));
                choose(
                    requested.as_deref(),
                    topology,
                    inbox_name(client, topology).as_deref(),
                )
            },
            Home::Existing,
        );
        match home {
            Home::Existing(name) => {
                self.connection.select_named_session(client, &name);
            }
            Home::Missing(name) => self.ensure_session(name),
            Home::Nothing => return false,
        }
        true
    }
}

/// The Intake session's name: from the topology when it lists the Intake,
/// else from the organization replica's record.
fn inbox_name(client: &Client, topology: &Topology) -> Option<String> {
    let key = topology.inbox?;
    topology
        .session_keyed(&key)
        .map(|session| session.name.clone())
        .or_else(|| client.organization().session(&key).map(|s| s.name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use phux_protocol::wire::frame::SessionLifetime;
    fn session(id: u32, name: &str, created: i64) -> SessionDescriptor {
        SessionDescriptor {
            id,
            name: name.to_owned(),
            attached_client_count: 0,
            keep_empty: true,
            created_at_unix_secs: created,
            key: None,
            touched: None,
            lifetime: Some(SessionLifetime::Persistent),
            revision: None,
            members: 1,
            arrangement: None,
            awaiting_organization: false,
            project: None,
            project_link: None,
        }
    }

    fn topology(sessions: Vec<SessionDescriptor>) -> Topology {
        crate::desk::topology(sessions, Vec::new())
    }

    #[test]
    fn the_home_session_is_chosen_by_name_inbox_then_recency() {
        let existing = |name: &str| Home::Existing(name.into());
        let pair = [(1, "work", 10), (2, "inbox", 5)];
        let three = [(1, "old", 10), (2, "recent", 30), (3, "inbox", 20)];
        let quick = [(1, "work", 10), (2, "quick", 30)];
        // (label, sessions as (id, name, created), named, inbox, expected)
        type Case<'a> = (
            &'a str,
            &'a [(u32, &'a str, i64)],
            Option<&'a str>,
            Option<&'a str>,
            Home,
        );
        let cases: &[Case] = &[
            (
                "an explicit name wins",
                &pair,
                Some("work"),
                Some("inbox"),
                existing("work"),
            ),
            (
                "an explicit name is never replaced",
                &pair,
                Some("new"),
                Some("inbox"),
                Home::Missing("new".into()),
            ),
            (
                "untargeted opens the inbox",
                &three,
                None,
                Some("inbox"),
                existing("inbox"),
            ),
            (
                "then the most recent",
                &three,
                None,
                None,
                existing("recent"),
            ),
            (
                "a blank name is untargeted",
                &three,
                Some("  "),
                None,
                existing("recent"),
            ),
            (
                "an unlisted inbox gives way",
                &pair[..1],
                None,
                Some("inbox"),
                existing("work"),
            ),
            (
                "never the quick session",
                &quick,
                None,
                None,
                existing("work"),
            ),
            (
                "quick asked for by name opens",
                &quick,
                Some("quick"),
                None,
                existing("quick"),
            ),
            (
                "nothing to open never invents a default",
                &[],
                None,
                None,
                Home::Nothing,
            ),
        ];
        for (label, sessions, named, inbox, expected) in cases {
            let graph = topology(
                sessions
                    .iter()
                    .map(|&(id, name, created)| session(id, name, created))
                    .collect(),
            );
            assert_eq!(choose(*named, &graph, *inbox), *expected, "{label}");
        }
    }

    #[test]
    fn the_main_window_lists_no_quick_session() {
        let graph = topology(vec![session(1, "work", 10), session(2, "quick", 30)]);
        let items = crate::palette::session_items(Some(&graph), &[]);
        let titles: Vec<_> = items.into_iter().map(|item| item.title).collect();
        assert_eq!(titles, ["work"], "the palette");
        let mut desk = crate::test_support::settings_window();
        desk.connection.topology = Some(graph);
        let names: Vec<_> = desk
            .ordered_sessions()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, ["work"], "the sidebar and session stepping");
    }

    #[test]
    fn a_reconnect_follows_the_key_and_never_a_reused_name() {
        let key = SessionKey::new([1; 16]).unwrap();
        let mut renamed = session(1, "renamed", 10);
        renamed.key = Some(key);
        let graph = topology(vec![renamed, session(2, "home", 20)]);
        assert_eq!(returning(&graph, "home", Some(key)).map(|s| s.id), Some(1));
        let other = SessionKey::new([2; 16]).unwrap();
        assert!(returning(&graph, "home", Some(other)).is_none());
        assert_eq!(returning(&graph, "home", None).map(|s| s.id), Some(2));
    }

    #[test]
    fn the_phone_reopens_its_last_session_only_while_it_exists() {
        let topology = topology(vec![session(1, "work", 10), session(2, "new", 20)]);
        let last = |name: &str| remembered(Some(name.to_owned()), &topology);
        assert_eq!(last("work"), Some("work".to_owned()));
        assert_eq!(last("gone"), None);
        assert_eq!(remembered(None, &topology), None);
        // It outranks the newest session as a named launch does, and one
        // that is gone is not made again.
        assert_eq!(
            choose(last("work").as_deref(), &topology, None),
            Home::Existing("work".to_owned())
        );
        assert_eq!(
            choose(last("gone").as_deref(), &topology, None),
            Home::Existing("new".to_owned())
        );
    }
}
