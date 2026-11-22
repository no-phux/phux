//! A hub's satellite documents. The hub's organization holds only its own
//! records, so a satellite's trees, surfaces and typed documents are found
//! in its inventory listing instead, each given a record that stands in for
//! the one the satellite keeps.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use phux_client_runtime::control::DocumentDescriptor;
use phux_protocol::ids::{ResourceKey, Revision, SessionKey};
use phux_protocol::wire::frame::ResourceRecord;

/// The stand-in record for a satellite document; `None` for a local one,
/// which the organization lists. Its keys derive from the id, so the same
/// document keeps them across refreshes.
pub(super) fn record(document: &DocumentDescriptor) -> Option<ResourceRecord> {
    let host = document.id.host()?;
    let key = ResourceKey::new(digest(&document.id.to_string()))?;
    let session = SessionKey::new(digest(host.as_str()))?;
    Some(
        ResourceRecord::new(key, session, Revision::new(0))
            .with_name(document.name.clone())
            .with_kind(document.kind)
            .with_live(Some(document.id.clone())),
    )
}

fn digest(text: &str) -> [u8; 16] {
    let half = |salt: u8| {
        let mut hasher = DefaultHasher::new();
        (salt, text).hash(&mut hasher);
        hasher.finish().to_le_bytes()
    };
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&half(0));
    bytes[8..].copy_from_slice(&half(1));
    bytes
}
