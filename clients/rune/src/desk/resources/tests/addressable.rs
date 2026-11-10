//! A session's documents are addressable like its terminals: a sidebar
//! row, a palette row, a stop when cycling, and the window title.

use super::*;

#[test]
fn a_local_document_has_a_sidebar_row_a_palette_row_a_cycle_stop_and_a_title() {
    let records = [
        record(1, ResourceKind::Terminal),
        record(2, ResourceKind::AccessibilityTree),
    ];
    let (_client, mut rt) = fixture(&records, &doc(LayoutNode::Leaf(id(1))));
    assert_eq!(rt.state.shown.as_ref().unwrap().resource, id(1));
    assert_eq!(rt.state.window_title(), "Mirror 1 — work");

    assert!(rt.find("pane-@1").is_some(), "its terminal");
    assert!(rt.find("pane-@2").is_some(), "its mirror");
    let texts: Vec<_> = rt.elements().into_iter().filter_map(|el| el.text).collect();
    assert!(texts.iter().any(|t| t == "Mirror 2"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Read-only mirror"), "{texts:?}");

    let rows: Vec<_> = rt
        .state
        .palette_hits()
        .into_iter()
        .filter(|h| matches!(h.item.kind, "terminal" | "document"))
        .map(|h| (h.item.kind, h.item.title, h.item.detail))
        .collect();
    let row = |kind, title: &str, detail: &str| (kind, title.to_owned(), detail.to_owned());
    assert_eq!(
        rows,
        [
            row("terminal", "Mirror 1", "work · @1"),
            row("document", "Mirror 2", "work · @2 · Read-only mirror"),
        ]
    );

    rt.update(|d, _| d.cycle(1));
    step(&mut rt);
    let shown = rt.state.shown.as_ref().unwrap();
    assert_eq!((&shown.resource, shown.view), (&id(2), None));
    assert_eq!(rt.state.window_title(), "Mirror 2 — work");
    rt.update(|d, _| d.cycle(1));
    step(&mut rt);
    assert_eq!(rt.state.shown.as_ref().unwrap().resource, id(1));
    rt.update(|d, _| d.cycle(-1));
    step(&mut rt);
    assert_eq!(rt.state.shown.as_ref().unwrap().resource, id(2));
}
