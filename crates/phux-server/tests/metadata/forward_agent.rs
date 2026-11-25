//! ssh-agent forwarding over the real sockets: the bit, the broker socket
//! and a pane's `SSH_AUTH_SOCK` all follow `[forward] agent`; a pane's
//! list and sign reach the offering client; and a
//! client that disconnects mid-request fails it at once.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::forward::ForwardOp;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, ErrorCode, FrameKind};
use phux_server::LaunchCommand;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use phux_server_testkit::{
    SpawnedServer, assert_command_error, command, recv_until, run_local, seed_pty,
};

use crate::common::{advertises, connect_as, full_caps};

const FAILURE: &[u8] = &[5];
const LIST: &[u8] = &[11];
/// Far below the server's 60 s request deadline: "at once".
const PROMPT: Duration = Duration::from_secs(5);

fn broker(socket: &Path) -> PathBuf {
    socket.with_file_name("phux.sock.ssh-agent")
}

/// Run `body` against a server whose seeded pane records the
/// `SSH_AUTH_SOCK` it was started with in `env`.
async fn serve(agent: bool, body: impl AsyncFnOnce(&Path, PathBuf)) {
    let tmp = TempDir::new().unwrap();
    let env = tmp.path().join("env");
    let mut seed = LaunchCommand::new("/bin/sh");
    seed.arg("-c");
    seed.arg(format!(
        "printf %s \"${{SSH_AUTH_SOCK-unset}}\" > '{0}.tmp'; mv '{0}.tmp' '{0}'; sleep 600",
        env.display()
    ));
    // What the server's own environment names is what a pane inherits.
    seed.env("SSH_AUTH_SOCK", "/the/servers/own/agent");
    let server = SpawnedServer::start_in(tmp, Some("main"), |cfg| {
        cfg.forward.agent = agent;
        seed_pty(cfg, seed);
    });
    body(&server.socket, env).await;
    server.stop().await;
}

async fn client(socket: &Path, name: &str) -> (UnixStream, bool) {
    let (stream, hello_ok) = connect_as(socket, name, full_caps()).await;
    (
        stream,
        advertises(&hello_ok, ServerFeatureExt::ForwardAgent),
    )
}

async fn offer(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    let op = ForwardOp::AgentOffer;
    command(stream, request_id, Command::Forward { op }).await
}

async fn reply(stream: &mut UnixStream, request_id: u32, id: u64, message: &[u8]) -> CommandResult {
    let message = message.to_vec();
    let op = ForwardOp::AgentReply { id, message };
    command(stream, request_id, Command::Forward { op }).await
}

/// The next `agent_request` this client is sent.
async fn request(stream: &mut UnixStream) -> (u64, Vec<u8>) {
    recv_until(stream, |_, frame| match frame {
        FrameKind::Event {
            event: AgentEvent::SshAgentRequest { id, message },
            stamp: None,
            ..
        } => Some((id, message)),
        _ => None,
    })
    .await
}

/// The pane the next `agent_request` names, and the request's id.
async fn request_from(stream: &mut UnixStream) -> (Option<ResourceId>, u64) {
    recv_until(stream, |_, frame| match frame {
        FrameKind::Event {
            terminal,
            event: AgentEvent::SshAgentRequest { id, .. },
            ..
        } => Some((terminal, id)),
        _ => None,
    })
    .await
}

async fn pane(socket: &Path) -> UnixStream {
    UnixStream::connect(broker(socket)).await.unwrap()
}

async fn send(pane: &mut UnixStream, message: &[u8]) {
    let mut framed = u32::try_from(message.len()).unwrap().to_be_bytes().to_vec();
    framed.extend(message);
    pane.write_all(&framed).await.unwrap();
}

/// The pane's next answer, which must arrive within [`PROMPT`].
async fn answer(pane: &mut UnixStream) -> Vec<u8> {
    tokio::time::timeout(PROMPT, async {
        let len = pane.read_u32().await.unwrap();
        let mut body = vec![0; usize::try_from(len).unwrap()];
        pane.read_exact(&mut body).await.unwrap();
        body
    })
    .await
    .expect("the broker did not answer promptly")
}

