//! Fixtures shared by the watch behavior tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

mod fleet;
mod resume;
mod scoped;

use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{FrameKind, Scope};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use crate::agent_meta::RESOURCE_AGENT_KEY;

fn agent_record(scope_terminal: &ResourceId, json: &str) -> FrameKind {
    FrameKind::MetadataChanged {
        scope: Scope::Resource(scope_terminal.clone()),
        key: RESOURCE_AGENT_KEY.to_owned(),
        value: Some(json.as_bytes().to_vec()),
        actor: None,
    }
}

fn snapshot(resources: Vec<ResourceInfo>) -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_resources(resources)
}
