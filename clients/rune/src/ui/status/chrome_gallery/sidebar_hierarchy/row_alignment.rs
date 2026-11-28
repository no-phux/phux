//! Long single-resource titles share width with context, not its close action.
use super::*;

const TITLE: &str = "Review deployment logs and release qualification";
const SESSION: &str = "Release engineering and nightly qualification";

fn long_single(size: (u16, u16), dark: bool, mode: &str) -> Runtime<Desk> {
    let mut rt = window(size, dark, mode);
    rt.update(|d, _| {
        let session = session(1, SESSION, 1);
        let pane = pane(1, &session, TITLE, None);
        let mut topology = crate::desk::topology(vec![session], vec![pane]);
        server::terminal_resources(&mut topology);
        d.connection.topology = Some(topology);
    });
    server::settle(&mut rt);
    rt
}

#[test]
fn single_resource_titles_do_not_reserve_session_close_space() {
    for size in [(1100, 720), (640, 360)] {
        for dark in [true, false] {
            for mode in ["normal", "large", "forced"] {
                let mut rt = long_single(size, dark, mode);
                rt.update(|d, _| {
                    d.connection.topology.as_mut().unwrap().panes[0].cwd =
                        Some("~/workspace/release-engineering/nightly-qualification".to_owned());
                });
                server::settle(&mut rt);
                let title = rt
                    .elements()
                    .into_iter()
                    .find(|e| e.path.contains("/sidebar-list/") && e.text.as_deref() == Some(TITLE))
                    .expect("the real sidebar title");
                let key = SessionKey::new([1; 16]).unwrap();
                let close = crate::ui::element(&rt, &format!("session-close-{key}"));
                let context = crate::ui::element(&rt, "select-session-1");
                let row = crate::ui::element(&rt, "pane-@1");
                // The existing two-line pitch, including interface-zoom rounding.
                assert!((row.rect[3] - 44.0).abs() < 0.01);
                assert!(
                    title.rect[0] + title.rect[2] >= close.rect[0] + close.rect[2] - 0.01,
                    "single-resource title still reserves the session Close column"
                );
                assert!(close.rect[1] >= title.rect[1] + title.rect[3] - 0.01);
                assert_eq!(close.rect[1], context.rect[1]);
                let min = phux_design::tokens::SPACE.pointer_min;
                assert!(close.rect[2] >= min && close.rect[3] >= min);
                assert!((close.visible[2] - close.rect[2]).abs() < 0.01);
                assert!((close.visible[3] - close.rect[3]).abs() < 0.01);
                assert!(!close.focusable, "Close is not another list Tab stop");
                for detail in rt.elements().into_iter().filter(|e| {
                    e.path.starts_with(&format!("{}/", row.path))
                        && e.text.is_some()
                        && e.text.as_deref() != Some(TITLE)
                }) {
                    assert!(
                        detail.rect[0] + detail.rect[2] <= close.rect[0] + 0.01,
                        "context overlaps Close: {detail:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn nested_single_close_cancels_and_keeps_its_accessibility_action() {
    let mut rt = long_single((1100, 720), true, "normal");
    let key = SessionKey::new([1; 16]).unwrap();
    let close = format!("session-close-{key}");
    let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let layout = rt.state.connection.layout.clone();
    let client = rt.state.connection.client.as_ref().unwrap().clone();
    let _ = client.take_outbound();
    let (x, y) = rt.find(&close).unwrap();
    rt.pointer_move(x, y);
    server::settle(&mut rt);
    assert!(rt.element_at(x, y).unwrap().path().contains(&close));
    rt.pointer_down(x, y);
    rt.pointer_move(x + 80.0, y + 80.0);
    rt.frame(3.0);
    assert!(rt.state.connection.reorder.held.is_none());
    rt.pointer_up(x + 80.0, y + 80.0);
    rt.frame(3.0);
    assert!(
        rt.state.pending.is_none(),
        "cancelled Close does not activate"
    );
    assert!(rt.accessibility_action(&close, rune::accesskit::Action::Click, None));
    rt.frame(3.0);
    assert_eq!(
        rt.state.pending,
        Some(crate::desk::Pending::CloseSession(key, SESSION.to_owned()))
    );
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, shown);
    assert_eq!(rt.state.connection.layout, layout);
    assert!(rt.state.connection.tabs.renaming.is_none());
    assert!(rt.state.connection.reorder.held.is_none());
    super::tests::no_terminal_input(&rt);
}

#[test]
fn a_keyless_single_has_no_close_slot_or_session_mutation() {
    let mut rt = long_single((1100, 720), true, "normal");
    rt.update(|d, _| d.connection.topology.as_mut().unwrap().sessions[0].key = None);
    server::settle(&mut rt);
    assert!(!crate::ui::element(&rt, "select-session-1").focusable);
    assert!(rt.elements().iter().all(|e| {
        !e.key
            .as_deref()
            .is_some_and(|k| k.starts_with("session-close-"))
    }));
}

#[test]
fn sidebar_hierarchy_visual_review_frames_with_long_single_rows() {
    let gpu = rune::render::Gpu::headless();
    for size in [(1100, 720), (640, 360)] {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for mode in ["normal", "large", "forced"] {
                let mut rt = long_single(size, dark, mode);
                let format = rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix();
                let mut renderer = rune::render::Renderer::new(&gpu.device, format);
                renderer.set_srgb_blend(true);
                for state in ["rest", "hover", "focused"] {
                    match state {
                        "hover" => {
                            let context = crate::ui::element(&rt, "select-session-1");
                            rt.pointer_move(
                                context.rect[0] + context.rect[2] / 2.0,
                                context.rect[1] + context.rect[3] / 2.0,
                            );
                        }
                        "focused" => {
                            rt.pointer_move(0.0, 0.0);
                            super::super::sidebar_focus::focus_sidebar(&mut rt);
                        }
                        _ => {}
                    }
                    let name = format!(
                        "sidebar-hierarchy-{theme}-{mode}-single-long-{state}-{}x{}-2x",
                        size.0, size.1
                    );
                    super::super::sidebar_focus::capture(&gpu, &mut renderer, &mut rt, size, &name);
                }
            }
        }
    }
}
