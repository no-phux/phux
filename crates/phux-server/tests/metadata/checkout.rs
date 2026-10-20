//! `CHECKOUT` over the real socket: the checkout is the one the Terminal's
//! process is in, answers are typed, and paths outside it are refused.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::path::Path;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::checkout::{
    CheckoutAnswer, CheckoutArea, CheckoutRefusalCode, CheckoutRequest, CheckoutStatus,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{command, join_after_shutdown, run_local, spawn_server_with_seed_cmd};

use crate::common::{attach_pane, connect_as, full_caps};

const SESSION: &str = "checkout";

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repo(root: &Path) -> std::path::PathBuf {
    let dir = root.join("repo");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.email", "test@example.com"),
        ("user.name", "Test"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&dir, &["config", key, value]);
    }
    std::fs::write(dir.join("src/lib.rs"), "one\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    std::fs::write(dir.join("src/lib.rs"), "one\ntwo\n").unwrap();
    std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();
    dir.canonicalize().unwrap()
}

async fn checkout(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: &ResourceId,
    request: CheckoutRequest,
) -> CheckoutAnswer {
    let result = command(
        stream,
        request_id,
        Command::Checkout {
            terminal_id: terminal_id.clone(),
            request,
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::Checkout(answer)) = result else {
        panic!("CHECKOUT must answer OkWith(Checkout): {result:?}");
    };
    *answer
}

fn status(answer: CheckoutAnswer) -> CheckoutStatus {
    let CheckoutAnswer::Status(status) = answer else {
        panic!("expected status: {answer:?}");
    };
    status
}

fn has(status: &CheckoutStatus, area: CheckoutArea, path: &str) -> bool {
    status
        .entries
        .iter()
        .any(|entry| entry.area == area && entry.path == path)
}

async fn assert_stage_round_trip(stream: &mut UnixStream, pane: &ResourceId) {
    let staged = status(
        checkout(
            stream,
            3,
            pane,
            CheckoutRequest::Stage {
                paths: vec!["new.txt".to_owned()],
            },
        )
        .await,
    );
    assert!(has(&staged, CheckoutArea::Staged, "new.txt"));
    let unstaged = status(
        checkout(
            stream,
            4,
            pane,
            CheckoutRequest::Unstage {
                paths: vec!["new.txt".to_owned()],
            },
        )
        .await,
    );
    assert!(has(&unstaged, CheckoutArea::Untracked, "new.txt"));
}

async fn assert_outside_paths_refused(stream: &mut UnixStream, pane: &ResourceId) {
    for (request_id, path) in [(7, "../outside"), (8, "/etc/passwd"), (9, ".git/config")] {
        let refused = checkout(
            stream,
            request_id,
            pane,
            CheckoutRequest::Read {
                path: path.to_owned(),
                max_bytes: 0,
            },
        )
        .await;
        assert!(
            matches!(&refused, CheckoutAnswer::Refused(r) if r.code == CheckoutRefusalCode::InvalidPath),
            "{path}: {refused:?}"
        );
    }
}

#[test]
fn checkout_methods_read_and_stage_the_terminals_checkout() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let repo = repo(tmp.path());
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 30");
        cmd.cwd(repo.join("src"));
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), SESSION, cmd);

        let (mut stream, hello) = connect_as(&socket, "checkout-test", full_caps()).await;
        let FrameKind::HelloOk { server_caps, .. } = hello else {
            panic!("expected HELLO_OK");
        };
        assert!(
            server_caps
                .features_ext
                .contains(ServerFeatureExt::Checkout)
        );
        let pane = attach_pane(&mut stream, SESSION).await;

        let before = status(checkout(&mut stream, 1, &pane, CheckoutRequest::Status).await);
        assert_eq!(before.root, repo.to_str().unwrap());
        assert_eq!(before.branch.as_deref(), Some("main"));
        let lib = before
            .entries
            .iter()
            .find(|entry| entry.area == CheckoutArea::Unstaged && entry.path == "src/lib.rs")
            .expect("unstaged src/lib.rs");
        assert_eq!((lib.added, lib.deleted), (Some(1), Some(0)));
        assert!(has(&before, CheckoutArea::Untracked, "new.txt"));

        let diff = checkout(
            &mut stream,
            2,
            &pane,
            CheckoutRequest::Diff {
                path: "src/lib.rs".to_owned(),
                area: CheckoutArea::Unstaged,
            },
        )
        .await;
        assert!(
            matches!(&diff, CheckoutAnswer::Diff(diff) if diff.text.contains("+two")),
            "{diff:?}"
        );

        assert_stage_round_trip(&mut stream, &pane).await;

        let file = checkout(
            &mut stream,
            5,
            &pane,
            CheckoutRequest::Read {
                path: "new.txt".to_owned(),
                max_bytes: 0,
            },
        )
        .await;
        assert!(matches!(&file, CheckoutAnswer::File(file) if file.text == "fresh\n"));

        let log = checkout(
            &mut stream,
            6,
            &pane,
            CheckoutRequest::Log {
                skip: 0,
                limit: 10,
                path: None,
            },
        )
        .await;
        assert!(
            matches!(&log, CheckoutAnswer::Log(log) if log.commits.len() == 1 && !log.has_more)
        );

        assert_outside_paths_refused(&mut stream, &pane).await;

        let missing = command(
            &mut stream,
            10,
            Command::Checkout {
                terminal_id: ResourceId::local(9_999),
                request: CheckoutRequest::Status,
            },
        )
        .await;
        assert!(
            matches!(
                missing,
                CommandResult::Error {
                    code: ErrorCode::TerminalNotFound,
                    ..
                }
            ),
            "{missing:?}"
        );

        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_terminal_outside_any_checkout_is_a_typed_refusal() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 30");
        cmd.cwd(&plain);
        // A fresh temp directory is under no checkout.
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), SESSION, cmd);
        let (mut stream, _) = connect_as(&socket, "checkout-none", full_caps()).await;
        let pane = attach_pane(&mut stream, SESSION).await;

        let answer = checkout(&mut stream, 1, &pane, CheckoutRequest::Branches).await;
        assert!(
            matches!(&answer, CheckoutAnswer::Refused(r) if r.code == CheckoutRefusalCode::NotARepository),
            "{answer:?}"
        );

        join_after_shutdown(shutdown, server).await;
    });
}
