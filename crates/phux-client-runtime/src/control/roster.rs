//! Level-state recovery after lossy event delivery. No new wire contract:
//! subscribe before reading, and fence unversioned snapshots by live delivery.
//! Journal stamps do not version `GET_STATE` or `GET_METADATA` replies. Explicit
//! gaps trigger recovery; a numeric jump in a filtered event stream does not.
//! Current asked-state recovery needs a separate public retraction contract:
//! `AgentAsked` announces a question but cannot represent an absent asked record.

use phux_protocol::caps::Layer;
use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};

use super::{
    AgentEvent, CommandResult, CommandValue, ControlError, ControlPlane, ErrorCode, Event,
    FrameKind, HashMap, HashSet, ResourceId,
};

#[derive(Debug, Default)]
pub(super) struct RosterRecovery {
    pub topology: Option<(u32, bool)>,
    /// Terminals whose subscription and level read this connection has
    /// already issued. An inventory refresh reads only terminals new to it.
    subscribed: HashSet<ResourceId>,
    /// Re-read every inventoried declaration at the next inventory.
    resync: bool,
    /// Terminals the grant inventories but may not observe: their reads are
    /// refused with `PERMISSION_DENIED`, a stable answer for the connection.
    denied: HashSet<ResourceId>,
    reads: HashMap<u32, MetadataRead>,
}

#[derive(Debug)]
struct MetadataRead {
    terminal_id: ResourceId,
    superseded: bool,
    repeat: bool,
}

impl ControlPlane {
    /// One read in flight, plus one dirty bit, regardless of gap burst size.
    pub(super) fn recover_roster(&mut self) {
        if !self.options.automatic_lifecycle {
            return;
        }
        self.fence_topology_read();
        for read in self.roster.reads.values_mut() {
            read.superseded = true;
            // Wait for the recovered inventory before reading its declarations.
            read.repeat = false;
        }
        self.roster.resync = true;
        self.queue_refresh_topology();
    }

    /// The server ended this attachment, and with it every metadata
    /// subscription (L3.md §1.2). The next inventory subscribes afresh.
    pub(super) fn forget_roster_subscriptions(&mut self) {
        self.roster.subscribed.clear();
    }

    pub(super) fn observe_roster_event(&mut self, event: &AgentEvent) {
        if !self.options.automatic_lifecycle {
            return;
        }
        match event {
            AgentEvent::JournalGap { .. } | AgentEvent::SourceGap { .. } => self.recover_roster(),
            AgentEvent::ResourceSpawned { .. } => {
                self.fence_topology_read();
                self.queue_refresh_topology();
            }
            AgentEvent::ResourceClosed { .. }
            | AgentEvent::TitleChanged { .. }
            | AgentEvent::CwdChanged { .. } => self.fence_topology_read(),
            _ => {}
        }
    }

    pub(super) const fn fence_topology_read(&mut self) {
        if let Some((_, stale)) = &mut self.roster.topology {
            *stale = true;
        }
    }

    pub(super) fn resolve_topology_read(
        &mut self,
        result: CommandResult,
    ) -> Result<(), ControlError> {
        let stale = self.roster.topology.take().is_some_and(|(_, stale)| stale);
        if stale {
            self.queue_refresh_topology();
        } else if let CommandResult::OkWith(CommandValue::State(snapshot)) = result {
            self.apply_topology_refresh(&snapshot)?;
        }
        Ok(())
    }

    pub(super) fn sync_agent_metadata(&mut self) {
        if !self.options.automatic_lifecycle {
            return;
        }
        if !self
            .server
            .as_ref()
            .is_some_and(|server| server.layers.contains(Layer::L3))
        {
            return;
        }
        let terminals: Vec<_> = self
            .topology
            .iter()
            .flat_map(|topology| &topology.panes)
            .map(|pane| pane.terminal_id.clone())
            .collect();
        let resync = std::mem::take(&mut self.roster.resync);
        // There is no unsubscribe frame. The server tears these down on detach;
        // keep only live inventory locally and tolerate an idempotent resubscribe.
        self.roster.subscribed.retain(|id| terminals.contains(id));
        for terminal_id in terminals {
            if self.roster.denied.contains(&terminal_id) {
                continue;
            }
            // A live subscription already delivers every later change, so a
            // plain inventory refresh leaves a known terminal alone. Recovery
            // re-reads them all, since the loss may have hidden a change.
            if !self.roster.subscribed.insert(terminal_id.clone()) && !resync {
                continue;
            }
            self.queue_frame(&FrameKind::SubscribeMetadata {
                scope: Scope::Resource(terminal_id.clone()),
                key: RESOURCE_AGENT_KEY.to_owned(),
            });
            self.read_agent_metadata(terminal_id);
        }
    }

