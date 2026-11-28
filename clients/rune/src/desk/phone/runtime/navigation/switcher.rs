use super::*;
use phux_protocol::wire::frame::Command;

fn read_request(client: &Client, pane: u32) -> u32 {
    let mut read = None;
    for frame in client.take_outbound() {
        match FrameKind::decode(&frame).unwrap().0 {
            FrameKind::Command {
                request_id,
                command:
                    Command::GetScreen {
                        terminal_id,
                        request_scrollback: None,
                        cells: false,
                        format: 0,
                    },
            } => {
                assert_eq!(terminal_id, ResourceId::local(pane));
                assert!(read.replace(request_id).is_none());
            }
            FrameKind::Attach { .. }
            | FrameKind::InputKey { .. }
            | FrameKind::InputPaste { .. } => panic!("preview changed the live session"),
            _ => {}
        }
    }
    read.expect("one read-only screen request")
}

#[test]
fn swiping_previews_without_attaching_and_only_the_current_read_can_paint() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((393.0, 852.0), 2.0);
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    let _ = client.take_outbound();
    tap(&mut rt, "phone-place", 0.1);
    assert_eq!(rt.state.phone_page(), Some(Page::Switcher));
    let before = rt.state.selected_session_name();
    let first = read_request(&client, 1);
    let (x, y) = rt.find("phone-switcher-card").unwrap();
    // Past a third of the stage, as the terminal's swipe pages.
    for (phase, dx, at) in [
        (TouchPhase::Started, 0.0, 0.3),
        (TouchPhase::Moved, -100.0, 0.38),
        (TouchPhase::Ended, -150.0, 0.44),
    ] {
        rt.touch(1, phase, x + dx, y, at);
        settle(&mut rt);
    }
    assert_eq!(
        rt.state.phone_page(),
        Some(Page::Switcher),
        "swiping doesn't open the card"
    );
    assert_eq!(rt.state.selected_session_name(), before);
    let second = read_request(&client, 4);
    let result = |text: &str| {
        CommandResult::OkWith(CommandValue::Json(
            serde_json::json!({
                "lines": ["", text, "  ❯ ", ""]
            })
            .to_string(),
        ))
    };
    rt.update(|d, tx| {
        assert!(!d.apply(
            &client,
            phux_client_runtime::control::Event::CommandResult {
                request_id: first,
                result: result("stale screen"),
            },
            tx
        ));
        assert!(d.apply(
            &client,
            phux_client_runtime::control::Event::CommandResult {
                request_id: second,
                result: result("Pinned to the previewed session"),
            },
            tx
        ));
    });
    settle(&mut rt);
    let page = rt.state.phone.as_ref().unwrap().switcher.as_ref().unwrap();
    assert_eq!(
        page.preview.as_deref(),
        Some("\nPinned to the previewed session\n  ❯ ")
    );
    if let Some(dir) = std::env::var_os("RUNE_COMPOSER_SNAP") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for _ in 0..90 {
            rt.frame(1.0 / 60.0);
        }
        let gpu = rune::render::Gpu::headless();
        let size = (786, 1704);
        let pixels = crate::ui::offscreen_as_app(&gpu, &mut rt, size);
        crate::test_support::visual::write(
            &dir.join("switcher-393.png"),
            &rt,
            size,
            &pixels,
            "session preview",
        );
    }
    tap(&mut rt, "phone-switcher-card", 0.7);
    assert!(rt.state.phone_page().is_none());
    assert_ne!(rt.state.selected_session_name(), before);
    no_terminal_input(&client);
}

fn switcher_page(rt: &Runtime<Desk>) -> &crate::desk::phone::switcher::Switcher {
    rt.state.phone.as_ref().unwrap().switcher.as_ref().unwrap()
}

fn card_shift(rt: &Runtime<Desk>) -> [f32; 2] {
    switcher_page(rt).slide.shift.get()
}

fn previewed(rt: &Runtime<Desk>) -> usize {
    rt.state.switcher_selection().unwrap().1
}

fn feedback(rt: &mut Runtime<Desk>) -> Option<crate::desk::phone::touch::Feedback> {
    rt.state.phone.as_mut().unwrap().feedback.take()
}

/// The switcher's card is swiped as the terminal is: one to one with the
/// finger and without a view running per move, a rubber band where no
/// session lies beyond, home again when let go short, a detent once at a
/// third of the stage, and a page there or on a flick.
#[test]
fn the_switcher_card_follows_the_finger_and_pages_as_the_terminal_does() {
    use crate::desk::phone::touch::Feedback;
    use TouchPhase::{Ended, Moved, Started};
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    // The app's poll: where a paged card settles.
    rt.poll = Some(Desk::poll);
    tap(&mut rt, "phone-place", 0.1);
    let (x, y) = rt.find("phone-switcher-card").unwrap();
    let _ = feedback(&mut rt);
    // One to one, with no commit per move.
    rt.touch(1, Started, x, y, 1.0);
    rt.touch(1, Moved, x - 20.0, y, 1.01);
    settle(&mut rt);
    let commits = rt.stats.commits;
    for dx in [40.0, 80.0, 120.0] {
        rt.touch(1, Moved, x - dx, y, 1.02);
        rt.frame(1.0 / 60.0);
        assert_eq!(card_shift(&rt), [-dx, 0.0]);
    }
    assert_eq!(rt.stats.commits, commits, "a move ran the view");
    assert_eq!(feedback(&mut rt), None, "short of a third");
    // Past a third of the stage: one detent.
    rt.touch(1, Moved, x - 140.0, y, 1.03);
    rt.frame(1.0 / 60.0);
    assert_eq!(feedback(&mut rt), Some(Feedback::Detent));
    rt.touch(1, Moved, x - 150.0, y, 1.04);
    rt.frame(1.0 / 60.0);
    assert_eq!(feedback(&mut rt), None, "once per crossing");
    // Back short of it and let go, still: home, on the same session.
    rt.touch(1, Moved, x - 60.0, y, 1.05);
    for _ in 0..12 {
        rt.frame(1.0 / 60.0);
    }
    rt.touch(1, Ended, x - 60.0, y, 1.3);
    settle(&mut rt);
    assert_eq!(previewed(&rt), 0);
    assert_eq!(card_shift(&rt), [0.0, 0.0]);
    // Before the first session nothing lies beyond: it resists.
    rt.touch(1, Started, x, y, 2.0);
    rt.touch(1, Moved, x + 20.0, y, 2.01);
    rt.touch(1, Moved, x + 200.0, y, 2.02);
    rt.frame(1.0 / 60.0);
    let band = card_shift(&rt)[0];
    assert!(band > 0.0 && band < 100.0, "rubber band: {band}");
    rt.touch(1, Ended, x + 200.0, y, 2.03);
    settle(&mut rt);
    assert_eq!(previewed(&rt), 0);
    // A flick pages, ticking the detent it never crossed.
    let _ = feedback(&mut rt);
    rt.touch(1, Started, x, y, 3.0);
    rt.touch(1, Moved, x - 20.0, y, 3.01);
    rt.frame(1.0 / 60.0);
    rt.touch(1, Moved, x - 60.0, y, 3.02);
    rt.touch(1, Ended, x - 60.0, y, 3.03);
    settle(&mut rt);
    assert_eq!(previewed(&rt), 1, "a flick pages");
    assert_eq!(feedback(&mut rt), Some(Feedback::Detent));
    assert_eq!(rt.state.phone_page(), Some(Page::Switcher));
    // The next card was drawn beside the stage and settles in, next frame.
    for _ in 0..30 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(card_shift(&rt), [0.0, 0.0]);
    no_terminal_input(&client);
}
