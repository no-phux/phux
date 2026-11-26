//! An attached replica seeded through real HELLO/attach/bootstrap frames.

use std::cell::Cell;
use std::sync::Arc;

use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_client_runtime::engine::ViewSource;
use phux_client_runtime::{Client, Runtime, ViewId};
use phux_protocol::caps::{ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{AuthorityId, ResourceId, Revision, SessionId, SessionKey, WindowId};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandValue, FrameKind, OrganizationPage, SessionRecord,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_wire_fixtures::{HelloOk, attached, bootstrap_frames, decode_one, output};

pub const FILL: &str = "terminal workload terminal workload terminal workload terminal workload ";

/// The organization authority the fixture server keeps across restarts:
/// session `n`'s durable key is `[n; 16]` in it.
pub const DURABLE_AUTHORITY: [u8; 16] = [0x0A; 16];

pub struct Replica {
    pub client: Arc<Client>,
    pub views: Vec<ViewId>,
    pub terminal: ResourceId,
    pub sources: Vec<ViewSource>,
    shape: Shape,
    /// The incarnation the last HELLO named, and how many times it attached.
    server: Cell<(u8, u64)>,
    seq: u64,
}

/// What every connection to the fixture server serves.
#[derive(Clone, Copy)]
struct Shape {
    size: (u16, u16),
    terminals: u32,
    sessions: u32,
    streamed: u32,
    capabilities: ServerCapabilities,
}

impl Replica {
    pub fn new(cols: u16, rows: u16, terminals: u32) -> Self {
        Self::with_capabilities(cols, rows, terminals, ServerCapabilities::new())
    }

    pub fn with_capabilities(
        cols: u16,
        rows: u16,
        terminals: u32,
        capabilities: ServerCapabilities,
    ) -> Self {
        Self::spread(cols, rows, terminals, 1, capabilities)
    }

    /// [`Self::new`] on a server that serves the organization, so each
    /// session has a durable authority and key: a new incarnation
    /// ([`Self::replace_server`]) recovers the session only through those.
    pub fn durable(cols: u16, rows: u16, terminals: u32) -> Self {
        Self::with_capabilities(cols, rows, terminals, organized(ServerCapabilities::new()))
    }

    /// `terminals` dealt round `sessions` sessions, one window each; the
    /// first terminal, in the first session, is attached and fed.
    pub fn spread(
        cols: u16,
        rows: u16,
        terminals: u32,
        sessions: u32,
        capabilities: ServerCapabilities,
    ) -> Self {
        Self::with_shape(Shape {
            size: (cols, rows),
            terminals,
            sessions,
            streamed: terminals,
            capabilities,
        })
    }

    /// `terminals` in `budget`, the last with no first source yet: it opened
    /// after the attach (a server sends `ATTACH_READY` only once every
    /// attached terminal is ready), so only the inventory names it. Durable,
    /// like [`Self::durable`].
    pub fn pending(cols: u16, rows: u16, terminals: u32) -> Self {
        Self::with_shape(Shape {
            size: (cols, rows),
            terminals,
            sessions: 1,
            streamed: terminals - 1,
            capabilities: organized(ServerCapabilities::new()),
        })
    }

    fn with_shape(shape: Shape) -> Self {
        let client = Runtime::embedded(ControlOptions {
            attach: Some(AttachTarget::ByName("budget".into())),
            viewport: shape.size,
            ..ControlOptions::default()
        });
        let ids = seed(&client, 1, 0, shape);
        let _ = client.take_outbound();
        let _ = client.take_events();
        let sources: Vec<_> = ids
            .iter()
            .take(shape.streamed as usize)
            .map(|id| {
                client
                    .view_source(id, Some(&[1; 16]), 1)
                    .expect("original source")
            })
            .collect();
        let views = sources
            .iter()
            .map(|source| client.create_view(source).expect("a view"))
            .collect();
        Self {
            client: Arc::new(client),
            views,
            terminal: ids[0].clone(),
            sources,
            shape,
            server: Cell::new((1, 0)),
            seq: 0,
        }
    }

    /// Reconnect to incarnation `server` and attach again, streaming every
    /// terminal. A new incarnation deliberately reuses resource, stream and
    /// bootstrap ids, and is re-attached only by the session's exact durable
    /// authority and key. The same incarnation keeps its replicas, so like a
    /// real server it streams each terminal on a new stream for the new
    /// attach.
    pub fn replace_server(&self, server: u8) {
        let (previous, attaches) = self.server.get();
        let attaches = if previous == server { attaches + 1 } else { 0 };
        self.server.set((server, attaches));
        let shape = Shape {
            streamed: self.shape.terminals,
            ..self.shape
        };
        seed(&self.client, server, attaches, shape);
    }

    pub fn window(&self) -> crate::desk::Desk {
        let mut desk = super::window();
        desk.connection.client = Some(self.client.clone());
        desk.connection.status = self.client.status();
        desk.connection.topology = self.client.topology();
        desk.connection
            .seed_view_sources(vec![1; 16], &self.sources);
        desk
    }

    pub fn feed(&mut self, bytes: String) {
        self.seq += 1;
        self.client
            .feed(output(&self.terminal, self.seq, bytes.as_bytes()))
            .expect("output");
    }
}

/// `capabilities` that also serve the organization replica.
pub fn organized(capabilities: ServerCapabilities) -> ServerCapabilities {
    let organization = ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]);
    let features_ext = capabilities.features_ext.as_wire() | organization.as_wire();
    capabilities.with_features_ext(ServerFeatureExtSet::from_wire(features_ext))
}

