//! Discover a waiting agent and open Inbox in the actual populated window.
use super::*;

fn focus_inbox(rt: &mut Runtime<Desk>) {
    // An unfocused terminal owns Tab. Use the app's Focus Sidebar chord first.
    let mods = rune::Mods {
        cmd: true,
        logo: cfg!(target_os = "macos"),
        ctrl: !cfg!(target_os = "macos"),
        alt: !cfg!(target_os = "macos"),
        shift: cfg!(target_os = "macos"),
    };
    let key = rune::Key::Command('b');
    assert_eq!(
        crate::keys::shortcut(&key, mods, None),
        Some(crate::keys::Shortcut::FocusSidebar)
    );
    rt.key(rune::KeyEvent { key, mods });
    rt.frame(3.0);
    assert!(crate::ui::element(rt, &format!("pane-{}", ResourceId::local(1))).focused);
    for _ in 0..32 {
        rt.key(rune::KeyEvent {
            key: rune::Key::Tab,
            mods: rune::Mods {
                shift: true,
                ..rune::Mods::default()
            },
        });
        rt.frame(3.0);
        if crate::ui::element(rt, "inbox-button").focused {
            return;
        }
    }
    panic!("Shift+Tab never reached the actual Inbox button");
}

fn attention_window(size: (u16, u16), dark: bool, forced: bool, state: &str) -> Runtime<Desk> {
    let mut rt = window(dark);
    rt.on_key = Some(Desk::key);
    rt.set_viewport((f32::from(size.0), f32::from(size.1)), scale());
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| d.pending = None);
    if forced {
        rt.set_system_contrast(Some(crate::test_support::visual::forced_colors(dark)));
    }
    server::settle(&mut rt);
    assert!(
        crate::ui::element(&rt, "inbox-button")
            .label
            .unwrap()
            .contains("waiting")
    );
    assert!(rt.find("tab-0").is_some(), "the selected build tab");
    if state != "closed" {
        focus_inbox(&mut rt);
        assert!(crate::ui::element(&rt, "inbox-button").focus_visible);
    }
    if state == "inbox" {
        // Native buttons activate with Space; Return belongs to the default action.
        rt.key(crate::keys::plain(rune::Key::Text(" ".into())));
        rt.frame(3.0);
        assert!(rt.state.inbox.open, "Space opens the actual Inbox");
    }
    rt
}

#[test]
fn attention_contrast_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(1100, 720), (640, 360)] {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for (forced, mode) in [(false, "normal"), (true, "forced")] {
                for state in ["closed", "focused", "inbox"] {
                    let mut rt = attention_window(size, dark, forced, state);
                    let name = format!("attention-{theme}-{mode}-{state}-{}x{}-2x", size.0, size.1);
                    let pixels = server::paint(
                        &gpu,
                        &mut rt,
                        (f32::from(size.0), f32::from(size.1)),
                        scale(),
                        "RUNE_NAVIGATION_SNAP",
                        &name,
                    );
                    if let Some(dir) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
                        crate::test_support::visual::write(
                            &std::path::Path::new(&dir).join(format!("{name}.png")),
                            &rt,
                            (u32::from(size.0) * 2, u32::from(size.1) * 2),
                            &pixels,
                            &name,
                        );
                    }
                }
            }
        }
    }
}