    fn read_agent_metadata(&mut self, terminal_id: ResourceId) {
        if let Some(read) = self
            .roster
            .reads
            .values_mut()
            .find(|read| read.terminal_id == terminal_id)
        {
            read.superseded = true;
            read.repeat = true;
            return;
        }
        let request_id = self.next_request_id();
        self.roster.reads.insert(
            request_id,
            MetadataRead {
                terminal_id: terminal_id.clone(),
                superseded: false,
                repeat: false,
            },
        );
        self.queue_frame(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Resource(terminal_id),
            key: RESOURCE_AGENT_KEY.to_owned(),
        });
    }

    /// Preserve extension frames exactly once. Only our own read correlations
    /// are consumed; live metadata remains available to existing raw consumers.
    pub(super) fn roster_metadata_frame(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.roster.reads.contains_key(&request_id) =>
            {
                self.resolve_agent_metadata(request_id, value);
                None
            }
            frame @ FrameKind::MetadataChanged { .. } => {
                self.observe_agent_metadata(&frame);
                Some(frame)
            }
            frame => Some(frame),
        }
    }

    fn observe_agent_metadata(&mut self, frame: &FrameKind) {
        let FrameKind::MetadataChanged {
            scope: Scope::Resource(terminal_id),
            key,
            value,
            ..
        } = frame
        else {
            return;
        };
        if key != RESOURCE_AGENT_KEY {
            return;
        }
        if !self.roster.subscribed.contains(terminal_id) {
            return;
        }
        for read in self
            .roster
            .reads
            .values_mut()
            .filter(|read| &read.terminal_id == terminal_id)
        {
            read.superseded = true;
            read.repeat = false;
        }
        self.push_event(Event::AgentMetadata {
            terminal_id: terminal_id.clone(),
            value: value.clone(),
        });
    }

    fn resolve_agent_metadata(&mut self, request_id: u32, value: Option<Vec<u8>>) {
        let Some(read) = self.roster.reads.remove(&request_id) else {
            return;
        };
        if !self.roster.subscribed.contains(&read.terminal_id) {
            return;
        }
        if read.repeat {
            self.read_agent_metadata(read.terminal_id);
        } else if !read.superseded {
            self.push_event(Event::AgentMetadata {
                terminal_id: read.terminal_id,
                value,
            });
        }
    }

    pub(super) fn forget_agent_metadata(&mut self, terminal_id: &ResourceId) {
        self.roster.subscribed.remove(terminal_id);
        self.roster.denied.remove(terminal_id);
        // Retain correlations until their replies so they cannot leak as raw
        // extension replies or be mistaken for a new resource's declaration.
        for read in self
            .roster
            .reads
            .values_mut()
            .filter(|read| &read.terminal_id == terminal_id)
        {
            read.superseded = true;
            read.repeat = false;
        }
    }

    /// Settle a refused roster read; `None` when `request_id` is not one.
    /// `Some(true)` when the refusal is the grant's stable answer, which is
    /// no consumer's error: the runtime issued the read on its own.
    pub(super) fn agent_metadata_error(
        &mut self,
        request_id: u32,
        code: ErrorCode,
    ) -> Option<bool> {
        let read = self.roster.reads.remove(&request_id)?;
        if code == ErrorCode::PermissionDenied {
            // A grant never widens on a live connection (workload-auth §7),
            // so neither the read nor its subscription is ever retried here.
            self.roster.subscribed.remove(&read.terminal_id);
            self.roster.denied.insert(read.terminal_id);
            return Some(true);
        }
        // An error is not a retraction. It still settles the old read and
        // must preserve a refresh requested while that read was in flight.
        if read.repeat && self.roster.subscribed.contains(&read.terminal_id) {
            self.read_agent_metadata(read.terminal_id);
        } else if !read.superseded {
            // The declaration stays unknown while its subscription stays
            // live; the next inventory reads it again.
            self.roster.resync = true;
        }
        Some(false)
    }
}
