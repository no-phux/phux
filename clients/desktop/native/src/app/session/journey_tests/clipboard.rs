//! A real PTY's OSC 52 reaches the desktop through a private server.

use super::*;

#[gpui::test]
#[ignore = "requires checkout CLI and disposable XDG state; run this journey alone"]
fn live_terminal_osc52_copies_to_the_desktop(cx: &mut TestAppContext) {
    let server = Server::new();
    let created: serde_json::Value = serde_json::from_slice(&server.output(
        &[
            "new", "--json", "-s", "clipboard", "--", "/bin/sh", "-c",
            r"read -r ready; printf '\033]52;c;aHR0cHM6Ly9leGFtcGxlLmNvbS9sb2dpbg==\007'; exec /bin/cat",
        ],
        b"",
    )).unwrap();
    let terminal =
        phux_protocol::ResourceId::local(created["terminal_id"].as_u64().unwrap() as u32)
            .to_string();
    let desktop = desktop(cx);
    let handle = server.launch_named(&desktop, "clipboard", false, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && placement_count(&desk, cx) == 1
    });
    cx.update_window(*handle, |_, window, cx| {
        window.activate_window();
        window.draw(cx).clear(cx);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into()));
    })
    .unwrap();
    cx.run_until_parked();
    server.run(&["send-keys", &terminal, "Enter"]);
    wait_for(cx, |cx| {
        cx.update(|cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref()
                == Some("https://example.com/login")
        })
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}
