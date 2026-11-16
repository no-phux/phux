//! Frames the tests feed an embedded client, as a server would send them.

use phux_client_runtime::Client;
use phux_protocol::caps::{
    BootstrapStreamProfile, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandValue, ErrorCode, FrameKind};
use phux_protocol::wire::info::SessionSnapshot;
use phux_wire_fixtures::{BootstrapOptions, HelloOk, bootstrap_frames_with, decode_one};

/// What the client has sent since last asked.
pub fn outbound(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| decode_one(bytes))
        .collect()
}

/// Server `server`'s hello, with the organization feature when `organized`.
pub fn hello(server: u8, organized: bool) -> FrameKind {
    let hello = HelloOk::new(vec![server; 16], ServerCapabilities::new());
    if !organized {
        return hello.build();
    }
    hello
        .features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .build()
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
        .feed(phux_wire_fixtures::command_value(request_id, value))
        .unwrap();
}

/// A whole bootstrap of `terminal`: begin, one chunk of `payload`, ready.
pub fn bootstrap_frames(
    terminal: &ResourceId,
    ids: [u64; 2],
    profile: BootstrapStreamProfile,
    size: [u16; 2],
    payload: &[u8],
) -> [FrameKind; 3] {
    let options = BootstrapOptions {
        profile,
        ..BootstrapOptions::default()
    };
    bootstrap_frames_with(terminal, ids, size, payload, options)
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
        .feed(phux_wire_fixtures::attached(attach_id, snapshot, 1))
        .unwrap();
}

/// Refuse `request_id` with `code` and `message`.
pub fn refuse(client: &Client, request_id: u32, code: ErrorCode, message: &str) {
    client
        .feed(phux_wire_fixtures::command_err(request_id, code, message))
        .unwrap();
}