fn bootstrap(client: &Client, id: &ResourceId, ids: [u64; 2], size: (u16, u16), payload: &str) {
    for frame in bootstrap_frames(id, ids, [size.0, size.1], payload.as_bytes()) {
        client.feed(frame).expect("bootstrap");
    }
}

fn session_name(n: u32) -> String {
    if n == 1 {
        "budget".to_owned()
    } else {
        format!("s{n}")
    }
}

fn decoded(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| decode_one(bytes))
        .collect()
}

fn attach_id(frames: &[FrameKind]) -> Option<u32> {
    frames.iter().find_map(|frame| match frame {
        FrameKind::Attach { attach_id, .. } => Some(*attach_id),
        _ => None,
    })
}

/// Answer the connection's organization seed, if it asked: every session
/// `n` live under its durable key `[n; 16]` in [`DURABLE_AUTHORITY`].
fn seed_organization(client: &Client, queued: &[FrameKind], sessions: u32) {
    let Some(request_id) = queued.iter().find_map(|frame| match frame {
        FrameKind::Command {
            request_id,
            command: Command::GetOrganization { .. },
        } => Some(*request_id),
        _ => None,
    }) else {
        return;
    };
    let records = (1..=sessions)
        .map(|n| {
            let byte = u8::try_from(n).expect("a few sessions");
            let key = SessionKey::new([byte; 16]).expect("nonzero key");
            SessionRecord::new(key, session_name(n), Revision::new(1))
                .with_live(Some(SessionId::new(n)))
        })
        .collect();
    let authority = AuthorityId::new(DURABLE_AUTHORITY).expect("nonzero authority");
    let page = OrganizationPage::new(authority, Revision::new(1), 0).with_sessions(records);
    client
        .feed(phux_wire_fixtures::command_value(
            request_id,
            CommandValue::OrganizationPage(Box::new(page)),
        ))
        .expect("organization page");
}

/// HELLO, attach and bootstrap: `terminals` dealt round `sessions`
/// sessions, one window each, the first `streamed` of them fed. An organized
/// server gives each session a durable identity. Its
/// `attaches`th attach on one incarnation streams on fresh stream ids.
fn seed(client: &Client, server: u8, attaches: u64, shape: Shape) -> Vec<ResourceId> {
    let Shape {
        size: (cols, rows),
        terminals,
        sessions,
        streamed,
        capabilities,
    } = shape;
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(HelloOk::new(vec![server; 16], capabilities).build())
        .expect("hello");
    let queued = decoded(client);
    seed_organization(client, &queued, sessions);
    // After a restart the exact-key attach waits for the organization seed.
    let attach_id = attach_id(&queued)
        .or_else(|| attach_id(&decoded(client)))
        .expect("attach request");
    let ids: Vec<_> = (1..=terminals).map(ResourceId::local).collect();
    let session_of = |index: usize| SessionId::new(index as u32 % sessions + 1);
    let resources = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let session = session_of(index);
            ResourceInfo::new(id.clone(), WindowId::new(session.get()), cols, rows)
                .with_session(Some(session))
        })
        .collect();
    let (session, window) = (SessionId::new(1), WindowId::new(1));
    let names = (1..=sessions).map(|n| SessionInfo::new(SessionId::new(n), session_name(n)));
    let windows = (1..=sessions).map(|n| {
        WindowInfo::new(WindowId::new(n), SessionId::new(n), "shell")
            .with_active_resource(Some(ids[n as usize - 1].clone()))
    });
    let inventory = SessionSnapshot::new(session, window, ids[0].clone())
        .with_sessions(names.collect())
        .with_windows(windows.collect())
        .with_resources(resources);
    // The attach bootstraps every terminal it names: one opened since, with
    // no first source yet, is only in the inventory that follows it.
    let mut snapshot = inventory.clone();
    let opened_since = |index: usize| index >= streamed as usize && session_of(index) == session;
    let mut index = 0;
    snapshot.resources.retain(|_| {
        index += 1;
        !opened_since(index - 1)
    });
    let pending = snapshot.resources.len() != inventory.resources.len();
    client
        .feed(attached(attach_id, snapshot, 1))
        .expect("attached");
    let payload = (0..rows)
        .map(|row| format!("{row:02} {FILL}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    for (index, id) in ids.iter().take(streamed as usize).enumerate() {
        let number = index as u64 + 1;
        let stream = number + attaches * u64::from(terminals);
        bootstrap(client, id, [stream, number], (cols, rows), &payload);
    }
    client
        .feed(FrameKind::AttachReady { attach_id })
        .expect("attach ready");
    if pending {
        let request_id = client
            .with_control(ControlPlane::refresh_topology)
            .expect("negotiated");
        client
            .feed(phux_wire_fixtures::command_value(
                request_id,
                CommandValue::State(inventory),
            ))
            .expect("inventory");
    }
    ids
}
