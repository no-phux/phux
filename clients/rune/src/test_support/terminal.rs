//! An attached replica seeded through real HELLO/attach/bootstrap frames.

use std::sync::Arc;

use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_client_runtime::{Client, Runtime, ViewId};
use phux_protocol::caps::ServerCapabilities;
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{AttachTarget, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_wire_fixtures::{HelloOk, attached, bootstrap_frames, decode_one, output};

pub const FILL: &str = "terminal workload terminal workload terminal workload terminal workload ";

pub struct Replica {
    pub client: Arc<Client>,
    pub views: Vec<ViewId>,
    pub terminal: ResourceId,
    seq: u64,
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

    /// `terminals` dealt round `sessions` sessions, one window each; the
    /// first terminal, in the first session, is attached and fed.
    pub fn spread(
        cols: u16,
        rows: u16,
        terminals: u32,
        sessions: u32,
        capabilities: ServerCapabilities,
    ) -> Self {
        let client = Runtime::embedded(ControlOptions {
            attach: Some(AttachTarget::ByName("budget".into())),
            viewport: (cols, rows),
            ..ControlOptions::default()
        });
        client.with_control(ControlPlane::connection_opened);
        let _ = client.take_outbound();
        client
            .feed(HelloOk::new(vec![1; 16], capabilities).build())
            .expect("hello");
        let attach_id = client
            .take_outbound()
            .iter()
            .find_map(|bytes| match decode_one(bytes) {
                FrameKind::Attach { attach_id, .. } => Some(attach_id),
                _ => None,
            })
            .expect("attach request");
        let ids: Vec<ResourceId> = (1..=terminals).map(ResourceId::local).collect();
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
        let names = (1..=sessions).map(|n| {
            let name = if n == 1 {
                "budget".to_owned()
            } else {
                format!("s{n}")
            };
            SessionInfo::new(SessionId::new(n), name)
        });
        let windows = (1..=sessions).map(|n| {
            WindowInfo::new(WindowId::new(n), SessionId::new(n), "shell")
                .with_active_resource(Some(ids[n as usize - 1].clone()))
        });
        let snapshot = SessionSnapshot::new(session, window, ids[0].clone())
            .with_sessions(names.collect())
            .with_windows(windows.collect())
            .with_resources(resources);
        client
            .feed(attached(attach_id, snapshot, 1))
            .expect("attached");
        let payload = (0..rows)
            .map(|row| format!("{row:02} {FILL}"))
            .collect::<Vec<_>>()
            .join("\r\n");
        for (index, id) in ids.iter().enumerate() {
            bootstrap(&client, id, index as u64 + 1, cols, rows, &payload);
        }
        client
            .feed(FrameKind::AttachReady { attach_id })
            .expect("attach ready");
        let _ = client.take_outbound();
        let _ = client.take_events();
        let views = ids
            .iter()
            .map(|id| client.create_view(id).expect("a view"))
            .collect();
        Self {
            client: Arc::new(client),
            views,
            terminal: ids[0].clone(),
            seq: 0,
        }
    }

    pub fn feed(&mut self, bytes: String) {
        self.seq += 1;
        self.client
            .feed(output(&self.terminal, self.seq, bytes.as_bytes()))
            .expect("output");
    }
}

fn bootstrap(client: &Client, id: &ResourceId, number: u64, cols: u16, rows: u16, payload: &str) {
    let ids = [number, number];
    for frame in bootstrap_frames(id, ids, [cols, rows], payload.as_bytes()) {
        client.feed(frame).expect("bootstrap");
    }
}
