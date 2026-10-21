//! The opted-in reply push (docs/chief.md) over the production dispatch
//! loop: a `stop` that carries `data.reply` reaches an absent device's
//! gateway with its text; an ordinary `stop` sends nothing.

use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, Scope, SpawnResource, SpawnResult,
};
use phux_server_testkit::{Spawn, command, send_frame, spawn_resource};
use portable_pty::CommandBuilder;
use tokio::net::UnixStream;

use crate::common::{Server, attach, get_metadata, sh, spawned};

const FOREVER: &str = "while :; do sleep 3600; done";

async fn start() -> (Server, UnixStream) {
    let server = Server::start(Some("demo"), |cfg| {
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.args(["-c", FOREVER]);
        phux_server_testkit::seed_pty(cfg, seed);
    });
    let mut owner = server.connect().await;
    attach(&mut owner, "demo").await;
    (server, owner)
}

async fn session(stream: &mut UnixStream) -> phux_protocol::ids::ResourceId {
    let parent = spawned(stream, 1, sh(FOREVER)).await;
    let spawn = Spawn {
        resource: Some(Box::new(SpawnResource::agent_session(parent, "pi"))),
        ..Spawn::default()
    };
    match spawn_resource(stream, 2, spawn).await {
        SpawnResult::Ok(session) => session,
        other => panic!("spawn session failed: {other:?}"),
    }
}

async fn append(
    stream: &mut UnixStream,
    request_id: u32,
    session: &phux_protocol::ids::ResourceId,
    records: &str,
) {
    let append = Command::AppendResourceOutput {
        terminal_id: session.clone(),
        bytes: records.as_bytes().to_vec(),
    };
    let result = command(stream, request_id, append).await;
    assert!(
        matches!(result, CommandResult::OkWith(_)),
        "append: {result:?}"
    );
}

/// Read one HTTP request from `listener` and answer 202; the raw request text.
fn capture_one_post(listener: std::net::TcpListener) -> std::thread::JoinHandle<String> {
    use std::io::{Read, Write};
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        conn.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("timeout");
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = conn.read(&mut buf).expect("read");
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if body.len() >= len || n == 0 {
                    break;
                }
            }
        }
        conn.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("reply");
        String::from_utf8_lossy(&raw).into_owned()
    })
}

/// docs/chief.md: a `stop` carrying `data.reply` pushes that text (bounded)
/// to a device whose grant-writing connection is gone; an ordinary stop
/// pushes nothing.
#[test]
fn a_stop_with_a_reply_pushes_its_text_to_an_absent_device() {
    phux_server_testkit::run_local(async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let captured = capture_one_post(listener);
        let (server, mut owner) = start().await;

        let key = "phux.push/v1/phone-1";
        let grant = format!(
            r#"{{"gateway":"http://127.0.0.1:{port}/v1/push","grant":"0123456789abcdef","host":"mac"}}"#
        );
        let mut phone = server.connect().await;
        send_frame(
            &mut phone,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: key.to_owned(),
                value: grant.clone().into_bytes(),
            },
        )
        .await;
        assert_eq!(
            get_metadata(&mut phone, 2, Scope::Global, key).await,
            Some(grant.into_bytes()),
            "the grant landed"
        );
        drop(phone);

        let session = session(&mut owner).await;
        // The phone's departure is observed asynchronously; wait until a
        // plain stop would no longer count it as live.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        append(&mut owner, 3, &session, "{\"type\":\"stop\",\"data\":{}}\n").await;
        append(
            &mut owner,
            4,
            &session,
            "{\"type\":\"stop\",\"data\":{\"turn_id\":\"run-9\",\"reply\":\"Opened PR #12.\",\"reply_title\":\"Chief\"}}\n",
        )
        .await;

        let request = tokio::task::spawn_blocking(move || captured.join().expect("gateway"))
            .await
            .expect("join");
        assert!(
            request.contains("Authorization: Bearer 0123456789abcdef\r\n"),
            "{request}"
        );
        let body = request.split("\r\n\r\n").nth(1).expect("body");
        let json: serde_json::Value = serde_json::from_str(body).expect("json");
        assert_eq!(json["kind"], "reply", "{json}");
        assert_eq!(json["text"], "Opened PR #12.");
        assert_eq!(json["title"], "Chief");
        assert_eq!(json["question"], "run-9");
        assert_eq!(json["host"], "mac");

        drop(owner);
        server.stop().await;
    });
}
