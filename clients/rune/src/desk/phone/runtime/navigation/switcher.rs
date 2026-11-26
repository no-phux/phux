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
    for (phase, dx, at) in [
        (TouchPhase::Started, 0.0, 0.3),
        (TouchPhase::Moved, -64.0, 0.38),
        (TouchPhase::Ended, -80.0, 0.44),
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
