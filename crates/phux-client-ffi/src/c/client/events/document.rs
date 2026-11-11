//! Projection of whole published documents (`PHUX_CLIENT_EFFECT_DOCUMENT`).
use phux_protocol::ResourceId;

use super::{Client, OwnedEffect};
use crate::c::types::EFFECT_DOCUMENT;

impl Client {
    /// Stage `resource`'s newest published document: `detail` is its wire
    /// kind tag, `seq` its publish sequence, `status_code` the length of the
    /// content type that `bytes` starts with (0 for a tree or surface), and
    /// the rest of `bytes` the document. Live-only, and the last delivered
    /// wins (not the highest `seq`), so it supersedes a staged one of the
    /// same resource instead of queueing behind it. A resource whose slot is
    /// already gone (closed, detached, replaced) stages nothing.
    pub(super) fn document_effect(&mut self, resource: &ResourceId) {
        let Some(publication) = self.runtime.acquire_document(resource) else {
            return;
        };
        self.owned_effects.retain(|effect| {
            effect.kind != EFFECT_DOCUMENT || effect.terminal_id != publication.resource_id
        });
        let content_type = publication.content_type.as_deref().unwrap_or_default();
        let mut effect = OwnedEffect::simple(
            EFFECT_DOCUMENT,
            u32::from(publication.kind.as_wire()),
            publication.resource_id.clone(),
        );
        effect.seq = publication.seq;
        effect.status_code = u32::try_from(content_type.len()).unwrap_or(u32::MAX);
        effect
            .bytes
            .reserve_exact(content_type.len() + publication.bytes.len());
        effect.bytes.extend_from_slice(content_type.as_bytes());
        effect.bytes.extend_from_slice(&publication.bytes);
        self.owned_effects.push(effect);
    }
}
