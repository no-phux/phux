//! `phux_snapshot`, `phux_wait` and `phux_send_keys` dispatch on their
//! target's kind against a real server: a document target reads as its
//! per-kind tool reads it, a Terminal reads as before, and a verb that does
//! not apply is refused by kind.

use phux_client::tree::Kind;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, join_after_shutdown, run_local, spawn_server_with, wait_for_socket,
};

use super::*;

/// A 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44, 0x41, 0x54,
    0x78, 0x9c, 0x63, 0x60, 0, 0, 0, 2, 0, 1, 0xe5, 0x27, 0xde, 0xfc, 0, 0, 0, 0, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn target(id: &ResourceId, socket: &std::path::Path, mut args: Value) -> Value {
    args["target"] = json!(phux_client::selector::format_terminal_id(id));
    args["socket"] = json!(socket);
    args
}

async fn ok(tool: &str, args: &Value) -> Value {
    let mut result = crate::tools::dispatch(tool, args)
        .await
        .unwrap_or_else(|err| panic!("{tool} {args}: {err}"));
    let image = result.as_object_mut().unwrap().remove(IMAGE_CONTENT_KEY);
    crate::contract_tests::validate_output(tool, &result);
    if let Some(image) = image {
        result[IMAGE_CONTENT_KEY] = image;
    }
    result
}

async fn refused(tool: &str, args: &Value) -> String {
    let err = crate::tools::dispatch(tool, args)
        .await
        .expect_err("refused");
    let code = err.document()["error"]["code"].as_str().unwrap().to_owned();
    assert_eq!(err.document()["exit_code"], 2, "{err}");
    code
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture server, every kind and verb in turn"
)]
fn the_screen_verbs_dispatch_on_the_target_kind() {
    run_local(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("kinds.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), Some("main"), |_| {});
        drop(wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await);
        let view = phux_client::state::get_state(&socket).await.unwrap();
        let pane = phux_client::resource::terminals(&view.into_snapshot_ignoring_degradation())
            .next()
            .unwrap()
            .id
            .clone();
        let mut producer = Connection::connect(&socket).await.unwrap();
        let tree_id = tree::open_kind(&mut producer, Kind::Tree, &pane, "smoke", None)
            .await
            .unwrap()
            .resource;
        let surface_id = tree::open_kind(&mut producer, Kind::Surface, &pane, "smoke", None)
            .await
            .unwrap()
            .resource;
        let notes = tree::open_document(&mut producer, &pane, "text/markdown", Some("notes"))
            .await
            .unwrap()
            .resource;
        let blob = tree::open_document(&mut producer, &pane, "application/octet-stream", None)
            .await
            .unwrap()
            .resource;
        for (id, bytes) in [
            (&tree_id, br#"{"title":"hello"}"#.to_vec()),
            (&surface_id, PNG.to_vec()),
            (&notes, b"# hi\n## Done".to_vec()),
            (&blob, vec![0, 1, 2]),
        ] {
            tree::publish(&mut producer, id, bytes).await.unwrap();
        }
        let none = || json!({});

        // Snapshot: each document as its own tool reads it.
        let snap = ok("phux_snapshot", &target(&tree_id, &socket, none())).await;
        let per_kind = ok("phux_tree_snapshot", &target(&tree_id, &socket, none())).await;
        assert_eq!(snap, per_kind);
        let surface = ok("phux_snapshot", &target(&surface_id, &socket, none())).await;
        assert_eq!(
            (surface["width"].as_u64(), surface["height"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(surface[IMAGE_CONTENT_KEY]["mimeType"], "image/png");
        let text = ok("phux_snapshot", &target(&notes, &socket, none())).await;
        assert_eq!(
            (text["text"].as_str(), text["content_type"].as_str()),
            (Some("# hi\n## Done"), Some("text/markdown"))
        );
        let binary = ok("phux_snapshot", &target(&blob, &socket, none())).await;
        assert!(binary["text"].is_null(), "{binary}");
        assert_eq!(binary["bytes"], 3);
        let screen = ok("phux_snapshot", &target(&pane, &socket, none())).await;
        assert!(
            screen["cols"].as_u64().is_some(),
            "a Terminal still reads as a screen"
        );
        let tail = target(&tree_id, &socket, json!({ "tail": 3 }));
        assert_eq!(refused("phux_snapshot", &tail).await, "usage_error");

        // Wait: a tree or a text document follows its text.
        let until = |id, text| target(id, &socket, json!({ "until": text, "timeout_secs": 1 }));
        let met = ok("phux_wait", &until(&tree_id, "hello")).await;
        assert_eq!(met["outcome"], "met", "{met}");
        let met = ok("phux_wait", &until(&notes, "## Done")).await;
        assert_eq!(met["outcome"], "met", "{met}");
        let missed = ok("phux_wait", &until(&tree_id, "never-there")).await;
        assert_eq!(missed["outcome"], "timed_out", "{missed}");
        assert_eq!(
            refused("phux_wait", &until(&surface_id, "x")).await,
            "wrong_resource_kind"
        );
        assert_eq!(
            refused("phux_wait", &until(&blob, "x")).await,
            "usage_error"
        );
        let idle = target(&tree_id, &socket, json!({ "idle_ms": 50 }));
        assert_eq!(refused("phux_wait", &idle).await, "usage_error");
        let bare = target(&tree_id, &socket, none());
        assert_eq!(refused("phux_wait", &bare).await, "usage_error");

        // Send-keys: keys reach a Terminal only.
        for id in [&tree_id, &surface_id, &notes] {
            let keys = target(id, &socket, json!({ "keys": ["Enter"] }));
            assert_eq!(
                refused("phux_send_keys", &keys).await,
                "wrong_resource_kind"
            );
        }

        drop(producer);
        join_after_shutdown(shutdown, server).await;
    });
}
