//! `LIST_DIRECTORY` over the wire (`docs/spec/L3.md` §4): the serving host's
//! child directories, sorted, and a typed refusal (not an `ERROR`) for a
//! missing path.

use phux_protocol::wire::frame::{DirectoryErrorCode, DirectoryListingResult, FrameKind};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{recv_until, run_local, send_frame, spawn_server_connected};

async fn list(stream: &mut UnixStream, request_id: u32, path: &str) -> DirectoryListingResult {
    send_frame(
        stream,
        &FrameKind::ListDirectory {
            request_id,
            path: path.to_owned(),
            host: None,
        },
    )
    .await;
    recv_until(stream, |_, frame| match frame {
        FrameKind::DirectoryListing {
            request_id: got,
            result,
        } if got == request_id => Some(result),
        _ => None,
    })
    .await
}

#[test]
fn list_directory_answers_with_child_directories_and_typed_refusals() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(tree.join("beta")).unwrap();
        std::fs::create_dir_all(tree.join("alpha")).unwrap();
        std::fs::write(tree.join("notes.txt"), b"x").unwrap();
        let (_server, mut stream) = spawn_server_connected(Some("dirs")).await;

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
    });
}
