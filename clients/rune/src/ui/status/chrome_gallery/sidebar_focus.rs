//! The actual keyboard route from a split's non-leading pane to Inbox.
use super::*;

mod tests;

fn focus_sidebar(rt: &mut Runtime<Desk>) {
    let event = rune::KeyEvent {
        key: rune::Key::Command('b'),
        mods: rune::Mods {
            cmd: true,
            logo: cfg!(any(target_os = "macos", target_os = "ios")),
            ctrl: !cfg!(any(target_os = "macos", target_os = "ios")),
            alt: !cfg!(any(target_os = "macos", target_os = "ios")),
            shift: true,
        },
    };
    assert_eq!(
        crate::keys::shortcut(&event.key, event.mods, None),
        Some(crate::keys::Shortcut::FocusSidebar)
    );
    rt.key(event);
    rt.frame(3.0);
}

fn previous(rt: &mut Runtime<Desk>) {
    rt.key(rune::KeyEvent {
        key: rune::Key::Tab,
        mods: rune::Mods {
            shift: true,
            ..rune::Mods::default()
        },
    });
    rt.frame(3.0);
}

fn split_window(size: (u16, u16), dark: bool, mode: &str) -> Runtime<Desk> {
    let mut rt = window(dark);
    rt.on_key = Some(Desk::key);
    rt.poll = Some(|d, _, tx| d.return_keyboard(tx));
    rt.set_system_reduced_motion(true);
    rt.set_viewport((f32::from(size.0), f32::from(size.1)), scale());
    rt.update(|d, _| {
        d.pending = None;
        // The attached terminal is @1, but the containing row is led by @2.
        let mut tabs = d.connection.tabs.list.clone();
        tabs[0].resources = [2, 1].map(ResourceId::local).to_vec();
        d.connection.tabs.list.clone_from(&tabs);
        d.connection.tabs.by_session.insert(1, tabs);
        d.connection.layout = Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.55,
            left: Box::new(LayoutNode::Leaf(ResourceId::local(1))),
            right: Box::new(LayoutNode::Leaf(ResourceId::local(2))),
        });
    });
    if mode == "collapsed" {
        rt.update(|d, _| {
            d.sidebar_collapsed
                .insert(SessionKey::new([1; 16]).unwrap());
        });
    }
    if mode == "forced" {
        rt.set_system_contrast(Some(crate::test_support::visual::forced_colors(dark)));
    }
    server::settle(&mut rt);
    rt
}

fn capture(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u16, u16),
    name: &str,
) {
    rt.set_srgb_blend(true);
    server::settle(rt);
    let pixels = crate::ui::offscreen_as_app_with_renderer(
        gpu,
        renderer,
        rt,
        (u32::from(size.0) * 2, u32::from(size.1) * 2),
    );
    assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]));
    if let Some(dir) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
        crate::test_support::visual::write(
            &std::path::Path::new(&dir).join(format!("{name}.png")),
            rt,
            (u32::from(size.0) * 2, u32::from(size.1) * 2),
            &pixels,
            name,
        );
    }
}

#[test]
fn sidebar_focus_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(1100, 720), (640, 360)] {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for mode in ["normal", "forced", "collapsed"] {
                let mut rt = split_window(size, dark, mode);
                // Each runtime's glyph uploads belong to its retained renderer.
                let format = rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix();
                let mut renderer = rune::render::Renderer::new(&gpu.device, format);
                renderer.set_srgb_blend(true);
                let prefix = format!("sidebar-focus-{theme}-{mode}");
                let suffix = format!("{}x{}-2x", size.0, size.1);
                capture(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    size,
                    &format!("{prefix}-idle-{suffix}"),
                );
                focus_sidebar(&mut rt);
                capture(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    size,
                    &format!("{prefix}-row-{suffix}"),
                );
                previous(&mut rt);
                capture(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    size,
                    &format!("{prefix}-inbox-focus-{suffix}"),
                );
                rt.key(crate::keys::plain(rune::Key::Text(" ".into())));
                rt.frame(3.0);
                capture(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    size,
                    &format!("{prefix}-inbox-open-{suffix}"),
                );
            }
        }
    }
}
