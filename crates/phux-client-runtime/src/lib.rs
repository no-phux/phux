//! The client runtime: everything between the sans-IO session kernel and a
//! language binding, implemented once (ADR-0133).
//!
//! - [`target`]: resolve `[USER@]HOST[:PORT]` against the `[[remote]]`
//!   registry (ADR-0093 rung 1).
//! - [`dial`]: dial a resolved entry under the CLI's trust rules.
//! - [`reconnect`]: backoff ladders and fatal-refusal rules.
//! - [`tunnel`]: the byte relay for a socket-owning embedder.
//! - [`control`]: the sans-IO control plane over `SessionKernel`.
//! - [`engine`]: the owner thread hosting the kernel and every replica.
//! - [`perf`]: owner-thread apply and publication telemetry.
//! - `publication` (feature `engine`): the double-buffered grid.
//! - [`connection`]: the async driver (dial, framing, keepalive, reconnect).
//! - [`runtime`]: [`runtime::Runtime::connect`] and the thread-safe
//!   [`runtime::Client`] a binding drives.
//!
//! Binding crates translate these into their language and hold no state
//! machine of their own.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod connection;
pub mod control;
pub mod dial;
pub mod engine;
pub mod perf;
#[cfg(feature = "engine")]
pub mod publication;
pub mod reconnect;
pub mod runtime;
pub mod target;
pub mod tunnel;

pub use runtime::{
    Client, ClientOptions, ConnectOptions, ControlGuard, Lane, Listener, PumpError, Runtime,
    Target, Transport,
};
mod view;
/// The TLS client identity a [`Target`] presents; re-exported so embedders
/// can name it without depending on `phux-dial`.
pub use phux_dial::{AuthorityLearner, TlsClientIdentity};
pub use view::ViewId;
