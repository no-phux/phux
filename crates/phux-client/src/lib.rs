//! phux headless client library.
//!
//! The control-plane half of a phux client: connect over UDS, QUIC, or
//! WebSocket, negotiate HELLO, and drive the wire verbs behind every `phux`
//! agent verb and the `phux-mcp` adapter (`docs/consumers/sdk.md`). It owns
//! no PTY and no screen and links no `ratatui`; the interactive attach is
//! `phux-tui`, which depends on this crate (ADR-0100). The pane-interior
//! substrate is `phux-client-core`, re-exported as [`layout`],
//! [`multi_pane`], and [`predict`].
//!
//! The `testkit` feature exposes the scripted server the workspace's
//! client-side tests speak to.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod agent_meta;
// `phux.agent/v1` wire round trips (ADR-0040); the record type is `agent_meta`.
pub mod agent_record;
// The AgentSession resource (ADR-0103).
pub mod agent_session;
// Provider-native session provenance (`phux.agent-session/v1`), distinct
// from both `agent_record` and the `agent_session` resource.
pub mod agent_session_record;
// Acknowledged, idempotent input delivery to an agent (ADR-0053, ADR-0076).
pub mod agent_prompt;
// Edge-triggered lifecycle wait over `phux.agent/v1` (ADR-0076 point 5).
pub mod agent_wait;
pub mod approvals;
pub mod ask;
pub mod attach;
// Conditional kills bound to the server's instance token (ADR-0109).
pub mod conditional_kill;
pub mod deadline;
// `DETACH_CLIENTS` (`phux detach`).
pub mod detach;
pub mod explain;
// `SHUTDOWN` / `KILL_RESOURCES` / `KILL_RESOURCE` (`phux kill`).
pub mod kill;
pub mod layout_ops;
pub mod pane_move;
pub mod perf;
pub mod record;
pub mod resize;
pub mod resource;
pub mod run;
pub mod selector;
pub mod send_keys;
// Session-identity writes over L3 (`phux rename`).
pub mod session;
// The `phux ls --json` document, shared by the CLI and MCP.
pub mod session_list;
// `ACQUIRE_INPUT` / `RELEASE_INPUT` / `SIGNAL_TERMINAL` (ADR-0033).
pub mod signal;
pub mod snapshot;
// `insert-pane` / `move-pane` / `swap-pane`, shared by the CLI and MCP.
pub mod spatial;
// `SPAWN_RESOURCE` and the placement rollback behind `phux spawn`.
pub mod spawn;
pub mod state;
// `phux.tags/v1` read/write (ADR-0027).
pub mod tags;
// `UPGRADE` (`phux upgrade`, ADR-0032).
pub mod upgrade;
// The scripted server every client-side test speaks to.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
pub mod vcs;
pub mod wait;
pub mod watch;

pub use phux_client_core::{layout, multi_pane, predict, rename};
