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
//!
//! Binding crates (`phux-client-ffi`, phux-mobile's bridge) translate these
//! into their language's idiom and hold no state machine of their own.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod dial;
pub mod reconnect;
pub mod target;
pub mod tunnel;
