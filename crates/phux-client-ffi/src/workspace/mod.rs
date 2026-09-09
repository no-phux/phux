//! Shared native workspace over protocol 0.8 `GET_STATE` and opaque L3 metadata.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher"
)]

mod exports;
mod model;
mod mutation;
mod types;
pub use exports::*;
pub use types::*;

use crate::{client::Client, error::BridgeError};
use model::{Catalog, Node};
use phux_client_core::layout::Workspace;
use phux_protocol::GroupId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, Scope, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;

pub(crate) const INTERNAL_START: u32 = 0x8000_0000;

struct Pending {
    request: u32,
    state_id: Option<u32>,
    metadata_id: u32,
    set_id: Option<u32>,
    catalog: Option<Catalog>,
    metadata: Option<MetadataReply>,
    proposed: Option<Workspace>,
}

enum MetadataReply {
    Absent,
    Present(Vec<u8>),
}

impl MetadataReply {
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Absent => None,
            Self::Present(bytes) => Some(bytes),
        }
    }
}

pub(crate) struct SharedWorkspace {
    selected: u32,
    pub(super) revision: u64,
    pub(super) state: u32,
    authoritative: bool,
    pub(super) request: u32,
    pub(super) status: u32,
    pub(super) message: Vec<u8>,
    catalog: Catalog,
    topology: Workspace,
    nodes: Vec<Node>,
    roots: Vec<u32>,
    pending: Option<Pending>,
    next_internal: u32,
}

impl Default for SharedWorkspace {
    fn default() -> Self {
        Self {
            selected: 0,
            revision: 0,
            state: 0,
            authoritative: false,
            request: 0,
            status: 0,
            message: Vec::new(),
            catalog: Catalog::default(),
            topology: Workspace::new(),
            nodes: Vec::new(),
            roots: Vec::new(),
            pending: None,
            next_internal: INTERNAL_START,
        }
    }
}

impl SharedWorkspace {
    fn reserve_internal(&mut self) -> Result<u32, BridgeError> {
        let id = self.next_internal;
        self.next_internal = id
            .checked_add(1)
            .ok_or_else(|| BridgeError::state("workspace request space exhausted"))?;
        Ok(id)
    }

    pub(crate) fn disconnect(&mut self) {
        if self.pending.take().is_some() {
            self.status = 4;
            self.message = b"connection ended; workspace transaction outcome unknown".to_vec();
        }
    }

    fn fail(&mut self, error: &BridgeError) {
        self.pending = None;
        self.state = 3;
        self.status = 3;
        let mut end = error.message.len().min(model::MAX_TEXT);
        while !error.message.is_char_boundary(end) {
            end -= 1;
        }
        self.message = error.message.as_bytes()[..end].to_vec();
    }

    fn begin(&mut self, pending: Pending) {
        self.request = pending.request;
        self.status = 1;
        self.message.clear();
        self.pending = Some(pending);
    }

    fn busy(&self) -> Result<(), BridgeError> {
        if self.pending.is_some() {
            return Err(BridgeError::state("workspace transaction already pending"));
        }
        if self.selected == 0 {
            return Err(BridgeError::state("no attached workspace session"));
        }
        Ok(())
    }
}

pub(crate) fn attached(client: &mut Client, snapshot: SessionSnapshot) {
    let selected = snapshot.focused_session.get();
    client.workspace.selected = selected;
    // ATTACHED carries registry data, not evidence that shared metadata is absent.
    // Leave topology unavailable until the initial metadata/state pair completes.
    let result = Catalog::from_snapshot(snapshot, selected)
        .and_then(|catalog| publish_catalog(client, catalog));
    if let Err(error) = result {
        client.workspace.fail(&error);
    }
}

pub(crate) fn initial_read(client: &mut Client) {
    let result = start_read(client, 0, None);
    if let Err(error) = result {
        client.workspace.fail(&error);
    }
}

const fn state_frame(id: u32) -> FrameKind {
    FrameKind::Command {
        request_id: id,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    }
}

