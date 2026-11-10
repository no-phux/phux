//! Layout leaves that are not Terminals: a document, accessibility tree,
//! pixel surface, or agent session another client placed in the arrangement
//! (docs/resource-kinds.md, "any resource with a surface can be a layout
//! leaf"). The TUI has no renderer for them, so it tiles a labelled
//! placeholder in the leaf's rect and never sends the leaf input or a resize.
//!
//! A leaf counts as one only once the session kernel knows its kind (from
//! its bootstrap); a leaf of unknown kind keeps the Terminal path.

use std::collections::HashMap;
use std::io::Write;

use phux_protocol::ResourceKind;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::ResourceInfo;

use super::pane_state::AttachKernel;
use crate::layout::Rect;

/// The placeholder's last line: why the tile is empty.
pub(super) const PLACEHOLDER_HINT: &str = "Not shown in the terminal";

/// The kind of `id` when the kernel knows it is not a Terminal.
pub(super) fn surface_kind(kernel: &AttachKernel, id: &ResourceId) -> Option<ResourceKind> {
    kernel
        .resource_kind(id)
        .filter(|kind| *kind != ResourceKind::Terminal)
}

/// Whether `id` is a known non-Terminal resource: no grid, input, or resize.
pub(super) fn is_surface_leaf(kernel: &AttachKernel, id: &ResourceId) -> bool {
    surface_kind(kernel, id).is_some()
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

/// The name a producer gave `id` (`--app Safari`, a document's `--name`),
/// stripped of control characters: it is untrusted and reaches the TTY.
fn resource_title(resources: &[ResourceInfo], id: &ResourceId) -> Option<String> {
    let title = resources
        .iter()
        .find(|info| &info.id == id)?
        .title
        .as_deref()?;
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
    kernel: &AttachKernel,
    resources: &[ResourceInfo],
) {
    for (id, rect) in rects {
        let Some(kind) = surface_kind(kernel, id) else {
            continue;
        };
        let lines = placeholder_lines(kind, resource_title(resources, id));
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

/// Teach `kernel` that `id` is a document of `profile`'s kind, as its
/// `BOOTSTRAP_BEGIN` would.
#[cfg(test)]
pub(super) fn declare_document(
    kernel: &mut AttachKernel,
    id: &ResourceId,
    profile: phux_protocol::BootstrapStreamProfile,
) {
    let mut effects = phux_client_core::session::EffectBuffer::new();
    kernel
        .update(
            phux_client_core::session::KernelInput::BootstrapBegin {
                terminal_id: id,
                stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                profile,
                geometry: phux_client_core::engine::CanonicalGeometry { cols: 0, rows: 0 },
                base_seq: 1,
            },
            &mut effects,
        )
        .expect("document BOOTSTRAP_BEGIN");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_shown_without_its_control_characters() {
        let id = ResourceId::local(4);
        let info = ResourceInfo::resource(id.clone(), ResourceKind::PixelSurface)
            .with_title(Some("Saf\x1b[2Jari\u{7}".to_owned()));
        let lines = placeholder_lines(
            ResourceKind::PixelSurface,
            resource_title(std::slice::from_ref(&info), &id),
        );
        assert_eq!(lines, ["Pixel surface", "Saf[2Jari", PLACEHOLDER_HINT]);
        let untitled = placeholder_lines(ResourceKind::Document, resource_title(&[], &id));
        assert_eq!(untitled, ["Document", PLACEHOLDER_HINT]);
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
