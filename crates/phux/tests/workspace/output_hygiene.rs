//! Output-hygiene contracts of the real binary, with no server (dead socket
//! paths, so nothing auto-spawns): one-shot verbs print no banner, `--json`
//! puts only JSON on stdout and errors on stderr, `--version` is clean stdout,
//! and a closed reader (`| head`) exits 0 silently. Every spawn goes through
//! [`phux`], which disables overlay detection so `doctor` never dials a real
//! tailnet server.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::process::{Command, Stdio};

use tempfile::TempDir;

include!("output_hygiene/support.rs");
include!("output_hygiene/cli.rs");
include!("output_hygiene/plugin_config.rs");
include!("output_hygiene/json_errors.rs");
include!("output_hygiene/closed_stdout.rs");
include!("output_hygiene/styling.rs");
