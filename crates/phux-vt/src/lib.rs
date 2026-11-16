//! libghostty glue shared by phux's native crates.
//!
//! `phux-protocol` is the published, pure-Rust wire crate; everything that
//! needs a libghostty `Terminal` and is shared by more than one native crate
//! lives here instead. This is a leaf: it depends on the wire and domain
//! crates, never on `phux-client-core`, whose engine adapters build on it.
//!
//! # Crate features
//!
//! - The default build exposes [`input`] conversions between wire atoms and
//!   libghostty's.
//! - **`render-pool`**: `render_pool`, the pooled libghostty render trio,
//!   with no kitty graphics and no png.
//! - **`kitty`**: enables `render-pool`, then adds `sgr`, `kitty_replay`, and
//!   the GHOSTSNP `snapshot` codec, over libghostty's kitty-graphics and png.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod input;

#[cfg(feature = "kitty")]
pub mod kitty_replay;
#[cfg(feature = "render-pool")]
pub mod render_pool;
#[cfg(feature = "kitty")]
pub mod sgr;
#[cfg(feature = "kitty")]
pub mod snapshot;
