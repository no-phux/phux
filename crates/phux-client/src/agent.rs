//! The agent level reads, shared by `phux agent list|show|explain` and the
//! MCP agent tools so both report one projection.
//!
//! [`detect`] builds a pane's [`detect::AgentStateReport`] from its
//! `phux.agent/v1` record and the detector's evidence; [`reads`] runs the
//! verbs over a socket and hands each surface the reports, or the failure,
//! to print on its own channel.

pub mod detect;
pub mod reads;
