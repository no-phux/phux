//! CLI migration results are physical handles from the request's server,
//! not permission to select whatever now reuses their numbers.

use super::{Done, Migrate, Migration, SLOW, words};
use crate::desk::{Desk, NoticeKind, connection::CallbackFence, view_intent::ViewIntent};
use phux_protocol::{ids::ResourceId, wire::frame::AttachTarget};

pub(super) struct Scope {
    origin: CallbackFence,
    server: Option<Vec<u8>>,
    epoch: u64,
}

pub(super) struct Reply {
    agent: String,
    result: Result<Migration, String>,
    scope: Option<Scope>,
    choice: u64,
}

impl Desk {
    /// Start the requested migration; view interest keeps its original scope.
    pub fn migrate_with(&mut self, agent: &'static str) {
        if matches!(self.onboarding.migrate, Migrate::Starting(_) | Migrate::Running(..)) {
            return;
        }
        self.connection.view_choice += 1;
        let choice = self.connection.view_choice;
        self.onboarding.migration_choice = Some(choice);
        let scope = self.connection.callback_fence()
            .filter(|_| self.machines.current.is_none())
            .map(|origin| Scope {
                origin,
                server: self.connection.view_server.clone(),
                epoch: self.connection.view_epoch,
            });
        self.onboarding.migrate = Migrate::Starting(agent.to_owned());
        self.ask(&["migrate", "--json", "--agent", agent], SLOW, move |out| {
            Done::Migrated(Reply {
                agent: agent.to_owned(),
                result: out.and_then(|bytes| words::parse_migration(&bytes)),
                scope,
                choice,
            })
        });
    }

    /// Explicit execution termination is separate from retiring a stale view.
    pub fn cancel_migration(&mut self) {
        if let Migrate::Running(_, Some(pane)) = &self.onboarding.migrate
            && let Some(scope) = self.onboarding.migration_scope.as_ref()
            && scope.origin.is_current(&self.connection)
            && let Some(client) = self.connection.client.as_ref()
        {
            let request = client.with_control(|control| {
                if control.connection_epoch() == scope.epoch
                    && control.server().map(|server| server.id.as_slice()) == scope.server.as_deref()
                {
                    control.close_terminals(vec![pane.clone()])
                } else {
                    None
                }
            });
            if !self.report_close_submission(request) {
                return;
            }
        }
        self.connection.view_choice += 1;
        self.onboarding.migration_scope = None;
        self.onboarding.migration_choice = None;
        self.onboarding.migrate = Migrate::Choosing;
    }

    pub(super) fn finish_migration(&mut self, reply: Reply) {
        if self.onboarding.migration_choice != Some(reply.choice)
            || !matches!(&self.onboarding.migrate, Migrate::Starting(agent) if *agent == reply.agent)
        {
            return;
        }
        self.onboarding.migration_choice = None;
        let migration = match reply.result {
            Ok(migration) => migration,
            Err(error) => {
                self.onboarding.migrate = Migrate::Failed(if words::unsupported(&error) {
                    "Update phux to enable this.".to_owned()
                } else { error });
                return;
            }
        };
        if reply.choice != self.connection.view_choice {
            self.onboarding.migrate = Migrate::Running(reply.agent, None);
            return;
        }
        let Some(scope) = reply.scope.filter(|scope| scope.origin.is_current(&self.connection)) else {
            self.onboarding.migrate = Migrate::Running(reply.agent, None);
            self.say(NoticeKind::Info, "The migration may be running, but its original server view is no longer available.");
            return;
        };
        let Some(client) = self.connection.client.clone() else { return; };
        let pane = migration.pane.map(ResourceId::local);
        if let Some(pane) = pane.as_ref() {
            let intent = ViewIntent::new(&client, pane, scope.server.clone(), scope.epoch);
            if intent.refused() {
                self.onboarding.migrate = Migrate::Running(reply.agent, None);
                return;
            }
            intent.select_terminal_session(&client, pane);
            self.connection.view_intents.insert(pane.clone(), intent);
            self.connection.pending_session = None;
            self.want_automatic(pane.clone());
            self.open_wanted(&client);
        } else if let Some(session) = migration.session {
            let selected = client.with_control(|control| {
                if control.status() != phux_client_runtime::control::Status::Attached
                    || control.connection_epoch() != scope.epoch
                    || control.server().map(|server| server.id.as_slice()) != scope.server.as_deref()
                {
                    return false;
                }
                let Some(id) = control.topology()
                    .and_then(|topology| topology.session_named(&session)).map(|session| session.id) else {
                    return false;
                };
                control.attach_session(AttachTarget::ById(phux_protocol::ids::SessionId::new(id)));
                control.selected_session() == Some(id)
            });
            if selected {
                self.wait_for_session(session);
            } else {
                self.say(NoticeKind::Info, "Migration returned, but its session view is unavailable on this connection.");
            }
        }
        self.onboarding.migration_scope = Some(scope);
        self.onboarding.migrate = Migrate::Running(reply.agent, pane);
    }
}
