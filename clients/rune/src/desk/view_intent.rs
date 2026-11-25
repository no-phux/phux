//! A presentation request keeps its observed server and, once available,
//! its exact publication. A retry never upgrades that authority.

use std::sync::{Arc, Weak};
use phux_client_runtime::publication::Publication;
use phux_client_runtime::{Client, ViewId, engine::{EngineError, ViewSource}};
use phux_protocol::ids::ResourceId;
use phux_client_runtime::control::Status;
use phux_protocol::wire::frame::AttachTarget;

#[derive(Debug)]
pub(super) struct ViewIntent {
    server: Option<Vec<u8>>,
    epoch: u64,
    owner: Weak<Publication>,
    source: Option<ViewSource>,
    refused: bool,
}

impl ViewIntent {
    pub(super) fn new(client: &Client, terminal: &ResourceId, server: Option<Vec<u8>>, epoch: u64) -> Self {
        let mut intent = Self {
            server, epoch, owner: Arc::downgrade(client.publication()), source: None, refused: false,
        };
        intent.capture(client, terminal);
        intent
    }
    #[cfg(test)]
    pub(super) fn from_source(client: &Client, server: Vec<u8>, epoch: u64, source: ViewSource) -> Self {
        Self { server: Some(server), epoch, owner: Arc::downgrade(client.publication()), source: Some(source), refused: false }
    }
    pub(super) fn owns_client(&self, client: &Client) -> bool {
        Weak::ptr_eq(&self.owner, &Arc::downgrade(client.publication()))
    }
    pub(super) fn has_source(&self) -> bool {
        self.source.is_some()
    }




    fn capture(&mut self, client: &Client, terminal: &ResourceId) {
        if !Weak::ptr_eq(&self.owner, &Arc::downgrade(client.publication())) {
            self.refused = true;
            self.source = None;
            return;
        }
        if self.source.is_some() || self.refused {
            return;
        }
        match client.view_source(terminal, self.server.as_deref(), self.epoch) {
            Ok(source) => self.source = Some(source),
            Err(EngineError::ProjectionUnavailable) => {},
            Err(_) => self.refused = true,
        }
    }

    pub(super) fn open(&mut self, client: &Client, terminal: &ResourceId) -> Option<ViewId> {
        self.capture(client, terminal);
        if self.refused {
            return None;
        }
        let source = self.source.as_ref()?;
        match client.create_view(source) {
            Ok(view) => Some(view),
            Err(EngineError::ProjectionUnavailable) => None,
            Err(_) => {
                self.refused = true;
                self.source = None;
                None
            }
        }
    }
    pub(super) fn subscribe(&self, client: &Client, terminal: &ResourceId) {
        if self.refused || self.source.is_some()
            || !Weak::ptr_eq(&self.owner, &Arc::downgrade(client.publication()))
        {
            return;
        }
        client.with_control(|control| {
            if control.status() == Status::Attached && control.connection_epoch() == self.epoch
                && control.server().map(|server| server.id.as_slice()) == self.server.as_deref()
            {
                control.attach_terminal(terminal);
            }
        });
    }

    /// Session selection follows the exact terminal in this id space, never
    /// an old session name that may now denote another session.
    pub(super) fn select_terminal_session(&self, client: &Client, terminal: &ResourceId) {
        if self.refused || terminal.local_id().is_none() || !self.owns_client(client) {
            return;
        }
        client.with_control(|control| {
            if control.status() == Status::Attached && control.connection_epoch() == self.epoch
                && control.server().map(|server| server.id.as_slice()) == self.server.as_deref()
                && let Some(session) = control.topology()
                    .and_then(|topology| topology.pane(terminal)).map(|pane| pane.session_id)
                && control.selected_session() != Some(session)
            {
                control.attach_session(AttachTarget::ById(phux_protocol::ids::SessionId::new(session)));
            }
        });
    }

    /// A move selects its explicit durable destination, not the terminal's
    /// possibly still-unrefreshed previous membership.
    pub(super) fn select_session(&self, client: &Client, session: &phux_protocol::ids::SessionKey) -> bool {
        if self.refused || !self.owns_client(client) {
            return false;
        }
        select_keyed_session(client, self.server.as_deref(), self.epoch, session)
    }


    pub(super) const fn refused(&self) -> bool {
        self.refused
    }
}

/// Exact destination selection shared by source-owning UI continuations.
pub(super) fn select_keyed_session(
    client: &Client, server: Option<&[u8]>, epoch: u64, session: &phux_protocol::ids::SessionKey,
) -> bool {
    client.with_control(|control| {
        if control.status() != Status::Attached || control.connection_epoch() != epoch
            || control.server().map(|server| server.id.as_slice()) != server {
            return false;
        }
        let Some(id) = control.topology().and_then(|topology| topology.session_keyed(session))
            .map(|session| session.id) else { return false };
        if control.selected_session() != Some(id) {
            control.attach_session(AttachTarget::ById(phux_protocol::ids::SessionId::new(id)));
        }
        control.selected_session() == Some(id)
    })
}
