//! The ADR-0040 agent-identity record end to end: `agent set` writes
//! `phux.agent/v1` over `SET_METADATA`, `agent show` reports it with
//! `agent_record` authority, and `agent clear` falls back to the other
//! sources. Also drives the wrapper `agent install-claude` generates, through
//! its `--phux-hook` entry, against a pane painting a real Claude permission
//! dialog: the record must carry a state only `rules/claude.toml` produces.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::ServerGuard;

include!("agent_record_e2e/support.rs");
include!("agent_record_e2e/identity.rs");
include!("agent_record_e2e/claude_detector.rs");
include!("agent_record_e2e/claude_stream.rs");
include!("agent_record_e2e/live_titles.rs");
include!("agent_record_e2e/delegation.rs");
