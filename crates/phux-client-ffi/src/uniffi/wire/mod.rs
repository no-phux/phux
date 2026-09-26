//! The `UniFFI` remote bridge over one `phux-client-runtime` session.
//!
//! The runtime owns dialing, reconnect, frame handling, the session kernel,
//! the engine thread, durable operations and grid publication.
//! [`crate::projection`] owns what those values mean. This module lowers the
//! projected values into the stable Swift-facing vocabulary and nothing else.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use phux_client_runtime::control::Event;
use phux_client_runtime::{
    Client, ClientOptions, ConnectOptions, Listener, Runtime, Target, Transport,
};
use phux_protocol::ResourceId;
use phux_protocol::caps::Layer;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::mouse::{
    MouseAction as WireMouseAction, MouseButton as WireMouseButton, MouseEvent,
};
use phux_protocol::input::paste::PasteTrust;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, RESOURCE_AGENT_KEY, Scope};

use crate::projection::{agent, event, grid, id, outcome, status, topology};

use super::keymap::{self, KeyMods, KeyPress};

mod client;
mod types;

#[allow(unused_imports)]
pub use client::*;
pub use types::*;
