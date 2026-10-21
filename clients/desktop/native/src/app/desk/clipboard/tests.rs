use super::*;
use gpui::{Entity, TestAppContext, VisualTestContext};
use phux_protocol::{
    ids::{BootstrapId, StreamId},
    wire::frame::FrameKind,
};

const OSC52: &[u8] = b"\x1b]52;c;aHR0cHM6Ly9leGFtcGxlLmNvbS9sb2dpbg==\x07";
const LOGIN: &str = "https://example.com/login";

fn fixture(cx: &mut TestAppContext) -> (Entity<Desk>, Arc<Client>, VisualTestContext) {
    let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let mut connection = crate::workspace::view_tests::attached();
    let workspace = crate::workspace::view_tests::opened(&mut connection);
    let client = Arc::clone(&connection.client);
    desk.update(cx, |desk, cx| {
        desk.link = Some(Link {
            connection,
            workspace,
        });
        cx.notify();
    });
    let mut cx = VisualTestContext::from_window(handle, cx);
    cx.update(|window, _| window.activate_window());
    draw(&mut cx);
    cx.update(|window, cx| {
        desk.read_with(cx, |desk, cx| {
            let pane = desk.panes.get(&desk.focused_id().unwrap()).unwrap();
            assert!(pane.read(cx).focus_handle().is_focused(window));
        });
    });
    (desk, client, cx)
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn output(client: &Client, seq: u64, bytes: &[u8]) {
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: bytes.to_vec().into(),
        })
        .unwrap();
}

fn clipboard(cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap())
}

fn seed(cx: &mut VisualTestContext) {
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
}

#[gpui::test]
fn osc52_live_output_reaches_the_desktop_clipboard_once(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (desk, client, mut cx) = fixture(cx);
    seed(&mut cx);
    // Exercise the stream parser too: the sequence need not fit one output frame.
    output(&client, 1, &OSC52[..12]);
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(clipboard(&mut cx), "untouched");
    output(&client, 2, &OSC52[12..]);
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(clipboard(&mut cx), LOGIN);
    seed(&mut cx);
    draw(&mut cx);
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(
        clipboard(&mut cx),
        "untouched",
        "a redraw cannot replay a copy"
    );
    output(&client, 3, b"\x1b]52;c;?\x07");
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(
        clipboard(&mut cx),
        "untouched",
        "clipboard queries stay inert"
    );
}

#[gpui::test]
fn clipboard_writes_expire_on_blur_and_window_deactivation(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (desk, client, mut cx) = fixture(cx);
    seed(&mut cx);
    output(&client, 1, OSC52);
    // The desk restores an empty focus to the terminal. Give another native
    // focus handle the keyboard, as a sidebar or overlay would.
    cx.update(|window, cx| {
        let focus = desk.read(cx).focus.clone();
        focus.focus(window, cx);
    });
    cx.run_until_parked();
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(
        clipboard(&mut cx),
        "untouched",
        "blur expires a queued write"
    );
    output(&client, 2, OSC52);
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.focus_terminal(window, cx)));
    cx.run_until_parked();
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(
        clipboard(&mut cx),
        "untouched",
        "refocus cannot revive background output"
    );
    output(&client, 3, OSC52);
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(clipboard(&mut cx), LOGIN);
    seed(&mut cx);
    // A different native window must revoke this terminal's clipboard authority.
    let other = cx.add_empty_window();
    other.update(|window, _| window.activate_window());
    cx.run_until_parked();
    output(&client, 4, OSC52);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(
        clipboard(&mut cx),
        "untouched",
        "reactivation cannot revive background output"
    );
    output(&client, 5, OSC52);
    desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
    assert_eq!(clipboard(&mut cx), LOGIN, "reactivation restores live copy");
}

#[gpui::test]
fn clipboard_focus_survives_switching_duplicate_views_in_both_directions(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (desk, client, mut cx) = fixture(cx);
    let original = desk.read_with(&cx, |desk, _| desk.focused_id().unwrap());
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("view", window, cx)));
    draw(&mut cx);
    let mirror = desk.read_with(&cx, |desk, _| desk.focused_id().unwrap());
    assert_ne!(original, mirror);
    for (seq, placement) in [(1, &original), (2, &mirror), (3, &original)] {
        cx.update(|window, cx| {
            desk.update(cx, |desk, cx| {
                let tab = desk.link.as_ref().unwrap().workspace.active_id.clone();
                desk.focus_placement(&tab, placement, window, cx);
            })
        });
        draw(&mut cx);
        seed(&mut cx);
        output(&client, seq, OSC52);
        desk.update(&mut cx, |desk, cx| desk.on_activity(cx));
        assert_eq!(clipboard(&mut cx), LOGIN, "focused placement {placement}");
    }
}
