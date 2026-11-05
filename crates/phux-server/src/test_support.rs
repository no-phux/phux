//! Explicit native composition used only by host unit fixtures.
#![allow(
    clippy::redundant_pub_crate,
    reason = "test-only re-export for sibling host fixtures, not a public server API"
)]
pub(super) use phux_terminal::downsample;
