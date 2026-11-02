//! Which session the window selects. The client connects with no attach, so
//! a server with nothing to open still gives a working window; this picks
//! once the server's inventory is in, and again whenever the selected session
//! is gone. Ported from the GPUI client's `home::choose`.

use phux_client_runtime::Client;
use phux_client_runtime::control::{SessionDescriptor, SessionRecovery, Status, Topology};
use phux_protocol::ids::SessionKey;
use phux_protocol::wire::frame::AttachTarget;

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
/// by `inbox`); else the most recently created session.
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
        .max_by_key(|session| (session.created_at_unix_secs, session.id))
        .map_or(Home::Nothing, |session| {
            Home::Existing(session.name.clone())
        })
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
        if self.status != Status::Attached {
            return false;
        }
        let Some(topology) = &self.topology else {
            return false;
        };
        let selected = client.selected_session();
        if let Some(session) = topology.sessions.iter().find(|s| Some(s.id) == selected) {
            self.link.home = Some((session.name.clone(), session.key));
            if std::mem::take(&mut self.link.creating) {
                client.with_control(|c| c.set_session_recovery(SessionRecovery::CallerValidated));
            }
            return false;
        }
        if self.pending_session.is_some() || !link::seeded(client, self.link.org_unavailable) {
            return false;
        }
        let back = self.link.home.as_ref().and_then(|(name, key)| {
            returning(topology, name, *key).map(|session| session.name.clone())
        });
        let home = back.map_or_else(
            || {
                // Asked for once: a killed session isn't made again.
                let requested = self.launch.session.take();
                choose(
                    requested.as_deref(),
                    topology,
                    inbox_name(client, topology).as_deref(),
                )
            },
            Home::Existing,
        );
        match home {
            Home::Existing(name) => client.attach_session(AttachTarget::ByName(name)),
            Home::Missing(name) => self.create_session(name),
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
    use phux_client_core::layout::wire::HandleMap;
    use phux_protocol::ids::ResourceId;
    use phux_protocol::wire::frame::SessionLifetime;

    fn session(id: u32, name: &str, created: i64) -> SessionDescriptor {
        SessionDescriptor {
            id,
            name: name.to_owned(),
            attached_client_count: 0,
            keep_empty: true,
            created_at_unix_secs: created,
            key: None,
            lifetime: Some(SessionLifetime::Persistent),
            revision: None,
            members: 1,
            arrangement: None,
            awaiting_organization: false,
        }
    }

    fn topology(sessions: Vec<SessionDescriptor>) -> Topology {
        Topology {
            sessions,
            panes: Vec::new(),
            agent_sessions: Vec::new(),
            focused_session: 0,
            focused_pane: ResourceId::local(0),
            authority: None,
            inbox: None,
            handles: HandleMap::new(),
        }
    }

    #[test]
    fn an_explicit_name_wins_and_is_never_replaced() {
        let graph = topology(vec![session(1, "work", 10), session(2, "inbox", 5)]);
        assert_eq!(
            choose(Some("work"), &graph, Some("inbox")),
            Home::Existing("work".into())
        );
        assert_eq!(
            choose(Some("new"), &graph, Some("inbox")),
            Home::Missing("new".into())
        );
    }

    #[test]
    fn untargeted_opens_the_inbox_then_the_most_recent_session() {
        let graph = topology(vec![
            session(1, "old", 10),
            session(2, "recent", 30),
            session(3, "inbox", 20),
        ]);
        assert_eq!(
            choose(None, &graph, Some("inbox")),
            Home::Existing("inbox".into())
        );
        assert_eq!(choose(None, &graph, None), Home::Existing("recent".into()));
        assert_eq!(
            choose(Some("  "), &graph, None),
            Home::Existing("recent".into())
        );
    }

    #[test]
    fn an_unlisted_inbox_gives_way() {
        let graph = topology(vec![session(1, "work", 10)]);
        assert_eq!(
            choose(None, &graph, Some("inbox")),
            Home::Existing("work".into())
        );
    }

    #[test]
    fn nothing_to_open_never_invents_a_default() {
        assert_eq!(choose(None, &topology(Vec::new()), None), Home::Nothing);
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
}
