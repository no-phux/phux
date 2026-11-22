//! The phux tool catalog, `tools/call` dispatch, and the shared argument
//! helpers.
//!
//! Each tool wraps the `phux-client` function the CLI verb calls (ADR-0022
//! §5); the residue that still runs the CLI is named in `crate::tool_table`.
//! A tool returns a JSON `Value` or a [`ToolError`], which becomes a
//! `tools/call` result with `isError: true`, never a crash.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in canonical CLI adapters"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_client::contract::{Refusal, error_document, transport_refusal};
use phux_client::detach::DetachOutcome;
use phux_client::read::{self, Resolved, Route, Verb};
use phux_client::selector::{self, Selector};
use phux_client::snapshot::{PROJECTED_DOCUMENT_MAX_BYTES, ScreenState};
use phux_client::state::{self, StateView};
use phux_client::wait::{Condition, DEFAULT_IDLE_DWELL, DEFAULT_POLL_INTERVAL, WaitOutcome};
use phux_client::watch::WatchItem;
use phux_protocol::ids::ResourceId;
use phux_protocol::input::paste::PasteTrust;
use serde_json::{Value, json};

include!("tools/errors.rs");
include!("tools/catalog.rs");
include!("tools/dispatch.rs");
include!("tools/reads.rs");
include!("tools/input.rs");
include!("tools/upload.rs");
include!("tools/watch.rs");
include!("tools/lifecycle.rs");
include!("tools/parsing.rs");
include!("tools/kinds.rs");
#[path = "tools/signals.rs"]
pub(crate) mod signals;
include!("tools/tests.rs");
include!("tools/error_tests.rs");
include!("tools/upload_tests.rs");

/// Longest `timeout_secs` any bounded tool accepts. A tool call that never
/// returns wedges its host, so no bounded tool has an unbounded mode.
pub(crate) const MAX_TIMEOUT_SECS: u64 = 3600;

/// The `timeout_secs` argument of a bounded tool.
///
/// An integer from 1 to [`MAX_TIMEOUT_SECS`]; `default` when absent, or
/// required when `default` is `None`.
pub(crate) fn bounded_timeout_secs(args: &Value, default: Option<u64>) -> Result<u64, ToolError> {
    let out_of_range = || {
        ToolError::new(format!(
            "`timeout_secs` must be an integer in 1..={MAX_TIMEOUT_SECS}"
        ))
    };
    args.get("timeout_secs").map_or_else(
        || default.ok_or_else(out_of_range),
        |value| {
            value
                .as_u64()
                .filter(|secs| (1..=MAX_TIMEOUT_SECS).contains(secs))
                .ok_or_else(out_of_range)
        },
    )
}
