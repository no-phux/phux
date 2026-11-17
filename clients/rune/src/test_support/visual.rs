//! Saved, real-renderer evidence. Keep ordered elements; keyed maps lose rows.

use std::path::Path;

use crate::desk::Desk;

/// Export pixels and an explicitly scoped geometry/focus/accessibility projection.
pub fn write(
    path: &Path,
    rt: &rune::Runtime<Desk>,
    size: (u32, u32),
    pixels: &[u8],
    scenario: &str,
) {
    rune::render::write_png(path, size.0, size.1, pixels).unwrap();
    let elements: Vec<_> = rt
        .elements()
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "path": e.path, "key": e.key, "parent": e.parent,
                "rect": e.rect, "visible": e.visible, "target": e.target,
                "role": e.role, "label": e.label, "text": e.text, "help": e.help,
                "focused": e.focused, "focusable": e.focusable,
                "focusVisible": e.focus_visible, "inert": e.inert,
                "caret": e.caret, "editRange": e.edit_range,
                "opacity": e.opacity, "scale": e.scale, "exiting": e.exiting,
            })
        })
        .collect();
    let scale = rt.env.scale;
    let tree = serde_json::json!({
        "schema": 1,
        "projection": "ordered geometry, focus, text and accessibility; not Rune's private trace format",
        "viewport": [rt.viewport.0, rt.viewport.1],
        "scenario": scenario,
        "theme": if rt.state.settings.theme.map_or(rt.env.system_dark, |theme| theme.dark) { "dark" } else { "light" },
        "scale": scale, "pixels": [size.0, size.1],
        "elements": elements, "accessibility": rt.accessibility_json(),
    });
    std::fs::write(
        path.with_extension("json"),
        serde_json::to_vec_pretty(&tree).unwrap(),
    )
    .unwrap();
}

#[test]
fn projection_observes_the_runtime_not_the_requested_image_size() {
    let (_, mut rt) = crate::test_support::input_fixture();
    rt.set_viewport((393.0, 420.0), 2.0);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("deliberately-wrong-target.png");
    write(&path, &rt, (1, 1), &[0, 0, 0, 255], "mismatched-target");
    let tree: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    assert_eq!(tree["viewport"], serde_json::json!([393.0, 420.0]));
    assert_eq!(tree["pixels"], serde_json::json!([1, 1]));
    assert_eq!(tree["scale"], 2.0);
    assert_eq!(tree["scenario"], "mismatched-target");
}
