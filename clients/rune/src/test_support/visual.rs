//! Saved, real-renderer evidence. Keep ordered elements; keyed maps lose rows.

use std::path::Path;

use crate::desk::Desk;

/// A deterministic Windows-style contrast theme, without changing host settings.
pub fn forced_colors(dark: bool) -> rune::SystemContrast {
    let c = |rgb: u32| crate::theme::color(rgb);
    let (window, text) = if dark {
        (0x00_0000, 0xff_ffff)
    } else {
        (0xff_ffff, 0x00_0000)
    };
    rune::SystemContrast {
        window: c(window),
        text: c(text),
        highlight: c(if dark { 0x1a_ebff } else { 0x37_006e }),
        highlight_text: c(window),
        disabled: c(if dark { 0x3f_f23f } else { 0x60_0000 }),
        button: c(window),
        button_text: c(text),
        link: c(if dark { 0xff_ff00 } else { 0x00_009f }),
    }
}

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
