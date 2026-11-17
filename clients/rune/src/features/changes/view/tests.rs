//! The changes sheet painted over a real window: file rows on the tokens,
//! the diff on the `diff.*` roles. `RUNE_GALLERY_SNAP=DIR` writes
//! `DIR/overlay-changes-LOOK.png`.

use phux_protocol::checkout::{CheckoutArea, CheckoutDiff, CheckoutEntry, CheckoutStatus};
use rune::{Env, Runtime};

use super::super::presentation;
use crate::desk::Desk;

const SIZE: (f32, f32) = (1280.0, 800.0);
use crate::test_support::SCALE;

fn entry(path: &str, code: &str, area: CheckoutArea) -> CheckoutEntry {
    CheckoutEntry {
        path: path.to_owned(),
        from: None,
        area,
        code: code.into(),
        added: Some(12),
        deleted: Some(3),
        binary: false,
    }
}

const PATCH: &str = "diff --git a/src/ui/overlay.rs b/src/ui/overlay.rs\n\
@@ -1,6 +1,7 @@\n \
//! What floats over the terminals\n\
-use rune::{Color, El};\n\
+use rune::{Edge, El};\n\
+use rune_kit::ToastRegion;\n \
\n \
pub fn toast() {}\n";

fn sheet(dark: bool) -> Runtime<Desk> {
    let mut rt = Runtime::new(
        crate::test_support::window(),
        crate::ui::view,
        SIZE,
        Env::new(dark, SCALE),
    );
    rt.frame(0.0);
    rt.update(|d, _| {
        let changes = &mut d.connection.changes;
        changes.open = true;
        changes.listing = Some(Ok(presentation::listing(&CheckoutStatus {
            branch: Some("design/c".into()),
            entries: vec![
                entry(
                    "clients/rune/src/ui/overlay.rs",
                    "M",
                    CheckoutArea::Unstaged,
                ),
                entry(
                    "clients/rune/src/ui/overlay/tests.rs",
                    "A",
                    CheckoutArea::Staged,
                ),
                entry("clients/rune/src/ui/old.rs", "D", CheckoutArea::Unstaged),
            ],
            ..CheckoutStatus::default()
        })));
        changes.diff = Some(Ok(presentation::diff(
            &CheckoutDiff {
                path: "clients/rune/src/ui/overlay.rs".into(),
                area: CheckoutArea::Unstaged,
                text: PATCH.into(),
                truncated: false,
                binary: false,
            },
            400,
        )));
    });
    for t in [0.0, 0.5, 1.0, 2.0] {
        rt.frame(t);
    }
    rt
}

#[test]
fn the_changes_sheet_paints_its_rows_and_diff_on_tokens() {
    let gpu = rune::render::Gpu::headless();
    for (tag, dark) in [("dark", true), ("light", false)] {
        let mut rt = sheet(dark);
        for key in [
            "changes",
            "changes-row-0",
            "changes-row-2",
            "changes-refresh",
        ] {
            assert!(rt.find(key).is_some(), "{key}");
        }
        let row = crate::ui::element(&rt, "changes-row-1");
        assert!(
            row.label
                .as_deref()
                .is_some_and(|l| l.contains("overlay/tests.rs")),
            "a row says its path, state and counts: {:?}",
            row.label
        );
        assert_eq!(crate::ui::element(&rt, "changes-refresh").role, "button");
        rt.set_srgb_blend(true);
        let (w, h) = ((SIZE.0 * SCALE) as u32, (SIZE.1 * SCALE) as u32);
        let pixels = crate::ui::offscreen_as_app(&gpu, &mut rt, (w, h));
        assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]));
        if let Some(dir) = std::env::var_os("RUNE_GALLERY_SNAP") {
            let dir = std::path::Path::new(&dir);
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join(format!("overlay-changes-{tag}.png"));
            rune::render::write_png(&path, w, h, &pixels).unwrap();
        }
    }
}
