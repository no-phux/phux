//! Close actions through actual keyboard and popup controls.
use super::*;
use phux_protocol::wire::frame::{Command, FrameKind};

fn live_workspace(desk: &mut Desk, cx: &mut gpui::Context<Desk>) {
    let mut connection = crate::workspace::view_tests::attached();
    let workspace = crate::workspace::view_tests::opened(&mut connection);
    desk.session = "views".into();
    desk.link = Some(Link {
        connection,
        workspace,
    });
    cx.notify();
}

#[gpui::test]
fn command_w_kills_the_focused_shell_without_optimistically_hiding_it(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, path) = fixture(cx);
    desk.update(cx, live_workspace);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    draw(cx);
    desk.update(cx, |desk, _| {
        desk.motion
            .set_spans(std::time::Duration::ZERO, std::time::Duration::ZERO)
    });
    cx.simulate_keystrokes("cmd-w");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        let link = desk.link.as_ref().unwrap();
        assert_eq!(
            link.workspace.tabs.len(),
            1,
            "the server must confirm closure"
        );
        let frames: Vec<_> = link
            .connection
            .client
            .take_outbound()
            .iter()
            .map(|bytes| FrameKind::decode(bytes).unwrap().0)
            .collect();
        assert!(
            frames.iter().any(|frame| matches!(frame,
                FrameKind::Command { command: Command::KillResource { terminal_id, .. }, .. }
                    if terminal_id == &phux_protocol::ResourceId::local(7)
            )),
            "Cmd-W must kill, not detach: {frames:?}"
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    std::fs::remove_dir_all(path).unwrap();
}

#[gpui::test]
fn session_confirmation_refuses_a_replaced_identity(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, path) = fixture(cx);
    desk.update(cx, live_workspace);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.update(|window, cx| {
        desk.update(cx, |desk, cx| {
            let target = desk.rename_session_target(1);
            desk.modal = Modal::KillSession {
                name: "views".into(),
                session: 1,
                target,
            };
            desk.link
                .as_mut()
                .unwrap()
                .connection
                .topology
                .as_mut()
                .unwrap()
                .sessions[0]
                .key = Some(phux_protocol::ids::SessionKey::new([9; 16]).unwrap());
            desk.confirm_kill_session(window, cx);
            assert!(
                desk.link
                    .as_ref()
                    .unwrap()
                    .connection
                    .client
                    .take_outbound()
                    .is_empty()
            );
            assert_eq!(
                desk.feed.peek().map(|entry| entry.title.as_str()),
                Some("Session changed")
            );
        })
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    std::fs::remove_dir_all(path).unwrap();
}

#[gpui::test]
fn inactive_empty_session_can_be_killed_from_its_three_dot_menu(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, path) = fixture(cx);
    let script = path.join("test-phux");
    let args = path.join("kill-args");
    let finished = path.join("kill-finished");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n/bin/sleep 0.5\ntouch '{}'\n",
            args.display(),
            finished.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    desk.update(cx, |desk, cx| {
        live_workspace(desk, cx);
        desk.phux = script;
        desk.prefs.sidebar_visible = true;
        let topology = desk
            .link
            .as_mut()
            .unwrap()
            .connection
            .topology
            .as_mut()
            .unwrap();
        let mut empty = topology.sessions[0].clone();
        empty.id = 2;
        empty.name = "empty".into();
        empty.members = 0;
        topology.sessions.push(empty);
        cx.notify();
    });
    let cx = &mut VisualTestContext::from_window(handle, cx);
    draw(cx);
    let row = cx
        .debug_bounds("work-Local-2")
        .expect("inactive session row");
    cx.simulate_mouse_move(row.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    let trigger = cx
        .debug_bounds("session-actions-Local-2")
        .expect("inactive session actions");
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw(cx);
    let menu = desk
        .read_with(cx, |desk, _| desk.menu_window())
        .expect("session menu");
    let menu_cx = &mut VisualTestContext::from_window(menu, cx);
    let kill = menu_cx
        .debug_bounds("glass-menu-kill-session")
        .expect("kill session item");
    menu_cx.simulate_click(kill.center(), Modifiers::none());
    menu_cx.run_until_parked();
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(
            matches!(&desk.modal, Modal::KillSession { name, session: 2, .. } if name == "empty")
        );
    });
    let dialog = cx.debug_bounds("modal-sheet").expect("kill confirmation");
    // The primary button is at the card's lower-right, inside its 24px padding.
    cx.simulate_click(
        point(dialog.right() - px(70.), dialog.bottom() - px(40.)),
        Modifiers::none(),
    );
    draw(cx);
    desk.read_with(cx, |desk, _| assert_eq!(desk.modal, Modal::None));
    run(&desk, "sidebar", cx);
    desk.read_with(cx, |desk, _| assert!(!desk.prefs.sidebar_visible));
    for _ in 0..300 {
        if finished.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        draw(cx);
    }
    assert!(finished.exists(), "test CLI must finish");
    let sent = std::fs::read_to_string(args).unwrap();
    assert!(
        sent.ends_with("kill\nempty\n"),
        "kill must target the clicked inactive session: {sent}"
    );
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    std::fs::remove_dir_all(path).unwrap();
}
