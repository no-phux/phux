//! The stable words a binding names session-graph values with.

use phux_client_runtime::activation::{core_exec_state, core_lifetime};
use phux_protocol::ResourceKind;
use phux_protocol::wire::frame::{ExecState, ExitPolicy, SessionLifetime};

/// The stable word for a session lifetime.
#[must_use]
pub const fn lifetime_word(lifetime: SessionLifetime) -> &'static str {
    match core_lifetime(lifetime) {
        Some(lifetime) => lifetime.as_str(),
        None => "unknown",
    }
}

/// The stable word for an activation's execution state.
#[must_use]
pub const fn exec_word(state: ExecState) -> &'static str {
    match core_exec_state(state) {
        Some(state) => state.as_str(),
        None => "unknown",
    }
}

/// The stable word for a resource kind (`terminal`, `agent_session`,
/// `accessibility_tree`, `pixel_surface`); an unknown wire kind reads
/// `kind-<tag>`.
#[must_use]
pub fn kind_word(kind: ResourceKind) -> String {
    match kind {
        ResourceKind::Terminal => "terminal".to_owned(),
        ResourceKind::AgentSession => "agent_session".to_owned(),
        ResourceKind::AccessibilityTree => "accessibility_tree".to_owned(),
        ResourceKind::PixelSurface => "pixel_surface".to_owned(),
        ResourceKind::Document => "document".to_owned(),
        other => format!("kind-{}", other.as_wire()),
    }
}

/// The stable word for an exit policy.
#[must_use]
pub const fn exit_policy_word(policy: ExitPolicy) -> &'static str {
    match policy {
        ExitPolicy::Remove => "remove",
        ExitPolicy::Retain => "retain",
        ExitPolicy::Unknown(_) => "unknown",
    }
}

/// Lowercase hex of a 16-byte arrangement group identity.
#[must_use]
pub fn group_id_text(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    id.iter().fold(String::with_capacity(32), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}
