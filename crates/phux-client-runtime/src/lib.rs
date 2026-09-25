//! The client runtime: everything between the sans-IO session kernel and a
//! language binding, implemented once (ADR-0133).
//!
//! `phux-client-core` owns terminal and session state and does no I/O.
//! `phux-dial` establishes one QUIC or WebSocket connection. Between them
//! sits the orchestration every consumer needs and used to rebuild:
//!
//! - [`target`] — resolving `[USER@]HOST[:PORT]` against the CLI's
//!   `[[remote]]` registry, rung 1 of the ADR-0093 ladder.
//! - [`dial`] — turning a resolved entry into a dial under the CLI's trust
//!   rules (pin required off loopback, `wss://` and a token for a routable
//!   WebSocket), the operator-facing wording of every failure, and the
//!   SPEC §5 frame cutting the WebSocket lane needs.
//! - [`reconnect`] — the backoff ladder, with one preset per lane
//!   (interactive, agent verb, local upgrade), and the rule for which
//!   refusals no retry can satisfy.
//! - [`tunnel`] — the byte-relay tunnel a socket-owning embedder hands one
//!   end of a Unix-domain socket pair.
//! - [`control`] — the sans-IO control plane over `SessionKernel`: decoded
//!   frames in, encoded frames and owned [`control::Event`]s out. It owns
//!   the connection lifecycle, the topology, per-terminal commands, input,
//!   and the event subscription, and it never touches a socket.
//! - [`engine`] — the owner thread that hosts the kernel and every engine
//!   replica. Ghostty values never cross threads; commands go in over a
//!   channel and owned results come back.
//! - `publication` (feature `engine`) — the double-buffered grid: an
//!   immutable `GridFrame` per terminal with a generation counter and dirty
//!   rows, acquired from any thread.
//! - [`connection`] — the async driver: dial, framing, keepalive, the
//!   reconnect ladder, and the pump that feeds the control plane.
//! - [`runtime`] — [`runtime::Runtime::connect`] and the thread-safe
//!   synchronous [`runtime::Client`] a binding drives.
//!
//! Binding crates (`phux-client-ffi`, phux-mobile's bridge) translate these
//! into their language's idiom and hold no state machine of their own.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod connection;
pub mod control;
pub mod dial;
pub mod engine;
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
pub use view::ViewId;
