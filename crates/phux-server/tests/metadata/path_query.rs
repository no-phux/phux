//! Local `PATH_QUERY` through the real socket, including request correlation.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{
    FrameKind, PathErrorCode, PathKind, PathQueryResult, PathStatus, TYPE_ATTACH_READY,
    TYPE_HELLO_OK, TYPE_PATH_RESULTS,
};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, join_after_shutdown, recv_typed, recv_until,
    run_local, send_frame, spawn_server_with_seed_cmd, wait_for_raw_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

async fn query(
    stream: &mut UnixStream,
    request_id: u32,
    root: &str,
    text: &str,
    recursive: bool,
    host: Option<SatelliteHost>,
) -> PathQueryResult {
    send_frame(
        stream,
        &FrameKind::PathQuery {
            request_id,
            root: root.to_owned(),
            query: text.to_owned(),
            recursive,
            host,
        },
    )
    .await;
    recv_until(stream, |kind, frame| {
        if kind != TYPE_PATH_RESULTS {
            return None;
        }
        let FrameKind::PathResults {
            request_id: actual,
            result,
        } = frame
        else {
            panic!("unexpected PATH_RESULTS frame {frame:?}");
        };
        assert_eq!(actual, request_id);
        Some(result)
    })
    .await
}

#[test]
fn paths_are_host_absolute_typed_and_refused_without_rerouting() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        std::fs::write(tree.join("nested/abc file.txt"), b"x").unwrap();
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 30");
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "paths", cmd);
        let mut stream = wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream,
            &FrameKind::Hello {
                client_name: "path-query-test".to_owned(),
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                client_caps: ClientCapabilities::new()
                    .with_color_support(ColorSupport::TrueColor)
                    .with_layers(LayerSet::all()),
            },
        )
        .await;
        assert_eq!(recv_typed(&mut stream).await.0, TYPE_HELLO_OK);
        send_frame(&mut stream, &attach_by_name("paths")).await;
        recv_until(&mut stream, |kind, _| {
            (kind == TYPE_ATTACH_READY).then_some(())
        })
        .await;

        let root = tree.to_str().unwrap();
        let browse = query(&mut stream, 31, root, "", false, None).await.unwrap();
        assert_eq!(browse.status, PathStatus::Complete);
        assert_eq!(browse.rows.len(), 1);
        assert_eq!(browse.rows[0].kind, PathKind::Directory);
        let search = query(&mut stream, 32, root, "abc", true, None)
            .await
            .unwrap();
        assert_eq!(search.rows.len(), 1);
        assert_eq!(search.rows[0].kind, PathKind::File);
        assert_eq!(
            search.rows[0].path,
            tree.join("nested/abc file.txt").to_str().unwrap()
        );
        let missing = query(&mut stream, 33, &format!("{root}/missing"), "", false, None)
            .await
            .unwrap_err();
        assert_eq!(missing.code, PathErrorCode::NotFound);
        let host = SatelliteHost::new("unknown".to_owned());
        let denied = query(&mut stream, 34, root, "abc", true, Some(host))
            .await
            .unwrap_err();
        assert_eq!(denied.code, PathErrorCode::Other);
        assert!(denied.message.contains("unknown"));
        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}
