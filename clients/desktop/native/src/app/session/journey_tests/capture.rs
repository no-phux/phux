//! Capture dispatch and opt-in real OpenCode journeys.

use super::*;

#[gpui::test]
#[ignore = "requires checkout CLI and disposable XDG state; launches an argv-recording OpenCode fixture"]
fn capture_dispatches_through_the_server_and_follows_only_on_enter(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;
    let server = Server::new();
    server.run(&[
        "new",
        "--json",
        "-s",
        "capture-target",
        "--cwd",
        server.root.to_str().unwrap(),
    ]);
    std::fs::create_dir(server.root.join("bin")).unwrap();
    let agent = server.root.join("bin/opencode");
    std::fs::write(
        &agent,
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" >> \"$HOME/capture-argv\"\nexec /bin/cat\n",
    )
    .unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let desktop = desktop(cx);
    let handle = server.launch_named(&desktop, "capture-target", false, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    let expected_cwd = server.root.canonicalize().unwrap();
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && placement_count(&desk, cx) == 1
            && desk.read_with(cx, |desk, _| {
                desk.focused_cwd().as_deref() == expected_cwd.to_str()
            })
    });
    let original = snapshot(&desk, cx);
    capture_type(cx, *handle, "fix 東京; $(false) 'quoted'");
    let cwd = desk.read_with(cx, |desk, _| desk.focused_cwd().unwrap());
    press(
        cx,
        *handle,
        if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        },
    );
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.capture.draft.is_empty())
    });
    assert_eq!(
        snapshot(&desk, cx).tabs,
        original.tabs,
        "background spawn creates no placement or focus change"
    );
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.modal.clone()),
        crate::app::Modal::Capture
    );
    wait_for(cx, |_| server.root.join("capture-argv").exists());
    assert_eq!(
        std::fs::read_to_string(server.root.join("capture-argv")).unwrap(),
        format!("{cwd}\n--prompt\nfix 東京; $(false) 'quoted'\n")
    );
    // A second connection sees the spawned resource: the desktop did not spawn locally.
    let state: serde_json::Value =
        serde_json::from_slice(&server.output(&["list", "--json"], b"")).unwrap();
    assert!(
        state["terminals"].as_array().unwrap().len() >= 2,
        "the shell and background capture are both server resources: {state}"
    );

    capture_type(cx, *handle, "@capture-target next thought");
    press(cx, *handle, "enter");
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            assert!(
                desk.capture.error.is_none(),
                "capture {:?}: {:?}; panes: {:?}",
                desk.capture.draft,
                desk.capture.error,
                desk.link.as_ref().unwrap().connection.panes()
            )
        });
        placement_count(&desk, cx) == 2
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.modal == crate::app::Modal::None)
    });
    let after = snapshot(&desk, cx);
    assert_ne!(
        after.active_tab, original.active_tab,
        "Enter focuses the new tab"
    );
    wait_for(cx, |_| {
        std::fs::read_to_string(server.root.join("capture-argv"))
            .unwrap_or_default()
            .ends_with("next thought\n")
    });
    assert_eq!(
        std::fs::read_to_string(server.root.join("capture-argv")).unwrap(),
        format!("{cwd}\n--prompt\nfix 東京; $(false) 'quoted'\n{cwd}\n--prompt\nnext thought\n")
    );
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

fn capture_type(cx: &mut TestAppContext, handle: gpui::AnyWindowHandle, text: &str) {
    // A real palette prefix and the actual Kit input, rather than calling submit directly.
    press(cx, handle, "cmd-shift-p");
    let cx = &mut gpui::VisualTestContext::from_window(handle, cx);
    for character in format!("!{text}").chars() {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            let key = character.to_string();
            window.dispatch_keystroke(
                gpui::Keystroke {
                    modifiers: gpui::Modifiers::default(),
                    key: key.clone(),
                    key_char: Some(key),
                },
                cx,
            );
        });
        cx.run_until_parked();
    }
}

#[gpui::test]
#[ignore = "opt-in real OpenCode/model smoke; requires PHUX_CAPTURE_OPENCODE_BIN and PHUX_CAPTURE_HOME"]
fn capture_real_opencode_receives_first_prompt(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;
    assert!(std::env::var_os("PHUX_CAPTURE_OPENCODE_BIN").is_some());
    assert!(std::env::var_os("PHUX_CAPTURE_HOME").is_some());
    let server = Server::new();
    std::fs::create_dir(server.root.join("bin")).unwrap();
    let agent = server.root.join("bin/opencode");
    std::fs::write(&agent, "#!/bin/sh\nexport HOME=\"$PHUX_CAPTURE_HOME\"\nexport XDG_CONFIG_HOME=\"$HOME/.config\"\nexport XDG_STATE_HOME=\"$HOME/.local/state\"\nexec \"$PHUX_CAPTURE_OPENCODE_BIN\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && placement_count(&desk, cx) == 1
    });
    capture_type(
        cx,
        *handle,
        "What is 367 plus 524? Reply with only the decimal sum. Do not use tools or modify files.",
    );
    press(cx, *handle, "enter");
    wait_for(cx, |cx| placement_count(&desk, cx) == 2);
    let terminal = desk.read_with(cx, |desk, _| desk.focused_terminal().unwrap());
    let target = terminal.to_string();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        cx.run_until_parked();
        let screen =
            String::from_utf8(server.output(&["snapshot", "--tail", "200", &target], b"")).unwrap();
        if screen.contains("891") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real OpenCode did not answer the initial prompt:\n{screen}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // When the normal integration is installed, the agent publishes its own
    // child resource; capture does not fabricate provider/session identity.
    let agent_registered = desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap()
            .agent_sessions
            .iter()
            .any(|agent| agent.parent.as_ref() == Some(&terminal))
    });
    eprintln!(
        "real OpenCode answered initial capture; AgentSession child registered: {agent_registered}"
    );
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}
