//! Whether a Terminal owns a live `AgentSession` child (ADR-0103 §5), the
//! top rung above the screen detector. The detector cannot see bindings, so
//! the answer is injected: [`LiveSessionProbe`] for the pane,
//! [`server_has_live_session`] for command handlers. No probe means no
//! child (test and upgrade-rebuilt actors).

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling runtime / state modules"
)]

use std::rc::Rc;

use phux_protocol::ids::ResourceId as WireResourceId;

use crate::state::ServerState;

/// "Does this pane own a live `AgentSession` child now?", bound per pane.
pub(crate) type LiveSessionProbe = Rc<dyn Fn() -> bool>;

/// The same question for command handlers holding `ServerState`
/// (`REPORT_AGENT_STATE`, ADR-0103 §6). `false` for unknown or satellite
/// ids.
pub(crate) fn server_has_live_session(state: &ServerState, terminal: &WireResourceId) -> bool {
    state
        .terminal_from_wire(terminal)
        .is_some_and(|core| state.has_live_agent_session_child(core))
}
