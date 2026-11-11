//! Live document mirrors. Organization keys are identity; handles are only
//! addresses on the current connection. No terminal view is ever allocated.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use phux_client_core::session::DocumentPublication;
use phux_client_runtime::Client;
use phux_protocol::ids::{ResourceId, ResourceKey, ResourceKind};
use phux_protocol::wire::frame::ResourceRecord;

use super::Desk;

mod input;
mod satellite;
mod screens;
mod selection;
mod startup;
mod subscriptions;
mod surface;
mod typed;
pub use startup::Source;
use subscriptions::Subscription;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub struct Resources {
    pub entries: HashMap<ResourceId, Document>,
    pub images: Option<rune::ImageCache>,
    decodes: surface::Decodes,
    pub(super) dismissed: HashSet<ResourceKey>,
    pub(super) returning: Option<ResourceKey>,
    pub(super) starting: HashMap<u32, Source>,
    pub(super) producers: HashMap<ResourceId, Source>,
    server: Option<Vec<u8>>,
    pending: HashMap<ResourceId, PendingDocument>,
    // RESOURCE_CLOSED can precede the organization journal. Fence that
    // execution until inventory removes or replaces it, never the stable key.
    closed: HashMap<ResourceId, ResourceRecord>,
    /// Which documents take pane input, and the sends awaiting a reply.
    pub(super) input: phux_client_core::document_input::DocumentInputs,
    /// The copy from a host clipboard awaiting its reply.
    pub(super) copying: Option<u32>,
    /// The paste in flight to a host, whose failure is worth a notice.
    pub(super) pasting: Option<u32>,
    /// The screen and window picker.
    pub screens: screens::Screens,
}

struct PendingDocument {
    publication: Arc<DocumentPublication>,
    since: Instant,
}

pub struct Document {
    pub record: ResourceRecord,
    pub seq: Option<u64>,
    pub body: Body,
    /// A typed document's content type, and what is shown of it.
    pub detail: Option<String>,
    surface: surface::Surface,
    subscription: Subscription,
}

pub enum Body {
    Pending,
    Decoding,
    Empty,
    Invalid(String),
    /// Source text: accessibility JSON or a typed text document.
    Tree(String),
    Surface(rune::Image),
}

pub const fn document_kind(kind: ResourceKind) -> bool {
    kind.is_document()
}

impl Resources {
    pub(super) fn execution_closed(&self, record: &ResourceRecord) -> bool {
        record
            .live
            .as_ref()
            .and_then(|id| self.closed.get(id))
            .is_some_and(|closed| same_execution(closed, record))
    }
}

pub fn text_key(id: &ResourceId) -> String {
    format!("document-json-{id}")
}

impl Document {
    fn new(record: ResourceRecord) -> Self {
        Self {
            record,
            seq: None,
            body: Body::Pending,
            detail: None,
            surface: surface::Surface::default(),
            subscription: Subscription::Unrequested,
        }
    }

    fn publish(
        &mut self,
        publication: &DocumentPublication,
        images: Option<&rune::ImageCache>,
        decodes: &mut surface::Decodes,
    ) -> bool {
        if publication.kind != self.record.kind
            || self.seq.is_some_and(|seq| seq >= publication.seq)
        {
            return false;
        }
        let content_type = publication.content_type.as_deref().unwrap_or_default();
        self.detail = publication.content_type.clone();
        if publication.kind == ResourceKind::PixelSurface {
            self.publish_surface(&publication.bytes, images, decodes);
        } else if publication.kind == ResourceKind::Document && typed::image(content_type) {
            self.publish_image(&publication.bytes, images, decodes);
        } else if publication.kind == ResourceKind::Document && !publication.bytes.is_empty() {
            self.invalidate_surface();
            let (body, detail) = typed::decode(content_type, &publication.bytes);
            self.body = body;
            self.detail = Some(detail);
        } else {
            self.body = decode(publication);
        }
        self.seq = Some(publication.seq);
        true
    }
}

fn decode(publication: &DocumentPublication) -> Body {
    if publication.bytes.is_empty() {
        return Body::Empty;
    }
    match publication.kind {
        ResourceKind::AccessibilityTree => {
            match serde_json::from_slice::<serde_json::Value>(&publication.bytes) {
                Ok(value) => Body::Tree(serde_json::to_string_pretty(&value).expect("JSON value")),
                Err(error) => Body::Invalid(format!("Invalid JSON: {error}")),
            }
        }
        _ => Body::Invalid("Unsupported document kind".to_owned()),
    }
}

fn same_execution(a: &ResourceRecord, b: &ResourceRecord) -> bool {
    a.key == b.key
        && a.kind == b.kind
        && a.live == b.live
        && a.activation.map(|a| (a.id, a.instance)) == b.activation.map(|a| (a.id, a.instance))
}

