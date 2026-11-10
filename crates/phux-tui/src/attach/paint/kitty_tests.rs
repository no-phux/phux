//! A pane that leaves the view takes its kitty images off the outer terminal.

use super::*;
use crate::attach::pane_state::published_test_state;
use crate::layout::LayoutNode;
use crate::render::theme::Theme;

const RED: &[u8] = b"\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2;/wAA/w==\x1b\\";
const GREEN: &[u8] = b"\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2;AP8A/w==\x1b\\";
const VIEWPORT: (u16, u16) = (20, 5);

fn leaf(id: &ResourceId) -> LayoutState {
    LayoutState {
        tree: Some(LayoutNode::Leaf(id.clone())),
        focus: Some(id.clone()),
    }
}

/// Repaint the panes of `layout` in place (no `ED2`) and return the bytes.
fn paint_in_place(
    layout: &LayoutState,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    kernel: &AttachKernel,
) -> Vec<u8> {
    let theme = Theme::default();
    let mut chrome = ChromeCtx {
        viewport: VIEWPORT,
        sidebar: None,
        status_bar: None,
        sidebar_painter: None,
        session_name: "demo",
        theme: &theme,
        leaves: &crate::attach::surface_leaf::LeafBook::default(),
    };
    let mut out = Vec::new();
    paint_panes_in_place(
        &mut out,
        layout,
        panes,
        kernel,
        layout.focus.as_ref(),
        &mut chrome,
    );
    out
}

/// The outer terminal the frames land on.
fn outer_terminal() -> PaneSlot {
    PaneSlot::new_with_size(VIEWPORT.0, VIEWPORT.1).expect("outer terminal")
}

fn outer_images(outer: &PaneSlot) -> Vec<u32> {
    let mut ids = Vec::new();
    outer
        .terminal
        .kitty_graphics()
        .unwrap()
        .image_ids_into(&mut ids)
        .unwrap();
    ids.sort_unstable();
    ids
}

fn outer_id(panes: &HashMap<ResourceId, PaneSlot>, pane: &ResourceId) -> u32 {
    panes[pane].renderer.kitty_outer_id(77).expect("mapped id")
}

fn deletes(bytes: &[u8], id: u32) -> bool {
    let needle = format!("\x1b_Ga=d,d=I,i={id},");
    String::from_utf8_lossy(bytes).contains(&needle)
}

/// A window switch onto an identically shaped tile repaints in place, never
/// painting the old window's pane again: its image must not stay on screen.
#[test]
fn a_window_switch_in_place_deletes_the_hidden_panes_images() {
    let (a, b) = (ResourceId::local(1), ResourceId::local(2));
    let (kernel, _, mut panes) = published_test_state(&[
        (&a, VIEWPORT.0, VIEWPORT.1, RED),
        (&b, VIEWPORT.0, VIEWPORT.1, GREEN),
    ]);
    let mut outer = outer_terminal();
    outer
        .terminal
        .vt_write(&paint_in_place(&leaf(&a), &mut panes, &kernel));
    let id_a = outer_id(&panes, &a);
    assert_eq!(outer_images(&outer), vec![id_a]);

    let switched = paint_in_place(&leaf(&b), &mut panes, &kernel);
    assert!(deletes(&switched, id_a), "{switched:?}");
    outer.terminal.vt_write(&switched);
    assert_eq!(outer_images(&outer), vec![outer_id(&panes, &b)]);
    // Released once: the next repaint of the same view deletes nothing.
    assert!(!deletes(
        &paint_in_place(&leaf(&b), &mut panes, &kernel),
        id_a
    ));

    // Switching back shows A's image again.
    outer
        .terminal
        .vt_write(&paint_in_place(&leaf(&a), &mut panes, &kernel));
    assert_eq!(outer_images(&outer), vec![id_a]);
}

/// A closed pane's renderer is dropped with its images on screen; the next
/// frame, painted without an `ED2`, deletes them.
#[test]
fn a_closed_panes_images_are_deleted_by_the_next_frame() {
    let (a, b) = (ResourceId::local(1), ResourceId::local(2));
    let (kernel, _, mut panes) = published_test_state(&[
        (&a, VIEWPORT.0, VIEWPORT.1, RED),
        (&b, VIEWPORT.0, VIEWPORT.1, GREEN),
    ]);
    let mut outer = outer_terminal();
    outer
        .terminal
        .vt_write(&paint_in_place(&leaf(&b), &mut panes, &kernel));
    let id_b = outer_id(&panes, &b);
    assert_eq!(outer_images(&outer), vec![id_b]);

    panes.remove(&b);
    let next = paint_in_place(&leaf(&a), &mut panes, &kernel);
    assert!(deletes(&next, id_b), "{next:?}");
    outer.terminal.vt_write(&next);
    assert_eq!(outer_images(&outer), vec![outer_id(&panes, &a)]);
    assert!(!deletes(
        &paint_in_place(&leaf(&a), &mut panes, &kernel),
        id_b
    ));
}
