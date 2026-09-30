//! A consumer's host query reaches the satellite, not the hub's filesystem.

use super::*;
use phux_protocol::caps::{ColorSupport, LayerSet, ServerFeatureExt};
use phux_protocol::wire::frame::{PathErrorCode, PathKind, PathQueryResult};

async fn connect(path: &std::path::Path) -> UnixStream {
    let mut socket = wait_for_raw_socket(path, STEP_DEADLINE).await;
    send_frame(
        &mut socket,
        &FrameKind::Hello {
            client_name: "satellite-path-query-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (_, frame) = recv_typed(&mut socket).await;
    let FrameKind::HelloOk { server_caps, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    assert!(
        server_caps
            .features_ext
            .contains(ServerFeatureExt::PathQuery)
    );
    socket
}

async fn search(socket: &mut UnixStream, id: u32, root: &str, host: &str) -> PathQueryResult {
    send_frame(
        socket,
        &FrameKind::PathQuery {
            request_id: id,
            root: root.to_owned(),
            query: "file".to_owned(),
            recursive: true,
            host: Some(SatelliteHost::new(host)),
        },
    )
    .await;
    loop {
        if let (_, FrameKind::PathResults { request_id, result }) = recv_typed(socket).await
            && request_id == id
        {
            return result;
        }
    }
}

#[test]
fn satellite_search_remaps_correlation_and_refuses_unknown_hosts() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let tree = tmp.path().join("sat-tree");
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        std::fs::write(tree.join("nested/file name.txt"), "test").unwrap();
        let (port, sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock")).await;
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", port)],
        );
        let mut hub = connect(&tmp.path().join("hub.sock")).await;
        let root = tree.to_str().unwrap();
        let deadline = Instant::now() + STEP_DEADLINE;
        let mut id = 20;
        let result = loop {
            let result = search(&mut hub, id, root, "sat").await;
            if !matches!(&result, Err(error) if error.message.contains("is unreachable"))
                || Instant::now() >= deadline
            {
                break result;
            }
            id += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        .expect("satellite must answer host search");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].kind, PathKind::File);
        assert_eq!(
            result.rows[0].path,
            tree.join("nested/file name.txt").to_str().unwrap()
        );
        let missing = search(&mut hub, 500, root, "ghost").await.unwrap_err();
        assert_eq!(missing.code, PathErrorCode::Other);
        assert!(missing.message.contains("ghost"));
        drop(hub);
        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
        drop(sat_shutdown);
        sat_task.await.unwrap().unwrap();
    });
}