impl Desk {
    pub(super) fn finish_document_images(&mut self) -> bool {
        self.resources.decodes.reap();
        let mut changed = false;
        // A document waiting for admission gets first use of freed slots;
        // continuously publishing surfaces cannot monopolize all decoders.
        for document in self
            .resources
            .entries
            .values_mut()
            .filter(|d| d.surface.flight.is_none())
        {
            changed |= document
                .finish_surface(self.resources.images.as_ref(), &mut self.resources.decodes);
        }
        for document in self
            .resources
            .entries
            .values_mut()
            .filter(|d| d.surface.flight.is_some())
        {
            changed |= document
                .finish_surface(self.resources.images.as_ref(), &mut self.resources.decodes);
        }
        if changed {
            self.generation += 1;
        }
        changed
    }
    pub fn is_document(&self, id: &ResourceId) -> bool {
        self.resources.entries.contains_key(id)
    }

    pub(super) fn listed_resources(&self) -> Vec<ResourceId> {
        self.topology
            .iter()
            .flat_map(|t| t.panes.iter().map(|p| p.terminal_id.clone()))
            .chain(self.resources.entries.keys().cloned())
            .collect()
    }

    pub(super) fn lists_resource(&self, id: &ResourceId) -> bool {
        self.is_document(id) || self.lists(id)
    }

    pub(super) fn default_document(&self, session: u32) -> Option<ResourceId> {
        if self.status != phux_client_runtime::control::Status::Attached {
            return None;
        }
        let client = self.client.as_ref()?;
        let org = client.organization();
        if !org.is_live() {
            return None;
        }
        let key = org.session_by_live(phux_protocol::SessionId(session))?.key;
        let listed: Vec<_> = org
            .members(&key)
            .filter(|r| document_kind(r.kind) && !self.resources.dismissed.contains(&r.key))
            .filter(|r| !self.resources.execution_closed(r))
            .filter_map(|r| r.live.clone())
            .collect();
        org.arrangement_document(&key)
            .and_then(|(_, _, doc, _)| {
                doc.active_group()
                    .and_then(|g| g.tree.as_ref())
                    .map(phux_client_core::layout::leaves)
            })
            .unwrap_or_default()
            .into_iter()
            .find(|id| listed.contains(id))
            .or_else(|| listed.first().cloned())
    }

    pub(super) fn publish_document(&mut self, publication: &Arc<DocumentPublication>) -> bool {
        let Some(document) = self.resources.entries.get_mut(&publication.resource_id) else {
            // Bootstrap may beat the paged organization seed. Retain only
            // the newest cut until its identity arrives on this epoch.
            let pending = self
                .resources
                .pending
                .entry(publication.resource_id.clone());
            let newer = pending.or_insert_with(|| PendingDocument {
                publication: Arc::clone(publication),
                since: Instant::now(),
            });
            if newer.publication.seq < publication.seq {
                *newer = PendingDocument {
                    publication: Arc::clone(publication),
                    since: Instant::now(),
                };
            }
            return false;
        };
        let changed = document.publish(
            publication,
            self.resources.images.as_ref(),
            &mut self.resources.decodes,
        );
        if changed {
            self.generation += 1;
        }
        changed
    }

    pub(super) fn retire_documents(&mut self, client: &Client) {
        let key = self
            .shown
            .as_ref()
            .and_then(|s| self.resources.entries.get(&s.terminal))
            .map(|d| d.record.key);
        if key.is_some() {
            self.resources.returning = self.resources.returning.or(key);
            self.drop_shown(client);
            self.wanted = None;
        }
        self.resources.entries.clear();
        self.resources.pending.clear();
        self.resources.closed.clear();
        self.resources.input.clear();
        // starting holds command correlations, not live resource handles;
        // reconnect receipts must settle them (including producer failures).
        self.resources.producers.clear();
    }

    pub(super) fn carry_document_focus(&mut self, next: &mut Desk) {
        next.resources.returning = self.resources.returning.or_else(|| {
            self.shown
                .as_ref()
                .and_then(|s| self.resources.entries.get(&s.terminal))
                .map(|d| d.record.key)
        });
        next.resources.dismissed = self.resources.dismissed.clone();
    }

