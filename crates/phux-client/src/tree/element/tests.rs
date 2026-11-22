use serde_json::json;

use super::*;

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

fn macos() -> Value {
    json!({"role": "AXApplication", "id": "0", "title": "TextEdit", "children": [
        {"role": "AXWindow", "id": "0.0", "title": "Untitled", "frame": [100, 50, 800, 600], "children": [
            {"role": "AXButton", "id": "0.0.0", "description": "Save", "frame": [120, 70, 60, 20]},
            {"role": "AXButton", "id": "0.0.2", "title": "Save As…", "frame": [200, 70, 60, 20]},
            {"role": "AXTextArea", "id": "0.0.3", "value": "hi"}
        ]}
    ]})
}

#[test]
fn a_nested_tree_flattens_with_names_and_the_window_bounds() {
    let all = elements(&macos());
    assert_eq!(all.len(), 5);
    assert_eq!(all[2].name, "Save");
    assert_eq!(all[2].window, Some(rect(100.0, 50.0, 800.0, 600.0)));
    assert_eq!(all[0].window, None);
    // A tree from a producer that predates ids names nothing.
    assert!(
        elements(&json!({"role": "AXApplication", "children": [{"role": "AXButton"}]})).is_empty()
    );
}

#[test]
fn a_page_tree_is_named_by_node_id_and_ignored_nodes_drop() {
    let page = json!({"nodes": [
        {"nodeId": "1", "role": {"value": "RootWebArea"}, "name": {"value": "Shop"}},
        {"nodeId": "7", "ignored": true, "role": {"value": "generic"}},
        {"nodeId": "9", "role": {"value": "button"}, "name": {"value": "Buy"}, "backendDOMNodeId": 42}
    ]});
    let all = elements(&page);
    assert_eq!(
        all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["1", "9"]
    );
    assert_eq!(
        find(
            &all,
            Selector::Name {
                name: "buy",
                role: None
            }
        )
        .unwrap()
        .id,
        "9"
    );
}

#[test]
fn an_exact_name_beats_a_substring_and_ties_list_the_candidates() {
    let all = elements(&macos());
    let save = |role| find(&all, Selector::Name { name: "save", role });
    assert_eq!(save(None).unwrap().id, "0.0.0");
    let Err(FindError::Ambiguous(many)) = find(
        &all,
        Selector::Name {
            name: "sav",
            role: None,
        },
    ) else {
        panic!("two substrings match");
    };
    assert_eq!(many.len(), 2);
    let message = ClickError::Find(FindError::Ambiguous(many)).to_string();
    assert!(
        message.contains("[0.0.2] AXButton \"Save As…\""),
        "{message}"
    );
    assert_eq!(
        find(
            &all,
            Selector::Name {
                name: "untitled",
                role: Some("axwindow")
            }
        )
        .unwrap()
        .id,
        "0.0"
    );
    assert_eq!(
        save(Some("AXTextArea")),
        Err(FindError::NoMatch { known: true })
    );
    assert_eq!(
        find(&all, Selector::Id("0.0.3")).unwrap().role,
        "AXTextArea"
    );
    assert_eq!(
        find(&[], Selector::Id("0")),
        Err(FindError::NoMatch { known: false })
    );
}

#[test]
fn bounds_map_to_frame_pixels_through_origin_scale_and_downscale() {
    let window = rect(100.0, 50.0, 800.0, 600.0);
    let button = rect(120.0, 70.0, 60.0, 20.0); // centre 150,80 → 50,30 in the window
    // A Retina capture: two pixels per point.
    assert_eq!(frame_pixel(button, window, (1600, 1200)), Some((100, 60)));
    // `--max-width 400`: half a pixel per point.
    assert_eq!(frame_pixel(button, window, (400, 300)), Some((25, 15)));
    // A window on a display left of the main one has a negative origin.
    let left = rect(-1000.0, 0.0, 500.0, 400.0);
    assert_eq!(
        frame_pixel(rect(-990.0, 10.0, 20.0, 20.0), left, (500, 400)),
        Some((20, 20))
    );
    // Outside the window, or nothing to scale by.
    assert_eq!(
        frame_pixel(rect(0.0, 0.0, 10.0, 10.0), window, (800, 600)),
        None
    );
    assert_eq!(frame_pixel(button, window, (0, 0)), None);
    assert_eq!(
        frame_pixel(button, rect(0.0, 0.0, 0.0, 0.0), (800, 600)),
        None
    );
}
