//! Kind-neutral placement admission, independent of document content.
use super::*;

impl Connection {
    /// A terminal accepts input, or a document has a live organization identity.
    #[must_use]
    pub fn ready(&self, id: &ResourceId) -> bool {
        !self.closed
            && ((self.status == Status::Attached && self.document_kind(id).is_some())
                || ((self.known.contains(id) || self.admitted.contains(id))
                    && self.client.input_ready(id)))
    }

    /// The live documents of session `name`, from the organization
    /// replica: the topology lists terminals only.
    #[must_use]
    pub fn documents_in(&self, name: &str) -> Vec<ResourceId> {
        self.with_organization(|replica| {
            let session = replica
                .sessions()
                .iter()
                .find(|session| session.name == name)
                .map(|session| session.key);
            replica
                .resources()
                .iter()
                .filter(|record| {
                    documents::is_document(record.kind) && Some(record.session) == session
                })
                .filter_map(|record| record.live.clone())
                .collect()
        })
    }

    pub(crate) fn document_kind(&self, id: &ResourceId) -> Option<ResourceKind> {
        self.with_organization(|replica| {
            let record = replica.resource_by_live(id)?;
            documents::is_document(record.kind).then_some(record.kind)
        })
    }

    pub(crate) fn resource_known(&self, id: &ResourceId) -> bool {
        self.pane(id).is_some() || self.document_kind(id).is_some()
    }

    pub(crate) fn resource_record(
        &self,
        id: &ResourceId,
    ) -> Option<phux_protocol::wire::frame::ResourceRecord> {
        self.with_organization(|replica| replica.resource_by_live(id).cloned())
    }

    pub(crate) fn document_label(&self, id: &ResourceId) -> Option<String> {
        self.with_organization(|replica| {
            let record = replica.resource_by_live(id)?;
            let fallback = match record.kind {
                ResourceKind::AccessibilityTree => "Accessibility tree",
                ResourceKind::PixelSurface => "Pixel surface",
                _ => return None,
            };
            Some(record.name.clone().unwrap_or_else(|| fallback.into()))
        })
    }
}
