//! Document resources (accessibility trees, pixel surfaces and typed
//! documents): read-only tiles drawn from each newest publication by its
//! content type. They take a layout leaf but never input or a resize.

use phux_client_core::document_view::{self, DocumentContent};
use phux_client_core::session::DocumentPublication;
use phux_protocol::wire::info::SessionSnapshot;

use super::*;

/// One document resource's newest publication, as the page draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebDocument {
    /// Which document kind.
    pub kind: ResourceKind,
    /// Its declared name, if any.
    pub name: Option<String>,
    /// The newest publication's sequence; `None` before the first.
    pub seq: Option<u64>,
    /// A typed document's declared content type.
    pub content_type: Option<String>,
    /// What the newest publication draws as; `None` before the first.
    pub content: Option<DocumentContent>,
    /// The newest publication's bytes when it is an image; empty otherwise.
    pub image: Vec<u8>,
}

impl WebDocument {
    fn new(kind: ResourceKind, name: Option<String>) -> Self {
        Self {
            kind,
            name,
            seq: None,
            content_type: None,
            content: None,
            image: Vec::new(),
        }
    }

    /// The line over the tile: its name, what it is, and how much of cut
    /// text is shown.
    #[must_use]
    pub fn caption(&self) -> String {
        let name = self.name.as_deref().filter(|n| !n.trim().is_empty());
        let what = self.content_type.as_deref().unwrap_or(match self.kind {
            ResourceKind::AccessibilityTree => "accessibility tree",
            ResourceKind::PixelSurface => "surface",
            _ => "document",
        });
        let mut caption = name.map_or_else(|| what.to_owned(), |name| format!("{name} · {what}"));
        if let Some(DocumentContent::Text { cut: Some(cut), .. }) = &self.content {
            caption = format!("{caption} · {cut}");
        }
        caption
    }

    /// The tile's one line when it draws neither text nor an image.
    #[must_use]
    pub fn notice(&self) -> Option<String> {
        match self.content.as_ref() {
            None => Some("Waiting for the producer’s first document…".to_owned()),
            Some(DocumentContent::Empty) => Some("Empty document".to_owned()),
            Some(DocumentContent::Opaque { content_type, len }) => {
                Some(format!("{content_type} · {}", size(*len)))
            }
            Some(DocumentContent::Invalid(why)) => Some(why.clone()),
            Some(DocumentContent::Text { .. } | DocumentContent::Image(_)) => None,
        }
    }

    fn publish(&mut self, publication: &DocumentPublication) -> bool {
        if self.seq.is_some_and(|seq| seq >= publication.seq) {
            return false;
        }
        let declared = publication.content_type.as_deref();
        let content = document_view::decode(publication.kind, declared, &publication.bytes);
        self.image = if matches!(content, DocumentContent::Image(_)) {
            publication.bytes.clone()
        } else {
            Vec::new()
        };
        self.kind = publication.kind;
        self.seq = Some(publication.seq);
        self.content_type.clone_from(&publication.content_type);
        self.content = Some(content);
        true
    }
}

/// A byte count as a person reads it.
fn size(len: usize) -> String {
    const KIB: f64 = 1024.0;
    let bytes = len as f64;
    match len {
        1 => "1 byte".to_owned(),
        0..1024 => format!("{len} bytes"),
        1024..1_048_576 => format!("{:.1} KiB", bytes / KIB),
        _ => format!("{:.1} MiB", bytes / (KIB * KIB)),
    }
}

impl Connection {
    /// Every document tile on screen and its local rectangle.
    #[must_use]
    pub fn document_tiles(&self) -> Vec<(ResourceId, PaneRect, &WebDocument)> {
        self.pane_rects()
            .into_iter()
            .filter_map(|(id, rect)| {
                let document = self.documents.get(&id)?;
                Some((id, rect, document))
            })
            .collect()
    }

    /// A document resource this connection holds.
    #[must_use]
    pub fn document(&self, id: &ResourceId) -> Option<&WebDocument> {
        self.documents.get(id)
    }

    /// Whether `id` is a document resource rather than a terminal.
    pub(super) fn is_document(&self, id: &ResourceId) -> bool {
        self.documents.contains_key(id)
            || self
                .kernel
                .as_ref()
                .and_then(|kernel| kernel.resource_kind(id))
                .is_some_and(ResourceKind::is_document)
    }

    /// The snapshot's documents, in its order, after its terminals in the layout.
    pub(super) fn learn_documents(&mut self, snapshot: &SessionSnapshot) {
        let listed = snapshot.resources.iter().filter(|r| r.kind.is_document());
        self.document_order = listed.clone().map(|r| r.id.clone()).collect();
        self.documents
            .retain(|id, _| snapshot.resources.iter().any(|r| &r.id == id));
        for resource in listed {
            self.documents
                .entry(resource.id.clone())
                .or_insert_with(|| WebDocument::new(resource.kind, None))
                .name
                .clone_from(&resource.title);
        }
    }

    /// Keep a newer publication. Returns whether it named a document this
    /// connection had not seen, which then wants a tile.
    pub(super) fn store_document(
        documents: &mut HashMap<ResourceId, WebDocument>,
        order: &mut Vec<ResourceId>,
        publication: &DocumentPublication,
    ) -> bool {
        let id = &publication.resource_id;
        let new = !documents.contains_key(id);
        if new {
            order.push(id.clone());
        }
        documents
            .entry(id.clone())
            .or_insert_with(|| WebDocument::new(publication.kind, None))
            .publish(publication);
        new
    }

    /// Give each document a tile while the layout has room, and resize the
    /// terminals that give it space.
    pub(super) fn place_documents(&mut self, outcome: &mut Outcome) {
        let mut placed = false;
        for id in self.document_order.clone() {
            let leaves: Vec<_> = self.pane_rects().into_iter().map(|(id, _)| id).collect();
            if leaves.len() >= 4 || leaves.contains(&id) {
                continue;
            }
            match (&mut self.layout, leaves.last()) {
                (Some(layout), Some(last)) => {
                    layout.insert(last, id, Axis::Vertical);
                }
                _ => self.layout = Some(Layout::Leaf(id)),
            }
            placed = true;
        }
        if placed {
            outcome.panes = true;
            if self.attach_ready {
                outcome.send.extend(self.pane_resize_frames());
            }
        }
    }

    /// Forget a closed document.
    pub(super) fn forget_document(&mut self, id: &ResourceId) {
        self.documents.remove(id);
        self.document_order.retain(|listed| listed != id);
    }
}
