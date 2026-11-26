//! Metadata subscription sweeps: the per-pane `phux.agent/v1` watches, and
//! [`PeerWatch`], the peer-session caches behind the picker and the fleet
//! together with the sweeps that keep them watched.

use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::ArrangementDoc;
use phux_client::agent_meta::{
    AgentRecord, RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY, RESOURCE_PANE_OCCUPANT_KEY,
    parse_agent_record,
};
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

mod peer_reads;

use peer_reads::InventoryRead;
pub(super) use peer_reads::{InventoryReply, PeerRead, PeerReply, SidebarFact};

/// Client presentation choices are not subscription lifetime or reply state.
#[derive(Default)]
pub(super) struct PeerPresentation {
    pub(super) chrome_dirty: bool,
    pub(super) session_order: Vec<phux_protocol::ids::SessionKey>,
    pub(super) collapsed_projects: std::collections::BTreeSet<String>,
}

/// The peer-session caches the roster, window picker, and fleet dashboard
/// project from. Written by the same peer sweep, reset together.
#[derive(Default)]
pub(super) struct PeerWatch {
    /// Identity of the serving machine, read once through the whoami key.
    pub(super) serving_host: Option<String>,
    serving_host_attempted: bool,
    /// ADR-0140: which machine this process attached to, when the CLI
    /// recorded one. `None` runs no hosts provider.
    pub(super) origin: Option<crate::attach::hosts::AttachOrigin>,
    /// ADR-0140: the hosts provider's latest `phux.hosts/v1` rows. The
    /// attached machine's row is filtered out at projection time.
    pub(super) remote_hosts: Vec<phux_core::host_list::HostJson>,
    /// The server's session graph, refreshed from every ATTACHED snapshot,
    /// for the session picker.
    pub(super) sessions: Vec<phux_protocol::wire::info::SessionInfo>,
    /// Windows from the same graph: an older server's tab grouping, and the
    /// session join for a resource that does not name its session. Never
    /// read for a resource that does (an `ORGANIZATION` server).
    pub(super) windows: Vec<phux_protocol::wire::info::WindowInfo>,
    /// Resources from the same graph, joined to their session by their
    /// `session` facet, else through `ResourceInfo::window_id`.
    pub(super) resources: Vec<phux_protocol::wire::info::ResourceInfo>,
    /// The session this client is attached to, once ATTACHED has named it.
    pub(super) focused_session: Option<SessionId>,
    /// Peer sessions' arrangements for the picker's one-step window rows.
    pub(super) foreign_layouts: HashMap<SessionId, ArrangementDoc>,
    /// Peer arrangement records to read over the arrangement link once this
    /// burst settles (the `ORGANIZATION` form of the layout sweep).
    pub(super) arrangement_reads_owed: crate::attach::peer_arrangements::PeerReads,
    /// One typed correlation table owns every metadata GET in this entry.
    pending: HashMap<u32, PeerRead>,
    /// `phux.agent/v1` records of foreign panes, so the fleet dashboard and
    /// Agents list show peer agents without attaching. Pruned to the live
    /// foreign terminal set on each fold.
    pub(super) foreign_agents: HashMap<ResourceId, AgentRecord>,
    /// Peer layout keys already subscribed. L3 has no unsubscribe, so a
    /// subscription lives as long as the connection.
    pub(super) foreign_layout_subscribed: HashSet<SessionId>,
    /// The per-pane half of the same send-once bookkeeping.
    pub(super) foreign_agent_subscribed: HashSet<ResourceId>,
    /// The federation host inventory from the latest `GET_STATE`: one row per
    /// satellite, with its sessions or why it could not be listed. Empty
    /// without `ServerFeature::HostSessions`.
    pub(super) hosts: Vec<phux_protocol::wire::info::HostInventory>,
    /// The correlated inventory read and notices waiting for its explanation.
    inventory: Option<InventoryRead>,
    global_subscribed: bool,
    /// Peer panes whose agent asked for a human; a foreign pane has no
    /// `PaneSlot::attention` to carry the flag.
    pub(super) foreign_attention: HashSet<ResourceId>,
    /// Project tags from `phux.session.project/v1`, keyed by session id.
    pub(super) project_tags: HashMap<SessionId, String>,
    /// The one stored tag (`name`, `project`) before it is joined to a session id.
    pub(super) stored_project_tag: Option<(String, String)>,
    /// The peer sweep waits for the first paint, so a bootstrap does not
    /// queue peer traffic ahead of the burst that paints.
    pub(super) sweep_pending: bool,
}

impl PeerWatch {
    pub(super) fn new(origin: Option<crate::attach::hosts::AttachOrigin>) -> Self {
        Self {
            sweep_pending: true,
            remote_hosts: if origin.is_some() {
                crate::attach::hosts::last_known()
            } else {
                Vec::new()
            },
            origin,
            ..Self::default()
        }
    }

