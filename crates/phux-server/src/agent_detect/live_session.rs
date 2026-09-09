//! The seam the screen detector consults before it publishes a derived state
//! (ADR-0103 decision 5).
//!
//! The evidence ladder for a Terminal's agent record is `Stream` > `Hook` >
//! `Process` > `Screen` ([`crate::agent_state::EvidenceSource`]). The top
//! rank is fed by an `AgentSession` child resource: while one is alive, the
//! agent itself is describing its own lifecycle through records it appends,
//! and the screen — which infers the same thing from pixels — is the weakest
//! source in the room and must not publish over it.
//!
//! The detector cannot answer "is there a live child" itself: bindings live
//! on `ServerState`, and the detector runs inside a pane engine that has no
//! view of it. So the answer is *injected*, and this module is the whole of
//! the injection. Both halves default to "no live child", which is exactly
//! the world before the `AgentSession` engine exists, so every shipped path
//! keeps its current behaviour until the producer lands.
//!
//! # For the `AgentSession` engine lane
//!
//! Two edits, both one line:
//!
//! 1. [`server_has_live_session`] — replace the body with the binding-graph
//!    lookup (`children(terminal)` containing a live `AgentSession`).
//! 2. [`AgentDetector::set_live_session_probe`] — call it where the pane
//!    engine builds its detector, with a [`LiveSessionProbe`] closed over
//!    that pane's id and a handle to the same lookup.
//!
//! [`AgentDetector::set_live_session_probe`]: super::AgentDetector::set_live_session_probe

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling runtime / state modules"
)]

use std::rc::Rc;

use phux_protocol::ids::TerminalId as WireTerminalId;

use crate::state::ServerState;

/// "Does the Terminal this detector watches own a live `AgentSession` child
/// right now?", bound to one pane.
///
/// Bound rather than parameterised on purpose: the detector is a pure
/// per-pane state machine that has never needed to know its own id, and
/// giving it one only so it could pass it straight back out again would be a
/// worse seam than a closure the wiring site captures. `Rc` because the pane
/// engine is single-threaded by construction (ADR-0003), same as the
/// `Rc<RuleSet>` the detector already holds.
pub(crate) type LiveSessionProbe = Rc<dyn Fn() -> bool>;

/// The same question asked of the whole server, for the command handlers that
/// hold a `ServerState` rather than a pane.
///
/// `REPORT_AGENT_STATE` is the caller (ADR-0103 decision 6): with a live child
/// the report becomes a synthesized `state` record on that child's stream, so
/// one source of truth feeds the arbiter; without one it takes the ADR-0085
/// path straight into the detector.
///
/// Answers `false` until the `AgentSession` engine lands, which is the honest
/// answer while no resource of that kind can exist.
pub(crate) const fn server_has_live_session(
    _state: &ServerState,
    _terminal: &WireTerminalId,
) -> bool {
    false
}