fn metadata_frame(id: u32, selected: u32) -> FrameKind {
    FrameKind::GetMetadata {
        request_id: id,
        scope: Scope::Group(GroupId::new(1)),
        key: model::key(selected),
    }
}

fn start_read(
    client: &mut Client,
    request: u32,
    proposed: Option<(Workspace, Vec<u8>)>,
) -> Result<(), BridgeError> {
    client.workspace.busy()?;
    if client.outgoing.len() > 125 {
        return Err(BridgeError::state("outgoing queue full"));
    }
    let state_id = client.workspace.reserve_internal()?;
    let metadata_id = client.workspace.reserve_internal()?;
    let set_id = queue_proposed(client, proposed.as_ref())?;
    // SET has no success reply in protocol 0.8. Read metadata before registry:
    // pruning against an older registry could falsely retire newly created leaves.
    client.queue_frame(&metadata_frame(metadata_id, client.workspace.selected))?;
    client.queue_frame(&state_frame(state_id))?;
    client.workspace.begin(Pending {
        request,
        state_id: Some(state_id),
        metadata_id,
        set_id,
        catalog: None,
        metadata: None,
        proposed: proposed.map(|(workspace, _)| workspace),
    });
    Ok(())
}

fn queue_proposed(
    client: &mut Client,
    proposed: Option<&(Workspace, Vec<u8>)>,
) -> Result<Option<u32>, BridgeError> {
    let Some((_, bytes)) = proposed else {
        return Ok(None);
    };
    let id = client.workspace.reserve_internal()?;
    client.queue_frame(&FrameKind::SetMetadata {
        request_id: id,
        scope: Scope::Group(GroupId::new(1)),
        key: model::key(client.workspace.selected),
        value: bytes.clone(),
    })?;
    Ok(Some(id))
}

pub(crate) fn dispatch(client: &mut Client, frame: FrameKind) -> Option<FrameKind> {
    match frame {
        FrameKind::CommandResult { request_id, result } if request_id >= INTERNAL_START => {
            if let Err(error) = receive_state(client, request_id, result) {
                client.workspace.fail(&error);
            }
        }
        FrameKind::MetadataValue { request_id, value } if request_id >= INTERNAL_START => {
            if let Err(error) = receive_metadata(client, request_id, value.as_deref()) {
                client.workspace.fail(&error);
            }
        }
        FrameKind::Error {
            request_id: Some(request_id),
            message,
            ..
        } if request_id >= INTERNAL_START => {
            if pending_id(&client.workspace, request_id) {
                client.workspace.fail(&BridgeError::state(message));
            }
        }
        FrameKind::TerminalSpawned { request_id, .. } if request_id >= INTERNAL_START => {
            if pending_id(&client.workspace, request_id) {
                client.workspace.fail(&BridgeError::invalid(
                    "spawn reply cannot answer a workspace read",
                ));
            }
        }
        other => return Some(other),
    }
    None
}

fn pending_id(workspace: &SharedWorkspace, id: u32) -> bool {
    workspace
        .pending
        .as_ref()
        .is_some_and(|p| p.state_id == Some(id) || p.metadata_id == id || p.set_id == Some(id))
}

fn receive_state(client: &mut Client, id: u32, result: CommandResult) -> Result<(), BridgeError> {
    let Some(pending) = client.workspace.pending.as_ref() else {
        return Ok(());
    };
    if pending.state_id != Some(id) {
        return Ok(());
    }
    let snapshot = match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        CommandResult::Error { message, .. } => return Err(BridgeError::state(message)),
        _ => return Err(BridgeError::invalid("unexpected workspace GET_STATE reply")),
    };
    let catalog = Catalog::from_snapshot(snapshot, client.workspace.selected)?;
    if let Some(pending) = client.workspace.pending.as_mut() {
        pending.state_id = None;
        pending.catalog = Some(catalog);
    }
    finish_read(client)
}

