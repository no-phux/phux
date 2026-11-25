use super::*;

fn request(method: &str, params: Value) -> Request {
    Request {
        id: "request-1".into(),
        method: method.into(),
        params,
        caller: Caller {
            pid: std::process::id(),
            cwd: "/".into(),
        },
    }
}

#[tokio::test]
async fn hello_negotiates_only_real_capabilities() {
    let hello = operations::dispatch(
        Path::new("/absent.sock"),
        &request("hello", json!({"protocol": 1})),
    )
    .await
    .unwrap();
    assert_eq!(hello["name"], "phux");
    assert_eq!(hello["methods"], json!(METHODS));
    assert!(!METHODS.contains(&"focusPane"));
    assert!(
        operations::dispatch(
            Path::new("/absent.sock"),
            &request("hello", json!({"protocol": 2}))
        )
        .await
        .is_err()
    );
    assert!(
        operations::dispatch(
            Path::new("/absent.sock"),
            &request("focusPane", json!({"pane":"@1"}))
        )
        .await
        .is_err()
    );
}

#[test]
fn socket_paths_are_short_stable_and_coordinator_scoped() {
    let first = socket_path(Path::new("/tmp/a.sock"));
    assert_eq!(first, socket_path(Path::new("/tmp/a.sock")));
    assert_ne!(first, socket_path(Path::new("/tmp/b.sock")));
    assert_eq!(first.parent(), Some(Path::new("/tmp")));
    assert_eq!(first.file_name().unwrap().len(), 24);
}

#[test]
fn refuse_to_unlink_a_file_or_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    let link = dir.path().join("link");
    std::fs::write(&file, "keep").unwrap();
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(remove_owned_socket(&file).is_err());
    assert!(remove_owned_socket(&link).is_err());
    assert_eq!(std::fs::read_to_string(file).unwrap(), "keep");
    assert!(link.is_symlink());
}

#[tokio::test]
async fn replies_correlate_ids_and_close_after_one_line() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let task = tokio::spawn(async { answer(server, Path::new("/absent.sock")).await });
    client.write_all(br#"{"id":"hello-42","method":"hello","params":{"protocol":1},"caller":{"pid":1,"cwd":"/"}}
"#).await.unwrap();
    let mut reply = String::new();
    client.read_to_string(&mut reply).await.unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["id"], "hello-42");
    assert_eq!(reply["result"]["name"], "phux");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn oversized_and_unterminated_requests_have_no_effect() {
    for bytes in [vec![b'x'; MAX_REQUEST as usize + 1], b"{}".to_vec()] {
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async { answer(server, Path::new("/absent.sock")).await });
        let _ = client.write_all(&bytes).await;
        let _ = client.shutdown().await;
        assert!(task.await.unwrap().is_err());
    }
}

#[tokio::test]
async fn invalid_split_intent_is_rejected_before_dialing() {
    for params in [
        json!({"from":{"id":"@1","tab":"t"},"direction":"right","command":[],"size":0.5}),
        json!({"from":{"id":"@1","tab":"t"},"direction":"right","command":["echo"],"size":0}),
        json!({"from":{"id":"@1","tab":"t"},"direction":"diagonal","command":["echo"]}),
    ] {
        let error = operations::dispatch(Path::new("/absent.sock"), &request("split", params))
            .await
            .unwrap_err();
        assert!(!error.contains("connect"), "{error}");
    }
}

#[test]
fn current_pane_uses_tty_then_nearest_ancestor_not_cwd_or_focus() {
    use panes::{Inventory, Pane, PaneDetails, Processes};
    let processes = Processes::parse("10 1 ttys001\n20 10 ttys001\n30 20 ttys999\n40 1 ??\n");
    let inventory = Inventory {
        panes: vec![
            PaneDetails {
                pane: Pane {
                    id: "@1".into(),
                    tab: "one".into(),
                },
                tty: Some("/dev/ttys001".into()),
                command: None,
                child: Some(10),
            },
            PaneDetails {
                pane: Pane {
                    id: "@2".into(),
                    tab: "two".into(),
                },
                tty: Some("/dev/ttys002".into()),
                command: None,
                child: Some(40),
            },
        ],
        layouts: Vec::new(),
    };
    let caller = Caller {
        pid: 30,
        cwd: "/same".into(),
    };
    assert_eq!(
        inventory
            .current(Some("/dev/ttys002"), &caller, &processes)
            .unwrap()
            .id,
        "@2"
    );
    assert_eq!(
        inventory
            .current(Some("/dev/ttys999"), &caller, &processes)
            .unwrap()
            .id,
        "@1"
    );
    assert!(
        inventory
            .current(
                None,
                &Caller {
                    pid: 999,
                    cwd: "/same".into()
                },
                &processes
            )
            .is_none()
    );
    assert!(processes.0[&40].tty.is_none());
}

#[test]
fn pane_ids_cannot_be_selectors_or_satellite_routes() {
    assert_eq!(
        panes::resource_id("@12").unwrap(),
        phux_protocol::ResourceId::local(12)
    );
    for invalid in [
        ".",
        "@0",
        "@01",
        "@+1",
        "remote/@1",
        "%agent",
        "@1; rm",
        "@4294967296",
    ] {
        assert!(panes::resource_id(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn neighbors_do_not_stop_at_another_sessions_layout() {
    use phux_client::layout::{ArrangementDoc, Direction, LayoutNode, SplitDir};
    use phux_protocol::ResourceId;
    let mut document = ArrangementDoc::single(ResourceId::local(2));
    document.groups[0].state.tree = Some(LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.4,
        left: Box::new(LayoutNode::Leaf(ResourceId::local(2))),
        right: Box::new(LayoutNode::Leaf(ResourceId::local(3))),
    });
    document.slots.push(phux_client::layout::Slot {
        strip: vec![ResourceId::local(2), ResourceId::local(4)],
        default: 0,
        shown: 0,
    });
    let pane = |id| panes::PaneDetails {
        pane: panes::Pane {
            id: format!("@{id}"),
            tab: "same-tab".into(),
        },
        tty: None,
        command: None,
        child: None,
    };
    let inventory = panes::Inventory {
        panes: vec![pane(2), pane(3)],
        layouts: vec![ArrangementDoc::single(ResourceId::local(1)), document],
    };
    assert_eq!(inventory.neighbor("@2", Direction::Right).unwrap().id, "@3");
    assert_eq!(inventory.neighbor("@4", Direction::Right).unwrap().id, "@3");
    assert_eq!(
        inventory.representative("@4").unwrap(),
        ResourceId::local(2)
    );
    assert!(inventory.neighbor("@3", Direction::Right).is_none());
}
