//! Desktop onboarding across an enrolled network connection, with an inert ACP peer.
use super::*;

fn install_opencode_fixture(server: &Server) {
    use std::os::unix::fs::PermissionsExt as _;
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let peer = manifest
        .join("../../../crates/phux/tests/fixtures/acp_peer.py")
        .canonicalize()
        .unwrap();
    std::fs::create_dir_all(server.root.join("bin")).unwrap();
    let executable = server.root.join("bin/opencode");
    std::fs::write(&executable, format!(
        "#!/bin/sh\nif [ \"$*\" = 'auth login' ]; then printf '%s\\n' \"$PWD\" > \"$HOME/signin-cwd\"; exit 0; fi\n[ \"$*\" = acp ] || exit 91\nprintf '%s\\n' \"$PWD\" > \"$HOME/acp-cwd\"\nexec /usr/bin/python3 '{}' '{}'\n",
        peer.display(), server.root.join("peer.jsonl").display(),
    )).unwrap();
    std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn open_picker(cx: &mut TestAppContext, handle: gpui::AnyWindowHandle, desk: &Entity<Desk>) {
    cx.update_window(handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("native-agent", window, cx))
    })
    .unwrap();
    wait_for(cx, |cx| {
        let cx = &mut gpui::VisualTestContext::from_window(handle, cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.debug_bounds("native-launch-opencode").is_some()
    });
}

fn click(cx: &mut TestAppContext, handle: gpui::AnyWindowHandle, selector: &'static str) {
    let cx = &mut gpui::VisualTestContext::from_window(handle, cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let bounds = cx.debug_bounds(selector).unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
#[ignore = "requires checkout CLI and disposable XDG_CONFIG_HOME/XDG_STATE_HOME; run alone"]
fn remote_opencode_onboarding_launches_headless_in_focused_folder_and_reconnects(
    cx: &mut TestAppContext,
) {
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("disposable config"));
    assert!(config.starts_with(std::env::temp_dir()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let local = Server::new();
    let remote = Server::with_listener(Some(port));
    install_opencode_fixture(&remote);
    remote.run(&[
        "new",
        "--json",
        "-s",
        "native-work",
        "--cwd",
        remote.root.to_str().unwrap(),
    ]);
    let checkout = remote.root.join("focused checkout");
    std::fs::create_dir(&checkout).unwrap();
    std::fs::create_dir_all(config.join("phux")).unwrap();
    std::fs::write(
        config.join("phux/config.toml"),
        remote.paired_remote_entry("native-remote"),
    )
    .unwrap();
    let desktop = desktop(cx);
    let handle = local.launch_named(&desktop, "journey-A", true, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.work_catalog().iter().any(|work| {
                work.destination.host.as_deref() == Some("native-remote")
                    && work.destination.name == "native-work"
            })
        })
    });
    let target = desk.read_with(cx, |desk, _| {
        desk.work_catalog()
            .into_iter()
            .find(|work| {
                work.destination.host.as_deref() == Some("native-remote")
                    && work.destination.name == "native-work"
            })
            .unwrap()
            .destination
    });
    desk.update(cx, |desk, cx| desk.navigate_work(target, cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && desk.read_with(cx, |desk, _| desk.focused_terminal().is_some())
    });
    let initial = desk.read_with(cx, |desk, _| desk.focused_terminal().unwrap());
    let id = match initial {
        phux_protocol::ResourceId::Local { id } => id,
        _ => panic!("remote connection uses host-local IDs"),
    };
    let spawned: serde_json::Value = serde_json::from_slice(&remote.output(
        &[
            "spawn",
            "--json",
            "--target",
            &format!("@{id}"),
            "--cwd",
            checkout.to_str().unwrap(),
        ],
        b"",
    ))
    .unwrap();
    let focused = phux_protocol::ResourceId::local(
        u32::try_from(spawned["terminal_id"].as_u64().unwrap()).unwrap(),
    );
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .pane(&focused)
                .is_some()
        })
    });
    desk.update(cx, |desk, _| desk.reveal_terminal(&focused));
    let expected_cwd = checkout.canonicalize().unwrap();
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && desk.read_with(cx, |desk, _| {
                desk.focused_cwd().as_deref() == expected_cwd.to_str()
            })
    });
    let original_terminal = desk.read_with(cx, |desk, _| desk.focused_terminal().unwrap());
    // Discovery/launch must use the connected server even with no usable local CLI.
    desk.update(cx, |desk, _| {
        desk.phux = local.root.join("not-an-installed-cli")
    });
    open_picker(cx, *handle, &desk);
    click(cx, *handle, "opencode-sign-in");
    wait_for(cx, |_| remote.root.join("signin-cwd").exists());
    assert_eq!(
        std::fs::read_to_string(remote.root.join("signin-cwd"))
            .unwrap()
            .trim(),
        expected_cwd.to_str().unwrap()
    );
    desk.update(cx, |desk, _| desk.reveal_terminal(&original_terminal));
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.focused_terminal().as_ref() == Some(&original_terminal)
        })
    });
    open_picker(cx, *handle, &desk);
    click(cx, *handle, "native-launch-opencode");
    wait_for(cx, |cx| native_ready(&desk, cx));
    assert_eq!(
        std::fs::read_to_string(remote.root.join("acp-cwd"))
            .unwrap()
            .trim(),
        expected_cwd.to_str().unwrap()
    );
    submit_and_reconnect(cx, *handle, &desk);
    let ledger = std::fs::read_to_string(remote.root.join("peer.jsonl")).unwrap();
    assert_eq!(
        ledger
            .lines()
            .filter(|line| line.contains("\"started\""))
            .count(),
        1,
        "reconnect must not create another OpenCode backend"
    );
    assert_eq!(
        ledger
            .lines()
            .filter(|line| line.contains("session/prompt"))
            .count(),
        1,
        "reconnect must not replay the prompt"
    );
    assert!(!local.root.join("peer.jsonl").exists());
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

