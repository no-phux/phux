//! `phux_snapshot`, `phux_wait` and `phux_send_keys` check their target's
//! kind against a real server: a Terminal reads as before, and a document
//! is refused with `wrong_resource_kind` naming the tool that reads it, so
//! the two readers stay read-only (they never attach).

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
    let result = crate::tools::dispatch(tool, args)
        .await
        .unwrap_or_else(|err| panic!("{tool} {args}: {err}"));
    crate::contract_tests::validate_output(tool, &result);
    result
}

/// The refusal's `(code, remedy)`; every one exits 2.
async fn refused(tool: &str, args: &Value) -> (String, String) {
    let err = crate::tools::dispatch(tool, args)
        .await
        .expect_err("refused");
    let doc = err.document();
    assert_eq!(doc["exit_code"], 2, "{err}");
    let field = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    (field(&doc["error"]["code"]), field(&doc["remedy"]))
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
        let screen = ok("phux_snapshot", &target(&pane, &socket, none())).await;
        assert!(screen["cols"].as_u64().is_some(), "a Terminal still reads");
        let waited = target(&pane, &socket, json!({ "idle_ms": 50, "timeout_secs": 5 }));
        assert_eq!(ok("phux_wait", &waited).await["outcome"], "met");

        // Each document is refused for the tool that reads it.
        let until = json!({ "until": "x", "timeout_secs": 1 });
        for (tool, id, extra, instead) in [
            ("phux_snapshot", &tree_id, none(), "phux_tree_snapshot"),
            (
                "phux_snapshot",
                &surface_id,
                none(),
                "phux_surface_snapshot",
            ),
            ("phux_snapshot", &notes, none(), "phux snapshot @N"),
            ("phux_wait", &tree_id, until.clone(), "phux_tree_wait"),
            ("phux_wait", &notes, until.clone(), "phux_tree_wait"),
            (
                "phux_wait",
                &surface_id,
                until.clone(),
                "phux_surface_snapshot",
            ),
        ] {
            let (code, remedy) = refused(tool, &target(id, &socket, extra)).await;
            assert_eq!(code, "wrong_resource_kind", "{tool} {id:?}");
            assert!(remedy.contains(instead), "{tool} {id:?}: {remedy}");
        }

        // Send-keys: keys reach a Terminal only.
        for id in [&tree_id, &surface_id, &notes, &blob] {
            let keys = target(id, &socket, json!({ "keys": ["Enter"] }));
            let (code, remedy) = refused("phux_send_keys", &keys).await;
            assert_eq!(code, "wrong_resource_kind");
            assert!(remedy.contains("phux agent page"), "{remedy}");
        }

        drop(producer);
        join_after_shutdown(shutdown, server).await;
    });
}
