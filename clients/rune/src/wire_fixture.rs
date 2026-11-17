//! Frames the tests feed an embedded client, as a server would send them.

use phux_client_runtime::Client;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
    ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, StreamId};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use phux_protocol::wire::info::SessionSnapshot;

/// What the client has sent since last asked.
pub fn outbound(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

/// Server `server`'s hello, with the organization features when `organized`.
pub fn hello(server: u8, organized: bool) -> FrameKind {
    let caps = ServerCapabilities::new();
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: if organized {
            caps.with_features_ext(ServerFeatureExtSet::with(&[
                ServerFeatureExt::Organization,
                ServerFeatureExt::SlotTabs,
            ]))
        } else {
            caps
        },
        server_id: vec![server; 16],
        selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
        bootstrap_limits: BootstrapLimits::default(),
    }
}

/// The attach in `sent`.
pub fn attach_id(sent: &[FrameKind]) -> Option<u32> {
    sent.iter().find_map(|frame| match frame {
        FrameKind::Attach { attach_id, .. } => Some(*attach_id),
        _ => None,
    })
}

/// The request id of the first command in `sent` that `is` picks.
pub fn request(sent: &[FrameKind], is: impl Fn(&Command) -> bool) -> Option<u32> {
    sent.iter().find_map(|frame| match frame {
        FrameKind::Command {
            request_id,
            command,
        } if is(command) => Some(*request_id),
        _ => None,
    })
}

/// The organization seed request in `sent`.
pub fn organization_request(sent: &[FrameKind]) -> Option<u32> {
    request(sent, |c| matches!(c, Command::GetOrganization { .. }))
}

/// Answer `request_id` with `value`.
pub fn answer(client: &Client, request_id: u32, value: CommandValue) {
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(value),
        })
        .unwrap();
}

/// A whole bootstrap of `terminal`: begin, one chunk of `payload`, ready.
pub fn bootstrap_frames(
    terminal: &ResourceId,
    [stream, bootstrap]: [u64; 2],
    profile: BootstrapStreamProfile,
    [cols, rows]: [u16; 2],
    payload: &[u8],
) -> [FrameKind; 3] {
    let stream_id = StreamId::new(stream).unwrap();
    let bootstrap_id = BootstrapId::new(bootstrap).unwrap();
    [
        FrameKind::BootstrapBegin {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: payload.to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}

/// [`bootstrap_frames`] of a terminal's `payload`, fed.
pub fn bootstrap(
    client: &Client,
    terminal: &ResourceId,
    ids: [u64; 2],
    size: [u16; 2],
    payload: &[u8],
) {
    let profile = BootstrapStreamProfile::SynthesizedVtRaw;
    for frame in bootstrap_frames(terminal, ids, profile, size, payload) {
        client.feed(frame).unwrap();
    }
}

/// The server's answer to attach `attach_id`: `snapshot`, as client 1.
pub fn attached(client: &Client, attach_id: u32, snapshot: SessionSnapshot) {
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
}

/// Refuse `request_id` with `code` and `message`.
pub fn refuse(client: &Client, request_id: u32, code: ErrorCode, message: &str) {
    let message = message.to_owned();
    let result = CommandResult::Error { code, message };
    client
        .feed(FrameKind::CommandResult { request_id, result })
        .unwrap();
}
