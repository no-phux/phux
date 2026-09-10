//! `LIST_DIRECTORY` over the wire (`docs/spec/L3.md` §4): the host query is
//! advertised, answers with the serving host's child directories, and
//! refuses a missing path with a typed code rather than an `ERROR`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::wire::frame::{
    DirectoryErrorCode, DirectoryListingResult, FrameKind, TYPE_ATTACH_READY,
    TYPE_DIRECTORY_LISTING, TYPE_HELLO_OK,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_raw_socket,
};

const SESSION: &str = "dirs";

async fn connect(path: &std::path::Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "list-directory-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK, "HELLO must be accepted");
    let FrameKind::HelloOk { server_caps, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    assert!(
        server_caps.features.contains(ServerFeature::ListDirectory),
        "server must advertise LIST_DIRECTORY: {server_caps:?}"
    );
    stream
}

async fn list(stream: &mut UnixStream, request_id: u32, path: &str) -> DirectoryListingResult {
    send_frame(
        stream,
        &FrameKind::ListDirectory {
            request_id,
            path: path.to_owned(),
        },
    )
    .await;
    loop {
        let (type_byte, frame) = recv_typed(stream).await;
        if type_byte != TYPE_DIRECTORY_LISTING {
            continue;
        }
        let FrameKind::DirectoryListing {
            request_id: got,
            result,
        } = frame
        else {
            panic!("expected DIRECTORY_LISTING, got {frame:?}");
        };
        assert_eq!(got, request_id, "reply must correlate to its request");
        return result;
    }
}

#[test]
fn list_directory_answers_with_child_directories_and_typed_refusals() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(tree.join("beta")).unwrap();
        std::fs::create_dir_all(tree.join("alpha")).unwrap();
        std::fs::write(tree.join("notes.txt"), b"x").unwrap();

        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 30");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), SESSION, cmd);

        let mut stream = connect(&socket_path).await;
        send_frame(&mut stream, &attach_by_name(SESSION)).await;
        loop {
            let (type_byte, _) = recv_typed(&mut stream).await;
            if type_byte == TYPE_ATTACH_READY {
                break;
            }
        }

        let listing = list(&mut stream, 7, tree.to_str().unwrap())
            .await
            .expect("an existing directory lists");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"], "directories only, sorted");
        assert_eq!(listing.path, tree.to_str().unwrap());
        assert_eq!(listing.parent.as_deref(), tmp.path().to_str());
        assert!(!listing.truncated);

        let refusal = list(&mut stream, 8, tree.join("missing").to_str().unwrap())
            .await
            .expect_err("a missing path is refused");
        assert_eq!(refusal.code, DirectoryErrorCode::NotFound);

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
