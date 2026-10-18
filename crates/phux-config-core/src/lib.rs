//! phux-config-core: the client-neutral half of `config.toml`.
//!
//! Where the file lives, how its `extends` layers merge (ADR-0039), the
//! tables every client reads (`[[remote]]`, `[organize]`), and the per-user
//! instance paths. The TUI's schema, its shipped `default.toml` and the
//! status-bar widgets live in `phux-config`, which re-exports this crate, so
//! a client that only dials (the desktop) never builds them.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

mod error;
pub mod instance;
pub mod known_authorities;
pub mod layer;
pub mod loader;
mod organize;
pub mod remote;
pub mod socket;

pub use error::{ConfigError, byte_offset_to_line_col};
pub use layer::{ConfigProvenance, KeyOrigin, LayerSource, MAX_EXTENDS_DEPTH};
pub use loader::ClientConfig;
pub use organize::OrganizeCfg;
pub use remote::RemoteConfigEntry;
