//! `UniFFI` projection shim over `phux-client-runtime`.
//!
//! The runtime owns dialing, reconnect, frame handling, the session kernel,
//! engine thread, durable operations, and grid publication. This module only
//! translates runtime-owned values into the stable Swift-facing vocabulary.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use phux_client_runtime::control::{DeliveryOutcome, Event, Status, Topology};
use phux_client_runtime::{
    Client, ClientOptions, ConnectOptions, Listener, Runtime, Target, Transport,
};
use phux_protocol::ResourceId;
use phux_protocol::caps::{Layer, ServerFeature};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::mouse::{
    MouseAction as WireMouseAction, MouseButton as WireMouseButton, MouseEvent,
};
use phux_protocol::input::paste::PasteTrust;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, RESOURCE_AGENT_KEY, Scope};

use crate::keymap::{self, KeyMods, KeyPress};

mod client;
mod types;

#[allow(unused_imports)]
pub use client::*;
pub use types::*;

fn terminal_id_string(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("local:{id}"),
        ResourceId::Satellite { host, id } => format!("satellite:{}:{id}", host.as_str()),
    }
}

fn parse_terminal_id(value: &str) -> Option<ResourceId> {
    if let Some(raw) = value.strip_prefix("local:") {
        return raw.parse::<u32>().ok().map(ResourceId::local);
    }
    let rest = value.strip_prefix("satellite:")?;
    let (host, raw) = rest.rsplit_once(':')?;
    (!host.is_empty())
        .then(|| {
            raw.parse::<u32>()
                .ok()
                .map(|id| ResourceId::satellite(host, id))
        })
        .flatten()
}

fn project_topology(topology: Topology) -> SessionTopology {
    SessionTopology {
        sessions: topology
            .sessions
            .into_iter()
            .map(|session| SessionDescriptor {
                id: session.id,
                name: session.name,
                window_count: session.window_count,
                attached_client_count: session.attached_client_count,
            })
            .collect(),
        panes: topology
            .panes
            .into_iter()
            .map(|pane| PaneDescriptor {
                terminal_id: terminal_id_string(&pane.terminal_id),
                session_id: pane.session_id,
                session_name: pane.session_name,
                window_id: pane.window_id,
                window_index: pane.window_index,
                window_name: pane.window_name,
                title: pane.title,
                cwd: pane.cwd,
                is_focused: pane.is_focused,
            })
            .collect(),
        focused_pane: terminal_id_string(&topology.focused_pane),
    }
}