fn receive_metadata(client: &mut Client, id: u32, bytes: Option<&[u8]>) -> Result<(), BridgeError> {
    let Some(pending) = client.workspace.pending.as_ref() else {
        return Ok(());
    };
    if pending.metadata_id != id || pending.metadata.is_some() {
        return Ok(());
    }
    if bytes.is_some_and(|bytes| bytes.len() > 256 * 1024) {
        return Err(BridgeError::state("layout metadata exceeds 256 KiB"));
    }
    if let Some(pending) = client.workspace.pending.as_mut() {
        pending.metadata = Some(bytes.map_or(MetadataReply::Absent, |bytes| {
            MetadataReply::Present(bytes.to_vec())
        }));
    }
    finish_read(client)
}

fn finish_read(client: &mut Client) -> Result<(), BridgeError> {
    if !client
        .workspace
        .pending
        .as_ref()
        .is_some_and(|p| p.catalog.is_some() && p.metadata.is_some())
    {
        return Ok(());
    }
    let pending = client
        .workspace
        .pending
        .take()
        .ok_or_else(|| BridgeError::state("missing workspace transaction"))?;
    complete_snapshot(client, pending)
}

fn complete_snapshot(client: &mut Client, pending: Pending) -> Result<(), BridgeError> {
    let catalog = pending
        .catalog
        .ok_or_else(|| BridgeError::state("missing workspace registry"))?;
    let bytes = pending.metadata.as_ref().and_then(MetadataReply::bytes);
    let topology = match model::adopt(
        bytes,
        &client.workspace.topology,
        &catalog,
        client.workspace.selected,
    ) {
        Ok(topology) => topology,
        Err(error) => {
            publish_catalog(client, catalog)?;
            return Err(error);
        }
    };
    let won = pending
        .proposed
        .as_ref()
        .is_none_or(|proposed| topology_equal(proposed, &topology));
    publish(client, catalog, topology, bytes.is_some())?;
    client.workspace.status = if won { 2 } else { 3 };
    if !won {
        client.workspace.message =
            b"concurrent layout write won confirmation; adopted current shared topology".to_vec();
    }
    Ok(())
}

fn topology_equal(a: &Workspace, b: &Workspace) -> bool {
    a.windows.len() == b.windows.len()
        && a.windows
            .iter()
            .zip(&b.windows)
            .all(|(a, b)| a.id == b.id && a.name == b.name && a.state.tree == b.state.tree)
}

fn catalog_equal(a: &Catalog, b: &Catalog) -> bool {
    a.terminals == b.terminals
        && a.sessions.len() == b.sessions.len()
        && a.sessions.iter().zip(&b.sessions).all(|(a, b)| {
            a.session_id == b.session_id
                && a.name == b.name
                && a.created_at_unix_secs == b.created_at_unix_secs
                && a.window_count == b.window_count
        })
}

fn publish_catalog(client: &mut Client, catalog: Catalog) -> Result<(), BridgeError> {
    let ws = &mut client.workspace;
    if !catalog_equal(&ws.catalog, &catalog) || ws.revision == 0 {
        ws.revision = ws
            .revision
            .checked_add(1)
            .ok_or_else(|| BridgeError::state("workspace revision exhausted"))?;
    }
    client.sessions.clone_from(&catalog.sessions);
    ws.catalog = catalog;
    Ok(())
}

fn publish(
    client: &mut Client,
    catalog: Catalog,
    topology: Workspace,
    authoritative: bool,
) -> Result<(), BridgeError> {
    let (nodes, roots) = model::flatten(&topology)?;
    let ws = &mut client.workspace;
    if !topology_equal(&ws.topology, &topology)
        || !catalog_equal(&ws.catalog, &catalog)
        || ws.authoritative != authoritative
        || ws.state == 0
        || ws.revision == 0
    {
        ws.revision = ws
            .revision
            .checked_add(1)
            .ok_or_else(|| BridgeError::state("workspace revision exhausted"))?;
    }
    client.sessions.clone_from(&catalog.sessions);
    ws.catalog = catalog;
    ws.topology = topology;
    ws.nodes = nodes;
    ws.roots = roots;
    ws.authoritative = authoritative;
    ws.state = if authoritative { 2 } else { 1 };
    Ok(())
}

#[cfg(test)]
mod tests;
