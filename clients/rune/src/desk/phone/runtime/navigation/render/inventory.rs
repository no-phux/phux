//! The Sessions and Terminal actions pages as the iOS host draws them: the
//! native safe area, Liquid Glass controls, and the text sizes people run.
use super::*;

/// UIKit has already removed the status bar, home indicator and keyboard,
/// so these are the content areas of a 402 and a 320 point phone.
const NATIVE_SIZES: [((u32, u32), u32); 2] = [((402, 781), 3), ((320, 478), 2)];

/// The default size and XXL, the largest size before the grid's cap: the
/// geometry a device with Dynamic Type turned up actually lays out.
const TEXT_SIZES: [(&str, &str); 2] = [
    ("L", "UICTContentSizeCategoryL"),
    ("XXL", "UICTContentSizeCategoryXXL"),
];

#[test]
fn phone_native_inventory_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, scale) in NATIVE_SIZES {
        for (appearance, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))] {
            for (text, category) in TEXT_SIZES {
                let (_, mut rt) = fixture();
                detailed_inventory(&mut rt);
                rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
                rt.set_system_reduced_motion(true);
                rt.update(|d, _| {
                    d.native = Some(crate::mobile::Platform::test());
                    d.settings.theme = theme;
                    d.set_accessibility(false, category);
                    d.open_phone_sessions();
                });
                settle_for_snapshot(&mut rt);
                let mut renderer = app_renderer(&gpu);
                let pixels = (size.0 * scale, size.1 * scale);
                let name = format!("native-{text}-{appearance}");
                paint(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    pixels,
                    &format!("{name}-sessions"),
                );
                rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
                settle_for_snapshot(&mut rt);
                paint(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    pixels,
                    &format!("{name}-sessions-bottom"),
                );
                rt.update(|d, tx| {
                    d.close_phone_navigation();
                    d.open_phone_actions();
                    tx.instant();
                });
                settle_for_snapshot(&mut rt);
                paint(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    pixels,
                    &format!("{name}-actions"),
                );
            }
        }
    }
}
