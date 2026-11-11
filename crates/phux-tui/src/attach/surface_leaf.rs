//! Layout leaves that are not Terminals: a document, accessibility tree,
//! pixel surface, or agent session another client placed in the arrangement
//! (docs/resource-kinds.md, "any resource with a surface can be a layout
//! leaf"). The TUI has no renderer for them, so it tiles a labelled
//! placeholder in the leaf's rect and never sends the leaf input or a resize.
//!
//! A leaf's kind comes first from the server's resource graph (the
//! `ATTACHED` snapshot, later inventories, and live `RESOURCE_SPAWNED`
//! events), recorded in a [`LeafBook`]; the session kernel's kind, learned
//! from a bootstrap, is the fallback. A leaf neither knows keeps the
//! Terminal path.

use std::collections::HashMap;
use std::io::Write;

use phux_protocol::ResourceKind;
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::info::ResourceInfo;

use super::pane_state::AttachKernel;
use crate::layout::Rect;

/// The placeholder's last line: why the tile is empty.
pub(super) const PLACEHOLDER_HINT: &str = "Not shown in the terminal";

/// What the server's graph says about one non-Terminal resource.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LeafEntry {
    kind: ResourceKind,
    /// The producer-given name, already stripped for the TTY.
    title: Option<String>,
    session: Option<SessionId>,
}

/// The non-Terminal resources the server's graph has named, by id.
#[derive(Debug, Default)]
pub(super) struct LeafBook {
    entries: HashMap<ResourceId, LeafEntry>,
}

impl LeafBook {
    /// Start over from an `ATTACHED` snapshot's graph.
    pub(super) fn replace(&mut self, resources: &[ResourceInfo]) {
        self.entries.clear();
        let _ = self.learn(resources);
    }

    /// Fold in a (possibly partial) inventory. A spawn noted since the
    /// inventory was requested is kept. Returns the ids whose kind or name
    /// this changed, so a placeholder showing one can repaint.
    pub(super) fn learn(&mut self, resources: &[ResourceInfo]) -> Vec<ResourceId> {
        let mut changed = Vec::new();
        for info in resources {
            if info.kind == ResourceKind::Terminal {
                if self.entries.remove(&info.id).is_some() {
                    changed.push(info.id.clone());
                }
                continue;
            }
            let entry = LeafEntry {
                kind: info.kind,
                title: info.title.as_deref().and_then(clean_title),
                session: info.session,
            };
            if self.entries.insert(info.id.clone(), entry.clone()) != Some(entry) {
                changed.push(info.id.clone());
            }
        }
        changed
    }

    /// Note a live `RESOURCE_SPAWNED` of `kind`. `true` when this is the
    /// first word that `id` is not a Terminal.
    pub(super) fn note_spawned(&mut self, id: &ResourceId, kind: ResourceKind) -> bool {
        if kind == ResourceKind::Terminal || self.entries.contains_key(id) {
            return false;
        }
        self.entries.insert(
            id.clone(),
            LeafEntry {
                kind,
                title: None,
                session: None,
            },
        );
        true
    }

    /// The documents, trees, and surfaces `session` holds, sorted: views
    /// `reopen-views` may restore. Agent sessions belong to their pane.
    pub(super) fn documents_in(&self, session: SessionId) -> Vec<ResourceId> {
        let mut ids: Vec<ResourceId> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.session == Some(session) && is_document(entry.kind))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// Kinds that are a view of their own, not a facet of a Terminal.
const fn is_document(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::Document | ResourceKind::PixelSurface | ResourceKind::AccessibilityTree
    )
}

/// Classifies leaves: the graph's word first, the kernel's as fallback.
#[derive(Clone, Copy)]
pub(super) struct SurfaceLeaves<'a> {
    kernel: &'a AttachKernel,
    book: &'a LeafBook,
}

impl<'a> SurfaceLeaves<'a> {
    pub(super) const fn new(kernel: &'a AttachKernel, book: &'a LeafBook) -> Self {
        Self { kernel, book }
    }

    /// The kind of `id` when it is known not to be a Terminal.
    pub(super) fn kind(self, id: &ResourceId) -> Option<ResourceKind> {
        if let Some(entry) = self.book.entries.get(id) {
            return Some(entry.kind);
        }
        self.kernel
            .resource_kind(id)
            .filter(|kind| *kind != ResourceKind::Terminal)
    }

    /// Whether `id` is a known non-Terminal resource: no grid, input, or
    /// resize.
    pub(super) fn contains(self, id: &ResourceId) -> bool {
        self.kind(id).is_some()
    }

    fn title(self, id: &ResourceId) -> Option<String> {
        self.book.entries.get(id)?.title.clone()
    }
}