async fn seeded_env(env: &Path) -> String {
    for _ in 0..500 {
        if let Ok(value) = std::fs::read_to_string(env) {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the seeded pane never recorded its environment");
}

#[test]
fn off_by_default_changes_nothing_for_panes() {
    run_local(serve(false, async |socket, env| {
        let (mut stream, advertised) = client(socket, "agent-off").await;
        assert!(!advertised);
        let CommandResult::Error { code, .. } = offer(&mut stream, 1).await else {
            panic!("an offer was accepted with forwarding off");
        };
        assert_eq!(code, ErrorCode::Unsupported);
        assert!(!broker(socket).exists());
        assert_eq!(seeded_env(&env).await, "/the/servers/own/agent");
    }));
}

#[test]
fn on_a_pane_reaches_the_offering_client_and_only_with_list_or_sign() {
    use std::os::unix::fs::PermissionsExt as _;
    run_local(serve(true, async |socket, env| {
        let (mut stream, advertised) = client(socket, "agent-on").await;
        assert!(advertised);
        let path = broker(socket);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(seeded_env(&env).await, path.to_str().unwrap());

        // Nobody offers: an immediate failure, on a connection that stays
        // usable.
        let mut pane = pane(socket).await;
        send(&mut pane, LIST).await;
        assert_eq!(answer(&mut pane).await, FAILURE);

        assert_eq!(offer(&mut stream, 1).await, CommandResult::Ok);
        send(&mut pane, LIST).await;
        let (first, message) = request(&mut stream).await;
        assert_eq!(message, LIST);
        let identities = [12, 0, 0, 0, 0];
        assert_eq!(
            reply(&mut stream, 2, first, &identities).await,
            CommandResult::Ok
        );
        assert_eq!(answer(&mut pane).await, identities);
        // Answered once: the id is spent.
        let again = reply(&mut stream, 3, first, &identities).await;
        assert_command_error(&again, ErrorCode::NotFound);

        // The next request the client sees is the sign, with the next id.
        let sign = [13, 0, 0, 0, 1, b'k', 0, 0, 0, 1, b'd', 0, 0, 0, 0];
        send(&mut pane, &sign).await;
        let (second, message) = request(&mut stream).await;
        assert_eq!((second, message.as_slice()), (first + 1, &sign[..]));
        assert_eq!(
            reply(&mut stream, 4, second, &[14, 1, 2]).await,
            CommandResult::Ok
        );
        assert_eq!(answer(&mut pane).await, [14, 1, 2]);
    }));
}

/// The ordering this feature leans on: a connection that ends with a
/// request in flight withdraws its offer as it ends, so the pane's ssh is
/// answered then, not at the 60 s deadline, and the next request fails at
/// once instead of going to a dead mailbox.
#[test]
fn a_client_that_disconnects_mid_request_fails_it_at_once() {
    run_local(serve(true, async |socket, _env| {
        let (mut stream, _) = client(socket, "agent-gone").await;
        assert_eq!(offer(&mut stream, 1).await, CommandResult::Ok);
        let mut pane = pane(socket).await;
        send(&mut pane, LIST).await;
        request(&mut stream).await;
        drop(stream);
        assert_eq!(answer(&mut pane).await, FAILURE);
        send(&mut pane, LIST).await;
        assert_eq!(answer(&mut pane).await, FAILURE);

        // A new offer serves the same socket: nothing about the path a
        // pane holds changed.
        let (mut stream, _) = client(socket, "agent-back").await;
        assert_eq!(offer(&mut stream, 1).await, CommandResult::Ok);
        send(&mut pane, LIST).await;
        let (id, _) = request(&mut stream).await;
        assert_eq!(reply(&mut stream, 2, id, &[12, 0]).await, CommandResult::Ok);
        assert_eq!(answer(&mut pane).await, [12, 0]);
    }));
}

/// A request names the pane whose process made it: one the pane's shell
/// runs, and one that moved to its own session but still has the shell for
/// an ancestor. One from outside every pane names none.
#[test]
fn a_request_names_the_pane_it_came_from() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let env = tmp.path().join("env");
        let list = "import os,socket;s=socket.socket(socket.AF_UNIX);\
                    s.connect(os.environ['SSH_AUTH_SOCK']);\
                    s.sendall(bytes([0,0,0,1,11]));s.recv(64)";
        let detach = format!("import os;os.setsid() if os.getpgrp()!=os.getpid() else None;{list}");
        let mut seed = LaunchCommand::new("/bin/sh");
        seed.arg("-c");
        seed.arg(format!(
            "while [ ! -e '{0}' ]; do sleep 0.05; done; \
             python3 -c \"{list}\"; (python3 -c \"{detach}\"); sleep 600",
            env.display()
        ));
        let server = SpawnedServer::start_in(tmp, Some("main"), |cfg| {
            cfg.forward.agent = true;
            seed_pty(cfg, seed);
        });
        let (mut stream, _) = client(&server.socket, "agent-pane").await;
        assert_eq!(offer(&mut stream, 1).await, CommandResult::Ok);
        let mut outside = pane(&server.socket).await;
        send(&mut outside, LIST).await;
        assert_eq!(request_from(&mut stream).await.0, None);
        std::fs::write(&env, b"go").unwrap();
        for turn in 0..2 {
            let (from, id) = request_from(&mut stream).await;
            assert!(matches!(from, Some(ResourceId::Local { .. })), "{from:?}");
            let answered = reply(&mut stream, 2 + turn, id, &[12, 0, 0, 0, 0]).await;
            assert_eq!(answered, CommandResult::Ok);
        }
        server.stop().await;
    });
}
