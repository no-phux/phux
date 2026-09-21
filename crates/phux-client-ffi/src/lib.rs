//! The phux binding crate: one projection of the client runtime, one encoder
//! per foreign language.
//!
//! [`projection`] derives the product vocabulary — terminal signals,
//! lifecycle answers, topology, status, delivery outcomes, receipts, agent
//! records and grid frames — from `phux-client-runtime` values exactly once,
//! in binding-neutral Rust. Nothing in it is `#[repr(C)]`.
//!
//! [`c`] is the encoder over that layer: the stable `extern "C"` surface
//! `include/phux/client.h` declares, which Cockpit links as a staticlib.

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

pub mod projection;

mod c;
pub use c::*;
