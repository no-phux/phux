use super::*;
use crate::desk::Shown;
use crate::test_support::terminal::Replica;

fn desk_with_client() -> Desk {
    let replica = Replica::new(85, 24, 2);
    let mut desk = crate::test_support::window();
    desk.connection.epoch = replica.client.connection_epoch();
    desk.connection.status = Status::Attached;
    desk.connection.client = Some(replica.client);
    desk.connection.action_transport = Some("quic");
    desk.machines.current = Some("remote-a".into());
    desk.connection.shown = Some(Shown {
        resource: replica.terminal,
        view: Some(replica.views[0]),
    });
    desk
}

#[test]
fn detached_rune_omits_previous_client_target() {
    let mut desk = desk_with_client();
    let observed = serde_json::to_value(desk.capture_plugin_context()).unwrap();
    assert_eq!(observed["source"], "rune_palette");
    assert_eq!(observed["client"], 1);
    assert_eq!(observed["target"]["session_name"], "budget");
    assert_eq!(observed["target"]["resource_handle"], "@1");
    assert_eq!(observed["target"]["route"], "remote-a");
    assert!(observed["target"].get("authority").is_none());
    assert!(observed["target"].get("workspace").is_none());
    assert!(observed["target"].get("host").is_none());
    desk.connection.epoch += 1;
    let stale = serde_json::to_value(desk.capture_plugin_context()).unwrap();
    assert_eq!(stale["target"], serde_json::json!({}));
    assert!(stale.get("client").is_none());
}

#[test]
fn refreshing_the_catalog_preserves_a_nondefault_keyboard_pick_by_id() {
    let mut desk = crate::test_support::window();
    let item = |id: &str, title: &str| ConfiguredAction {
        plugin: "proof.rune".into(),
        id: id.into(),
        title: title.into(),
    };
    let first = item("first", "Test catalog alpha");
    let selected = item("selected", "Test catalog bravo");
    desk.palette_open = true;
    desk.palette_query.text = "Test catalog".into();
    desk.plugin_actions.catalog = vec![first.clone(), selected.clone()];
    desk.palette_picked = 1;
    assert_eq!(
        desk.palette_hits()[1].item.target,
        Target::Extension(selected.clone())
    );
    desk.install_action_catalog(Ok(vec![selected.clone(), first]));
    assert_eq!(
        desk.palette_hits()[desk.palette_picked].item.target,
        Target::Extension(selected)
    );
}

#[test]
fn rune_palette_runs_the_configured_child_with_the_committed_target() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path();
    let config = path.join("config.toml");
    std::fs::write(&config, "[[plugins]]\nmanifest = 'phux-plugin.toml'\n").unwrap();
    std::fs::write(
        path.join("phux-plugin.toml"),
        r#"
id = "proof.rune"
name = "Proof"
version = "1.0.0"
min_phux_version = "0.0.2"
[[actions]]
id = "context"
title = "Plugin Context Evidence"
command = ["/bin/sh", "-c", 'test "$PHUX_SOCKET" = /dev/null/phux-action.sock || exit 42; test -z "$PHUX_WS_TOKENS$PHUX_WS_TLS_KEY" || exit 43; while ! test -f continue; do sleep 0.05; done; printf "%s" "$PHUX_ACTION_CONTEXT" > context.json']
"#,
    )
    .unwrap();
    let mut desk = desk_with_client();
    desk.plugin_actions.config = config;
    desk.plugin_actions.log = path.join("plugin-runs.ndjson");
    desk.toggle_palette();
    let delivery = desk.plugin_actions.loading.take().unwrap();
    let catalog = delivery.recv_timeout(Duration::from_secs(5)).unwrap();
    let (send, recv) = channel();
    send.send(catalog).unwrap();
    desk.plugin_actions.loading = Some(recv);
    assert!(desk.pump_plugin_actions());
    desk.palette_query.text = "Plugin Context Evidence".into();
    let hits = desk.palette_hits();
    assert!(matches!(&hits[0].item.target, Target::Extension(action) if action.id == "context"));
    let drawn = hits[0].item.target.clone();
    desk.palette_run_target(drawn, &mut rune::Tx::default());
    assert!(!desk.palette_open);
    let delivery = desk.plugin_actions.running.take().unwrap();
    desk.machines.current = Some("remote-b".into());
    desk.connection.shown = Some(Shown {
        resource: ResourceId::local(2),
        view: None,
    });
    std::fs::write(path.join("continue"), "").unwrap();
    delivery
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let captured: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("context.json")).unwrap()).unwrap();
    assert_eq!(captured["source"], "rune_palette");
    assert_eq!(captured["client"], 1);
    assert_eq!(captured["target"]["resource_handle"], "@1");
    assert_eq!(captured["target"]["route"], "remote-a");
    assert_eq!(captured["target"]["transport"], "quic");
    assert!(captured["target"].get("entry").is_none());
}