/// What the placeholder calls a kind.
const fn kind_label(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Document => "Document",
        ResourceKind::PixelSurface => "Pixel surface",
        ResourceKind::AccessibilityTree => "Accessibility tree",
        ResourceKind::AgentSession => "Agent session",
        _ => "Resource",
    }
}

/// A producer-given name (`--app Safari`, a document's `--name`) stripped
/// of control characters: it is untrusted and reaches the TTY.
fn clean_title(title: &str) -> Option<String> {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_owned())
}

/// The centered lines a placeholder shows: the kind, the name when known,
/// and the hint.
pub(super) fn placeholder_lines(kind: ResourceKind, title: Option<String>) -> Vec<String> {
    let mut lines = vec![kind_label(kind).to_owned()];
    lines.extend(title);
    lines.push(PLACEHOLDER_HINT.to_owned());
    lines
}

/// Paint a placeholder for every non-Terminal leaf in `rects`. Each one
/// blanks its whole rect first, so an in-place repaint leaves no stale cell.
pub(super) fn paint_placeholders<W: Write>(
    out: &mut W,
    rects: &HashMap<ResourceId, Rect>,
    leaves: SurfaceLeaves<'_>,
) {
    for (id, rect) in rects {
        let Some(kind) = leaves.kind(id) else {
            continue;
        };
        let lines = placeholder_lines(kind, leaves.title(id));
        paint_placeholder(out, *rect, &lines);
    }
}

fn paint_placeholder<W: Write>(out: &mut W, rect: Rect, lines: &[String]) {
    let _ = out.write_all(b"\x1b[0m");
    let blank = " ".repeat(usize::from(rect.w));
    for row in rect.y..rect.y.saturating_add(rect.h) {
        let _ = super::render::write_cup(out, row, rect.x);
        let _ = out.write_all(blank.as_bytes());
    }
    super::paint::write_centered_lines(out, rect, lines);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_shown_without_its_control_characters() {
        let id = ResourceId::local(4);
        let info = ResourceInfo::resource(id.clone(), ResourceKind::PixelSurface)
            .with_title(Some("Saf\x1b[2Jari\u{7}".to_owned()));
        let mut book = LeafBook::default();
        book.learn(&[info]);
        let (kernel, ..) = crate::attach::pane_state::published_test_state(&[]);
        let leaves = SurfaceLeaves::new(&kernel, &book);
        let lines = placeholder_lines(ResourceKind::PixelSurface, leaves.title(&id));
        assert_eq!(lines, ["Pixel surface", "Saf[2Jari", PLACEHOLDER_HINT]);
        let untitled =
            placeholder_lines(ResourceKind::Document, leaves.title(&ResourceId::local(5)));
        assert_eq!(untitled, ["Document", PLACEHOLDER_HINT]);
    }

    /// The graph's word outlives a stale inventory and yields to a
    /// Terminal; a spawn is news once.
    #[test]
    fn the_book_learns_from_the_graph_and_spawns() {
        let (doc, pane) = (ResourceId::local(5), ResourceId::local(6));
        let (kernel, ..) = crate::attach::pane_state::published_test_state(&[]);
        let mut book = LeafBook::default();
        assert!(book.note_spawned(&doc, ResourceKind::Document));
        assert!(!book.note_spawned(&doc, ResourceKind::Document));
        assert!(!book.note_spawned(&pane, ResourceKind::Terminal));
        book.learn(&[
            ResourceInfo::resource(pane.clone(), ResourceKind::AccessibilityTree)
                .with_session(Some(SessionId::new(1))),
        ]);
        let leaves = SurfaceLeaves::new(&kernel, &book);
        assert!(leaves.contains(&doc), "a later inventory keeps the spawn");
        assert_eq!(book.documents_in(SessionId::new(1)), vec![pane.clone()]);
        book.learn(&[ResourceInfo::new(
            pane.clone(),
            phux_protocol::ids::WindowId::new(1),
            80,
            24,
        )]);
        assert!(!SurfaceLeaves::new(&kernel, &book).contains(&pane));
        book.replace(&[]);
        assert!(!SurfaceLeaves::new(&kernel, &book).contains(&doc));
    }

    #[test]
    fn a_placeholder_blanks_its_whole_rect_and_centers_its_label() {
        let mut out = Vec::new();
        let rect = Rect {
            x: 10,
            y: 2,
            w: 20,
            h: 5,
        };
        paint_placeholder(&mut out, rect, &["Document".to_owned()]);
        let text = String::from_utf8(out).expect("utf8");
        // One blank run per row, then the label on the middle row.
        assert_eq!(text.matches(&" ".repeat(20)).count(), 5);
        assert!(text.contains("\x1b[3;11H"), "first row: {text:?}");
        assert!(text.contains("\x1b[7;11H"), "last row: {text:?}");
        assert!(text.ends_with("\x1b[5;17HDocument"), "{text:?}");
    }
}
