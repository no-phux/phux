//! The Terminal app's host side: session attach and the input lane, over the
//! engine in `phux-terminal`.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

pub mod attach;
pub(crate) mod commands;
pub mod input_lane;

pub(crate) use commands::TerminalApp;
