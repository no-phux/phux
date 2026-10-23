//! A live remote inventory, metadata wake, and exact fleet-to-pane navigation.

use super::*;
use crate::connection::AgentState;

#[gpui::test]
#[ignore = "requires checkout CLI and disposable XDG_CONFIG_HOME/XDG_STATE_HOME; run this journey alone"]
fn live_fleet_observes_state_without_views_and_opens_the_exact_remote_agent(
    cx: &mut TestAppContext,
) {
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("disposable config"));
    assert!(config.starts_with(std::env::temp_dir()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let local = Server::new();
    let remote = Server::with_listener(Some(port));
    let home: serde_json::Value = serde_json::from_slice(&remote.output(
        &["new", "--json", "-s", "fleet-agent", "--", "/bin/cat"],
        b"",
    ))
    .unwrap();
    let home_selector = format!("@{}", home["terminal_id"].as_u64().unwrap());
    let created: serde_json::Value = serde_json::from_slice(&remote.output(
        &[
            "spawn",
            "--target",
            &home_selector,
            "--json",
            "--",
            "/bin/cat",
        ],
        b"",
    ))
    .unwrap();
    let terminal =
        phux_protocol::ResourceId::local(created["terminal_id"].as_u64().unwrap() as u32);
    let selector = terminal.to_string();
    remote.run(&[
        "agent",
        "set",
        &selector,
        "--name",
        "fleet-reviewer",
        "--kind",
        "opencode",
        "--state",
        "working",
    ]);
    std::fs::create_dir_all(config.join("phux")).unwrap();
    std::fs::write(
        config.join("phux/config.toml"),
        remote.paired_remote_entry("fleet-remote"),
    )
    .unwrap();
    let desktop = desktop(cx);
    let handle = local.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && desk.read_with(cx, |desk, _| {
                desk.agent_catalog()
                    .iter()
                    .any(|agent| agent.info.name == "fleet-reviewer")
            })
    });
    let revision = desk.read_with(cx, |desk, _| desk.fleet.revision);
    // Working -> idle changes no Work attention or terminal count. It must
    // still wake the cached dashboard, not merely become visible on refresh.
    remote.run(&[
        "agent",
        "set",
        &selector,
        "--name",
        "fleet-reviewer",
        "--kind",
        "opencode",
        "--state",
        "idle",
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.fleet.revision > revision
                && desk.agent_catalog().iter().any(|agent| {
                    agent.info.name == "fleet-reviewer" && agent.info.state == AgentState::Idle
                })
        })
    });
    let mut inventory = crate::connection::Connection::browse(&remote.socket, None).unwrap();
    wait_for(cx, |_| {
        inventory.drain();
        inventory.pane(&terminal).is_some()
    });
    let before = inventory.pane(&terminal).unwrap().clone();
    let placements = placement_count(&desk, cx);
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.update_prefs(
                |prefs| {
                    prefs.agent_fleet = true;
                    prefs.animations = false;
                },
                cx,
            );
            desk.execute("agent-fleet", window, cx);
        })
    })
    .unwrap();
    assert_eq!(
        placement_count(&desk, cx),
        placements,
        "opening the grid acquires no views"
    );
    inventory.refresh_topology().unwrap();
    wait_for(cx, |_| {
        inventory
            .drain()
            .iter()
            .any(|event| matches!(event, phux_client_runtime::control::Event::TopologyChanged))
    });
    let after = inventory.pane(&terminal).unwrap();
    assert_eq!(
        (before.cols, before.rows),
        (after.cols, after.rows),
        "browsing casts no geometry vote"
    );
    assert!(inventory.client.attached_session().is_none());
    let target = desk.read_with(cx, |desk, _| {
        desk.agent_catalog()
            .into_iter()
            .find(|agent| agent.info.name == "fleet-reviewer")
            .unwrap()
    });
    assert_eq!(target.destination.host.as_deref(), Some("fleet-remote"));
    assert_eq!(target.terminal, terminal);
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.open_fleet_agent(target, window, cx))
    })
    .unwrap();
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && desk.read_with(cx, |desk, _| {
                desk.remote.as_deref() == Some("fleet-remote")
                    && desk.session == "fleet-agent"
                    && desk.focused_terminal() == Some(terminal.clone())
            })
    });
    desk.read_with(cx, |desk, _| assert!(desk.fleet_view.pending.is_none()));
    desktop.read_with(cx, |desktop, _| assert_eq!(desktop.windows.len(), 1));
    inventory.close();
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    std::fs::remove_file(config.join("phux/config.toml")).unwrap();
}