    pub(super) fn refresh_resources(&mut self, client: &Client) -> bool {
        self.follow_document_server(client);
        // The replica can still report Live between transport loss and the
        // next HELLO/seed. Its old handles are not current-epoch identity.
        if self.status != phux_client_runtime::control::Status::Attached {
            return false;
        }
        let org = client.organization();
        if !org.is_live() {
            return false;
        }
        let satellites = self.topology.iter().flat_map(|t| &t.documents);
        let live: Vec<_> = org
            .resources()
            .iter()
            .filter(|r| document_kind(r.kind) && r.live.is_some())
            .cloned()
            .chain(satellites.filter_map(satellite::record))
            .collect();
        self.resources
            .closed
            .retain(|_, closed| live.iter().any(|record| same_execution(record, closed)));
        let live: Vec<_> = live
            .into_iter()
            .filter(|record| !self.resources.execution_closed(record))
            .collect();
        self.retire_replaced_document(client, &live);
        let before = self.resources.entries.len();
        self.resources
            .entries
            .retain(|_, doc| live.iter().any(|r| same_execution(r, &doc.record)));
        let mut changed = self.resources.entries.len() != before;
        for record in live {
            let id = record.live.clone().expect("live records");
            match self.resources.entries.entry(id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Document::new(record));
                    changed = true;
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().record = record;
                }
            }
        }
        drop(org);
        changed | self.adopt_pending_documents() | self.restore_document(client)
    }

    fn follow_document_server(&mut self, client: &Client) {
        let server = client.server().map(|s| s.id);
        // None → HELLO identity establishes this epoch; it must not retire a
        // bootstrap cut received before the first session AttachReady.
        if self.resources.server.is_some() && self.resources.server != server {
            self.retire_documents(client);
        }
        self.resources.server = server;
    }

    fn adopt_pending_documents(&mut self) -> bool {
        let mut changed = false;
        for pending in std::mem::take(&mut self.resources.pending).into_values() {
            let publication = &pending.publication;
            if self
                .resources
                .entries
                .contains_key(&publication.resource_id)
            {
                changed |= self.publish_document(publication);
            } else if pending.since.elapsed() < super::WANTED_GRACE {
                self.resources
                    .pending
                    .insert(publication.resource_id.clone(), pending);
            }
        }
        changed
    }

    pub(super) fn forget_document(&mut self, client: &Client, id: &ResourceId) -> bool {
        self.resources.pending.remove(id);
        let record = self
            .resources
            .entries
            .remove(id)
            .map(|d| d.record)
            .or_else(|| {
                client
                    .organization()
                    .resource_by_live(id)
                    .filter(|record| document_kind(record.kind))
                    .cloned()
            });
        let Some(record) = record else { return false };
        if self.resources.execution_closed(&record) {
            return false;
        }
        self.resources.closed.insert(id.clone(), record);
        self.mirror_unavailable();
        true
    }

    fn mirror_unavailable(&mut self) {
        self.say(super::NoticeKind::Error, "Mirror unavailable: its resource was removed or the producer ended. Open another mirror from the palette, or start a new producer.");
    }

    fn retire_replaced_document(&mut self, client: &Client, live: &[ResourceRecord]) {
        let Some(shown) = &self.shown else { return };
        let Some(old) = self.resources.entries.get(&shown.terminal) else {
            return;
        };
        if live.iter().any(|r| same_execution(r, &old.record)) {
            return;
        }
        let key = old.record.key;
        let returning = self.resources.returning.unwrap_or(key);
        self.close_shown(client, &[]);
        // A temporary phone fallback is not an explicit focus change: the
        // replacement execution of this key still takes priority.
        self.resources.returning = Some(returning);
        if !live.iter().any(|record| record.key == key) {
            self.mirror_unavailable();
        }
    }

    fn restore_document(&mut self, client: &Client) -> bool {
        if let Some(key) = self.resources.returning
            && let Some(id) = self
                .resources
                .entries
                .iter()
                .find(|(_, d)| d.record.key == key)
                .map(|(id, _)| id.clone())
        {
            self.want(id);
            return true;
        }
        let gone = self
            .shown
            .as_ref()
            .filter(|s| s.view.is_none())
            .map(|s| s.terminal.clone())
            .filter(|id| !self.is_document(id));
        let Some(gone) = gone else { return false };
        let next = self
            .layout
            .as_ref()
            .filter(|_| self.phone.is_none())
            .and_then(|tree| super::layout::neighbour(tree, &gone));
        self.close_shown(client, &[]);
        if let Some(next) = next {
            self.want_automatic(next);
        }
        true
    }

    /// Closing a mirror removes its view from the shared arrangement. It
    /// does not close a terminal, stop its producer, or migrate its process.
    pub(super) fn hide_documents(&mut self, ids: &[ResourceId]) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.resources.dismissed.extend(
            ids.iter()
                .filter_map(|id| self.resources.entries.get(id).map(|d| d.record.key)),
        );
        if let Some((session, mut doc)) = self.arrangement(&client) {
            remove_document_leaves(&mut doc, ids);
            self.write_arrangement(&client, session, &doc, "Mirror view not closed");
        }
        if self
            .shown
            .as_ref()
            .is_some_and(|s| ids.contains(&s.terminal))
        {
            let next = self
                .layout
                .as_ref()
                .filter(|_| self.phone.is_none())
                .map(phux_client_core::layout::leaves)
                .unwrap_or_default()
                .into_iter()
                .find(|id| !ids.contains(id));
            self.close_shown(&client, ids);
            if let Some(next) = next {
                self.want_automatic(next);
            }
        }
        self.refresh_layout(&client);
    }
}

fn remove_document_leaves(doc: &mut phux_client_core::layout::ArrangementDoc, ids: &[ResourceId]) {
    for group in &mut doc.groups {
        for id in ids {
            if let Some(tree) = &group.state.tree
                && phux_client_core::layout::leaves(tree).contains(id)
            {
                group.state.tree = phux_client_core::layout::kill_pane(tree, id).ok().flatten();
            }
        }
    }
    doc.prune_empty_groups();
}
