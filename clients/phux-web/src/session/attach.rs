//! Choosing which session an untargeted connection attaches.
//!
//! A server advertising `ORGANIZATION` is asked for its organization page and
//! the connection opens the owner's designated Inbox session. Any other
//! server is asked for its state and the connection opens its most recent
//! session. Neither path creates a session while one exists.

use super::*;
use phux_protocol::ids::{SessionId, SessionKey};
use phux_protocol::wire::frame::{OrgFilter, OrganizationPage, StateScope};
use phux_protocol::wire::info::SessionSnapshot;

/// The session a connection opens when the page names none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttachPreference {
    /// The owner's Inbox session, where untargeted work lands. A server
    /// without `ORGANIZATION` has no Inbox; the most recent session stands in.
    #[default]
    Inbox,
    /// The most recently created session the server reports.
    MostRecent,
}

/// Where the connection is in choosing its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Discovery {
    /// `HELLO_OK` has not arrived.
    Idle,
    /// Waiting for the `GET_ORGANIZATION` page that names the Inbox.
    Organization(u32),
    /// Waiting for the `GET_STATE` snapshot that lists the sessions.
    State(u32),
    /// The `ATTACH` is on the wire.
    Attaching,
}

impl Connection {
    /// Prefer the Inbox or the most recent session for the attach that
    /// follows `HELLO_OK`. Has no effect once the attach is chosen.
    pub fn set_attach_preference(&mut self, preference: AttachPreference) {
        self.attach_preference = preference;
    }

    /// Whether the server advertised `ORGANIZATION` in `HELLO_OK`.
    #[must_use]
    pub const fn organization_supported(&self) -> bool {
        self.organization
    }

    /// The first request after `HELLO_OK`: the organization page when the
    /// server has one and the Inbox is wanted, otherwise the server state.
    pub(super) fn discovery_request(&mut self) -> Vec<u8> {
        let request_id = self.next_pane_request();
        if self.organization && self.attach_preference == AttachPreference::Inbox {
            self.discovery = Discovery::Organization(request_id);
            encode(&FrameKind::Command {
                request_id,
                command: Command::GetOrganization {
                    filter: OrgFilter::Intake,
                    page_size: 0,
                    cursor: None,
                },
            })
        } else {
            self.state_request()
        }
    }

    fn state_request(&mut self) -> Vec<u8> {
        let request_id = self.next_pane_request();
        self.discovery = Discovery::State(request_id);
        encode(&FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        })
    }

    /// Consume the reply to the discovery request, answering with the
    /// `ATTACH` (or the fallback request). Other frames pass through.
    pub(super) fn reduce_discovery_reply(&mut self, frame: &FrameKind) -> Option<Outcome> {
        let (request_id, result) = match frame {
            FrameKind::CommandResult { request_id, result } => (*request_id, Some(result)),
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } => (*request_id, None),
            _ => return None,
        };
        let send = match self.discovery {
            Discovery::Organization(pending) if pending == request_id => {
                match result {
                    Some(CommandResult::OkWith(CommandValue::OrganizationPage(page))) => {
                        self.inbox_attach(page)
                    }
                    // A refused or failed read (a grant without INVENTORY, for
                    // one) still leaves the legacy state path to try.
                    _ => None,
                }
                .unwrap_or_else(|| self.state_request())
            }
            Discovery::State(pending) if pending == request_id => match result {
                Some(CommandResult::OkWith(CommandValue::State(snapshot))) => {
                    self.state_attach(snapshot)
                }
                // Without a readable state, ask for the server's most recently
                // touched session. That target never creates one.
                _ => self.attach_frame(AttachTarget::Last),
            },
            _ => return None,
        };
        Some(Outcome {
            send: vec![send],
            ..Outcome::default()
        })
    }

    /// Attach the designated Inbox when the page names it and it is live on
    /// this server instance.
    fn inbox_attach(&mut self, page: &OrganizationPage) -> Option<Vec<u8>> {
        let inbox = page.inbox?;
        let live = page
            .sessions
            .iter()
            .find(|session| session.key == inbox)?
            .live?;
        self.session_key = Some(inbox);
        Some(self.attach_frame(AttachTarget::ById(live)))
    }

    fn state_attach(&mut self, snapshot: &SessionSnapshot) -> Vec<u8> {
        // `SessionInfo` carries no touch time, so creation time (then the
        // larger id) stands in for recency.
        let Some(recent) = snapshot
            .sessions
            .iter()
            .max_by_key(|session| (session.created_at_unix_secs, session.id.get()))
        else {
            // Legacy fallback: a server without ORGANIZATION has no Inbox, and
            // an empty one has nothing to attach. Creating "default" here
            // keeps a fresh pre-organization server usable from the browser;
            // a server with any session never reaches this branch.
            return self.attach_frame(AttachTarget::CreateIfMissing {
                name: "default".to_owned(),
                command: None,
                cwd: None,
            });
        };
        self.session_key = recent.key;
        let id: SessionId = recent.id;
        self.attach_frame(AttachTarget::ById(id))
    }

    fn attach_frame(&mut self, target: AttachTarget) -> Vec<u8> {
        self.discovery = Discovery::Attaching;
        encode(&FrameKind::Attach {
            attach_id: ATTACH_ID,
            target,
            viewport: self.viewport(),
            request_scrollback: true,
            scrollback_limit_lines: HISTORY_LINES,
            role_policy: None,
        })
    }

    /// Remember the attached session's key from the `ATTACHED` snapshot, for
    /// the organization commands a kill sends.
    pub(super) fn learn_session_key(&mut self, snapshot: &SessionSnapshot) {
        let key: Option<SessionKey> = snapshot
            .sessions
            .iter()
            .find(|session| session.id == snapshot.focused_session)
            .and_then(|session| session.key);
        if key.is_some() {
            self.session_key = key;
        }
    }
}
