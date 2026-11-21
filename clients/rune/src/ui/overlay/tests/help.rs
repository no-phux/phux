//! The real Find toolbar's pointer and keyboard tooltip contracts.
use super::*;
use phux_protocol::wire::frame::FrameKind;

fn toolbar() -> Runtime<Desk> {
    let mut rt = window(true, None, false);
    rt.update(|d, _| d.open_find());
    settle(&mut rt);
    rt.pointer_move(0.0, 0.0);
    rt.frame(1.0 / 60.0);
    rt
}

fn tab_to(rt: &mut Runtime<Desk>, key: &str) {
    for _ in 0..12 {
        if focused(rt).as_deref() == Some(key) {
            return;
        }
        rt.key(crate::keys::plain(Key::Tab));
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(focused(rt).as_deref(), Some(key), "actual Tab traversal");
}

fn no_input(rt: &Runtime<Desk>) {
    for bytes in rt.state.connection.client.as_ref().unwrap().take_outbound() {
        assert!(
            !matches!(
                FrameKind::decode(&bytes).unwrap().0,
                FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
            ),
            "toolbar keys must not type behind Find"
        );
    }
}

#[test]
fn find_keyboard_help_follows_focus_and_dismisses_without_terminal_input() {
    let mut rt = toolbar();
    let _ = rt.state.connection.client.as_ref().unwrap().take_outbound();
    tab_to(&mut rt, "find-older");
    rt.frame(0.15);
    assert_eq!(rt.help_tooltip().map(|tip| tip.0), Some("Older match"));
    tab_to(&mut rt, "find-newer");
    rt.frame(0.15);
    assert_eq!(rt.help_tooltip().map(|tip| tip.0), Some("Newer match"));
    rt.key(crate::keys::plain(Key::Text(" ".into())));
    rt.frame(0.15);
    assert!(rt.help_tooltip().is_none());
    rt.frame(1.0);
    assert!(rt.help_tooltip().is_none(), "no immediate resurrection");
    no_input(&rt);
}

#[test]
fn find_toolbar_escape_closes_find_without_terminal_input() {
    for key in ["find-older", "find-newer", "find-close"] {
        let mut rt = toolbar();
        tab_to(&mut rt, key);
        let _ = rt.state.connection.client.as_ref().unwrap().take_outbound();
        rt.key(crate::keys::plain(Key::Escape));
        rt.frame(0.15);
        no_input(&rt);
        assert!(
            rt.state.connection.find.is_none(),
            "Escape closes the toolbar from {key}"
        );
    }
}

#[test]
fn find_pointer_help_waits_for_the_upstream_six_hundred_milliseconds() {
    let mut rt = toolbar();
    rt.update(|d, tx| {
        d.connection.find_query.focused = false;
        tx.blur();
    });
    rt.frame(0.0);
    let (x, y) = rt.find("find-close").unwrap();
    rt.pointer_move(x, y);
    rt.frame(0.01);
    rt.frame(0.45);
    assert!(rt.help_tooltip().is_none(), "not before 600 ms");
    rt.frame(0.3);
    assert_eq!(rt.help_tooltip().map(|tip| tip.0), Some("Close find"));
}

#[test]
fn find_help_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(1100.0, 720.0), (640.0, 360.0)] {
        for (tag, dark) in [("dark", true), ("light", false)] {
            let mut rt = window(dark, None, false);
            rt.set_viewport(size, 2.0);
            rt.update(|d, _| d.open_find());
            settle(&mut rt);
            rt.pointer_move(0.0, 0.0);
            tab_to(&mut rt, "find-close");
            let pixels = server::paint(
                &gpu,
                &mut rt,
                size,
                2.0,
                "RUNE_HELP_SNAP",
                &format!("help-{tag}-{}", size.0),
            );
            if let Some(dir) = std::env::var_os("RUNE_HELP_SNAP") {
                let path = std::path::PathBuf::from(dir).join(format!("help-{tag}-{}.png", size.0));
                crate::test_support::visual::write(
                    &path,
                    &rt,
                    (size.0 as u32 * 2, size.1 as u32 * 2),
                    &pixels,
                    "Find keyboard help",
                );
            }
        }
    }
}

#[test]
fn a_confirmation_over_find_swallows_keys_that_do_not_answer_it() {
    let mut rt = toolbar();
    assert!(
        rt.state.connection.find_query.focused,
        "Find holds the keyboard"
    );
    let before = rt.state.connection.find_query.text.clone();
    rt.update(|d, _| {
        d.pending = Some(crate::desk::Pending::Close(
            phux_protocol::ids::ResourceId::local(1),
        ))
    });
    settle(&mut rt);
    let _ = rt.state.connection.client.as_ref().unwrap().take_outbound();
    rt.key(crate::keys::plain(Key::Text("x".into())));
    settle(&mut rt);
    assert_eq!(
        rt.state.connection.find_query.text, before,
        "the query is not edited"
    );
    assert!(rt.state.pending.is_some(), "x does not answer the bar");
    no_input(&rt);
    rt.key(crate::keys::plain(Key::Escape));
    settle(&mut rt);
    assert_eq!(rt.state.pending, None, "Escape still answers it");
    no_input(&rt);
}
