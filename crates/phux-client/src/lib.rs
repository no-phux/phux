//! phux headless client library.
//!
//! The control-plane half of a phux client: connect to a server over UDS,
//! QUIC, or WebSocket, negotiate HELLO, and drive the wire verbs an agent or
//! a CLI needs -- structured state reads, side-effect-free screen snapshots,
//! keystroke routing, waits, event watches, layout edits, recording. Every
//! `phux` agent verb and the `phux-mcp` adapter are thin projections over
//! the free functions in this crate (`docs/consumers/sdk.md`).
//!
//! This crate knows nothing about PTYs and owns no screen: it links no
//! `ratatui` and never touches the controlling terminal. The interactive
//! attach -- raw mode, the libghostty replicas, the chrome, the keybinding
//! dispatcher -- is the `phux-tui` crate, which depends on this one and
//! never the reverse (ADR-0100). The pane-interior substrate both share
//! (layout math, multi-pane composition, predictive echo, the session
//! kernel) is `phux-client-core`, re-exported here as [`layout`],
//! [`multi_pane`], and [`predict`] so consumers keep their
//! `phux_client::{layout, predict, ...}` paths.
//!
//! # Features
//!
//! `testkit` exposes [`testkit`], the one scripted server every client-side
//! unit test in the workspace speaks to. There is no `tui` feature any more:
//! the TUI is a crate.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod agent_meta;
// The AgentSession resource: open/close/emit/log over the wire verbs the
// server already has (ADR-0103). Sits beside `agent_wait` and above
// `attach::connection`; the selector's `%name` production caller lives in
// `selector` and reaches sessions through `resource`.
pub mod agent_session;
// Acknowledged, idempotent input delivery to an agent (ADR-0053, ADR-0076
// points 1-4/6/7). Sits above `attach::connection` and beside `agent_wait`,
// whose `EdgeTracker` predicate it reuses rather than re-deriving: `prompt
// --wait` differs from `agent wait` only in that its subscription, its
// baseline read, its submit, and its wait all share ONE connection.
pub mod agent_prompt;
// Edge-triggered lifecycle wait over `phux.agent/v1` (ADR-0076 point 5).
// Sits above `watch` (the subscription) and beside `wait` (the poll floor),
// and is deliberately its own module: its predicate is "an observed
// transition", which is a different contract from `wait`'s screen-level
// conditions and must not be confused with them.
pub mod agent_wait;
pub mod ask;
pub mod attach;
// Conditional kills (ADR-0109): bind a spawn to the server's instance token
// and kill it later only if it is still untouched. Pure builders plus one
// request wrapper over `attach::connection`.
pub mod conditional_kill;
pub mod deadline;
pub mod explain;
pub mod layout_ops;
pub mod pane_move;
pub mod perf;
pub mod record;
// Resource kinds as a snapshot carries them: which entries are panes, which
// are children of one, and the two directions of the parent binding.
pub mod resize;
pub mod resource;
pub mod run;
pub mod selector;
pub mod send_keys;
pub mod snapshot;
pub mod state;
// The one scripted server every client-side test speaks to (phux-h5hj.3).
// Compiled for this crate's own unit tests, and behind the `testkit` feature
// for the downstream crates (`phux-mcp`, the `phux` binary) whose unit tests
// used to hand-roll their own fake — and therefore encoded whatever their
// author believed the server does.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
pub mod vcs;
pub mod wait;
pub mod watch;

// Pane-interior substrate, re-exported from `phux-client-core` so the
// `ratatui`-free boundary is compiler-enforced (ADR-0020) while consumers
// keep stable `phux_client::{layout, multi_pane, predict}` paths.
pub use phux_client_core::{layout, multi_pane, predict};
