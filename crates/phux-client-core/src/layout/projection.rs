//! Legacy layout metadata keys and the default Group-1 scope.

use phux_protocol::GroupId;

pub use phux_protocol::wire::info::{projection_key_session, tui_layout_key as layout_key};

/// Bare legacy TUI layout key used by servers without per-session layout metadata.
pub const LAYOUT_KEY: &str = "phux.tui.layout/v1";

/// Group 1 is the v0.x layout metadata scope (`docs/organization.md`).
pub const LAYOUT_METADATA_GROUP: GroupId = GroupId::new(1);