fn native_ready(desk: &Entity<Desk>, cx: &TestAppContext) -> bool {
    desk.read_with(cx, |desk, _| {
        let Some(placement) = desk.focused_placement() else {
            return false;
        };
        desk.conversation_visible(&placement.id)
            && desk
                .link
                .as_ref()
                .and_then(|link| {
                    link.connection
                        .conversation(placement.terminal_id.as_ref()?)
                })
                .and_then(|conversation| conversation.native.as_ref())
                .is_some_and(|native| native.connected && native.capabilities.prompt.is_supported())
    })
}

fn submit_and_reconnect(
    cx: &mut TestAppContext,
    handle: gpui::AnyWindowHandle,
    desk: &Entity<Desk>,
) {
    cx.update_window(handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            let placement = desk.focused_placement().unwrap().id.clone();
            desk.conversation_faces[&placement]
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("native onboarding proof", window, cx)
                });
            desk.send_conversation(&placement, cx);
        })
    })
    .unwrap();
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversations
                .values()
                .any(|conversation| {
                    conversation
                        .native
                        .as_ref()
                        .is_some_and(|native| !native.questions.is_empty())
                })
        })
    });
    let before = desk.read_with(cx, |desk, _| {
        desk.link.as_ref().unwrap().connection.connection_epoch()
    });
    desk.update(cx, |desk, _| {
        desk.link.as_ref().unwrap().connection.reconnect().unwrap()
    });
    wait_for(cx, |cx| {
        attached(desk, cx)
            && desk.read_with(cx, |desk, _| {
                desk.link.as_ref().unwrap().connection.connection_epoch() > before
            })
    });
    wait_for(cx, |cx| native_ready(desk, cx));
    desk.read_with(cx, |desk, _| {
        assert!(
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversations
                .values()
                .any(|conversation| conversation
                    .native
                    .as_ref()
                    .is_some_and(|native| !native.questions.is_empty())),
            "pending human permission must survive reconnect"
        )
    });
}