    /// The peer-wide projection the sidebar strip renders from.
    pub(super) fn inputs<'a>(
        &'a self,
        review: &'a crate::attach::review::ReviewIndex,
        presentation: &'a PeerPresentation,
    ) -> crate::attach::sidebar_zones::PeerInputs<'a> {
        crate::attach::sidebar_zones::PeerInputs {
            serving_host: self.serving_host.as_deref(),
            origin: self.origin.as_ref(),
            remote_hosts: &self.remote_hosts,
            hosts: &self.hosts,
            sessions: &self.sessions,
            focused_session: self.focused_session,
            windows: &self.windows,
            resources: &self.resources,
            foreign_layouts: &self.foreign_layouts,
            foreign_agents: &self.foreign_agents,
            foreign_attention: &self.foreign_attention,
            project_tags: &self.project_tags,
            session_order: &presentation.session_order,
            collapsed_projects: &presentation.collapsed_projects,
            review,
        }
    }

    /// GET and SUBSCRIBE each peer session's legacy layout key for the
    /// picker's one-step rows (without `ORGANIZATION` only).
    pub(super) async fn sweep_layouts(
        &mut self,
        conn: &mut Connection,
        next_request_id: &mut u32,
    ) -> Result<(), AttachError> {
        let focused = self.focused_session;
        for s in self.sessions.iter().filter(|s| Some(s.id) != focused) {
            let request_id = *next_request_id;
            *next_request_id = next_request_id.wrapping_add(1);
            self.pending.insert(request_id, PeerRead::Layout(s.id));
            let key = layout_key(s.id);
            conn.send(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: key.clone(),
            })
            .await?;
            // Subscribe even if the GET answers `None`: a peer's first write is
            // the one that matters. Send-once, since L3 has no unsubscribe.
            if self.foreign_layout_subscribed.insert(s.id) {
                conn.send(&FrameKind::SubscribeMetadata {
                    scope: Scope::Group(DEFAULT_GROUP_ID),
                    key,
                })
                .await?;
            }
        }
        Ok(())
    }

    /// Prune and re-sync the foreign agent watches against the live foreign
    /// terminal set (from persisted layouts, or the server graph).
    pub(super) async fn sweep_agents(
        &mut self,
        conn: &mut Connection,
        next_request_id: &mut u32,
        review: &crate::attach::review::ReviewIndex,
        presentation: &PeerPresentation,
    ) -> Result<(), AttachError> {
        let live =
            crate::attach::sidebar_zones::foreign_terminal_ids(&self.inputs(review, presentation));
        self.prune_agents(&live);
        self.watch_agents(conn, live.into_iter().collect(), next_request_id)
            .await
    }

    /// GET/SUBSCRIBE `phux.agent/v1` for each terminal in `targets`. Satellite
    /// terminals also GET the asked flag (ADR-0136), correlated by its own
    /// read kind since its value is not an agent record.
    pub(super) async fn watch_agents(
        &mut self,
        conn: &mut Connection,
        targets: Vec<ResourceId>,
        next_request_id: &mut u32,
    ) -> Result<(), AttachError> {
        let in_flight: HashSet<&ResourceId> = self
            .pending
            .values()
            .filter_map(|read| {
                if let PeerRead::Agent(id) = read {
                    Some(id)
                } else {
                    None
                }
            })
            .collect();
        // Dedup while preserving order; skip in-flight GETs.
        let mut seen = HashSet::new();
        let targets: Vec<ResourceId> = targets
            .into_iter()
            .filter(|id| !in_flight.contains(id))
            .filter(|id| seen.insert(id.clone()))
            .collect();
        for id in targets {
            let request_id = *next_request_id;
            *next_request_id = next_request_id.wrapping_add(1);
            self.pending.insert(request_id, PeerRead::Agent(id.clone()));
            conn.send(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Resource(id.clone()),
                key: RESOURCE_AGENT_KEY.to_owned(),
            })
            .await?;
            if self.foreign_agent_subscribed.insert(id.clone()) {
                conn.send(&FrameKind::SubscribeMetadata {
                    scope: Scope::Resource(id.clone()),
                    key: RESOURCE_AGENT_KEY.to_owned(),
                })
                .await?;
                if !id.is_local() {
                    let asked_id = *next_request_id;
                    *next_request_id = next_request_id.wrapping_add(1);
                    self.pending.insert(asked_id, PeerRead::Asked(id.clone()));
                    conn.send(&FrameKind::GetMetadata {
                        request_id: asked_id,
                        scope: Scope::Resource(id.clone()),
                        key: RESOURCE_ASKED_KEY.to_owned(),
                    })
                    .await?;
                    conn.send(&FrameKind::SubscribeMetadata {
                        scope: Scope::Resource(id),
                        key: RESOURCE_ASKED_KEY.to_owned(),
                    })
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Fold a sweep of peer arrangement records: a full sweep replaces the
    /// cache, a targeted read replaces only the sessions it read. A session
    /// with no arrangement clears its entry.
    pub(super) fn apply_arrangement_reads(
        &mut self,
        reads: HashMap<SessionId, Option<ArrangementDoc>>,
        full: bool,
    ) {
        if full {
            self.foreign_layouts
                .retain(|session, _| reads.contains_key(session));
        }
        for (session, document) in reads {
            match document {
                Some(document) => {
                    self.foreign_layouts.insert(session, document);
                }
                None => {
                    self.foreign_layouts.remove(&session);
                }
            }
        }
    }

    /// Fold a peer layout GET reply; `None` or garbage clears the entry.
    pub(super) fn apply_layout_reply(&mut self, session: SessionId, value: Option<&[u8]>) {
        match value {
            Some(bytes) => match ArrangementDoc::decode_cbor(bytes) {
                Ok(ws) => {
                    self.foreign_layouts.insert(session, ws);
                }
                Err(err) => {
                    tracing::debug!(
                        session = session.get(),
                        error = %err,
                        "foreign layout decode failed; window picker keeps the fallback row",
                    );
                    self.foreign_layouts.remove(&session);
                }
            },
            None => {
                self.foreign_layouts.remove(&session);
            }
        }
    }

    /// Fold a peer agent-record GET reply (`None` or garbage clears); returns
    /// whether the cache moved.
    pub(super) fn apply_agent_reply(&mut self, id: ResourceId, value: Option<&[u8]>) -> bool {
        match value.and_then(parse_agent_record) {
            Some(record) => self.foreign_agents.insert(id, record.clone()) != Some(record),
            None => self.foreign_agents.remove(&id).is_some(),
        }
    }

    /// Drop peer agent records (and their send-once subscription markers, so a
    /// returning id re-subscribes) for panes no longer in `live`.
    pub(super) fn prune_agents(&mut self, live: &HashSet<ResourceId>) {
        self.foreign_agents.retain(|id, _| live.contains(id));
        self.foreign_agent_subscribed.retain(|id| live.contains(id));
        self.pending.retain(|_, read| match read {
            PeerRead::Agent(id) | PeerRead::Asked(id) => live.contains(id),
            _ => true,
        });
        self.foreign_attention.retain(|id| live.contains(id));
    }
}

/// ADR-0040: GET + SUBSCRIBE `phux.agent/v1` for every pane not yet
/// watched, and prune closed panes from the side tables. Idempotent.
pub(super) async fn sync_agent_meta_subscriptions(
    conn: &mut Connection,
    // Owned ids: a `PaneSlot` reference across sends would make this `!Send`.
    pane_ids: Vec<ResourceId>,
    agent_meta: &mut AgentMetaIndex,
    next_request_id: &mut u32,
) -> Result<(), AttachError> {
    agent_meta.subscribed.retain(|id| pane_ids.contains(id));
    agent_meta.records.retain(|id, _| pane_ids.contains(id));
    agent_meta.pending.retain(|_, id| pane_ids.contains(id));
    agent_meta
        .asked_pending
        .retain(|_, id| pane_ids.contains(id));
    agent_meta
        .occupant_pending
        .retain(|_, id| pane_ids.contains(id));
    // Same hygiene for the attention ladder's clock: a closed pane must not
    // leave a timestamp behind for a recycled ResourceId to inherit.
    agent_meta.change_at.retain(|id, _| pane_ids.contains(id));
    for id in &pane_ids {
        if agent_meta.subscribed.contains(id) {
            continue;
        }
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        agent_meta.pending.insert(request_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Resource(id.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(id.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
        // ADR-0136: a satellite pane's asked flag is metadata this client is
        // not guaranteed to see; the GET must not share `pending`.
        let (key, pending) = if id.is_local() {
            (RESOURCE_PANE_OCCUPANT_KEY, &mut agent_meta.occupant_pending)
        } else {
            (RESOURCE_ASKED_KEY, &mut agent_meta.asked_pending)
        };
        let side_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        pending.insert(side_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id: side_id,
            scope: Scope::Resource(id.clone()),
            key: key.to_owned(),
        })
        .await?;
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(id.clone()),
            key: key.to_owned(),
        })
        .await?;
        // Notes are edges: only a change after this is news, so no read.
        if id.is_local() {
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Resource(id.clone()),
                key: phux_protocol::wire::frame::RESOURCE_NOTE_KEY.to_owned(),
            })
            .await?;
        }
        agent_meta.subscribed.insert(id.clone());
    }
    Ok(())
}
