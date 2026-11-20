//! Matched reading and interaction cases for the Connections destination.
use super::*;

#[test]
fn phone_connections_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, native, scale) in [
        ((393, 852), false, 1),
        ((320, 568), false, 1),
        ((393, 759), true, 2),
        ((320, 478), true, 2),
    ] {
        for (appearance, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))] {
            for (case, machine, largest) in [
                ("ordinary", "Build studio", false),
                ("long", "build-studio-integration-runner", false),
                ("large", "開発コンピュータ", true),
            ] {
                let (_, mut rt) = fixture();
                detailed_inventory(&mut rt);
                rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
                rt.set_system_reduced_motion(true);
                rt.update(|d, _| {
                    if native {
                        d.native = Some(crate::mobile::Platform::test());
                    }
                    d.machines.current = Some(machine.into());
                    d.settings.theme = theme;
                    d.set_accessibility(
                        false,
                        if largest {
                            "UICTContentSizeCategoryAccessibilityXXXL"
                        } else {
                            "UICTContentSizeCategoryL"
                        },
                    );
                    d.open_phone_sessions();
                });
                settle_for_snapshot(&mut rt);
                let mut renderer = app_renderer(&gpu);
                let name = format!("connections-{case}-{appearance}-{scale}x-native-{native}");
                paint(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    (size.0 * scale, size.1 * scale),
                    &name,
                );
                // Keep the exact same fixture on the baseline: this captures
                // reading composition, not an implementation-specific golden.
                if !largest {
                    rt.update(|_, tx| {
                        tx.focus("phone-session-connections", true);
                        tx.instant();
                    });
                    settle_for_snapshot(&mut rt);
                    paint(
                        &gpu,
                        &mut renderer,
                        &mut rt,
                        (size.0 * scale, size.1 * scale),
                        &format!("{name}-focused"),
                    );
                }
            }
        }
    }
}
