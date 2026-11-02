//! Whether a Terminal owns a live `AgentSession` child (ADR-0103 §5), the
//! top rung above the screen detector.
//!
//! The detector cannot see bindings, so
//! the answer is injected as a [`LiveSessionProbe`]. No probe means no child
//! (test and upgrade-rebuilt actors).

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling runtime / state modules"
)]

use std::rc::Rc;

/// "Does this pane own a live `AgentSession` child now?", bound per pane.
pub type LiveSessionProbe = Rc<dyn Fn() -> bool>;
