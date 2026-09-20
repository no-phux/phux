//! The runtime against a real server over a Unix socket: attach, spawn,
//! type, observe the published frame; survive a server restart; fail
//! terminally on a refusal.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use phux_client_runtime::control::{ControlOptions, Event, SpawnRequest, Status};
use phux_client_runtime::reconnect::Ladder;
use phux_client_runtime::{Client, ClientOptions, ConnectOptions, Listener, Runtime, Target};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, SpawnResult, ViewportInfo};
use phux_protocol::{GroupId, ResourceId};
use phux_server_testkit::{recv_until, run_local, send_frame, spawn_server, wait_for_socket};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// Generous, like the testkit's deadlines: these drive real PTYs under a
/// parallel test run, and a genuine hang still fails.
const DEADLINE: Duration = Duration::from_secs(20);

fn options() -> ClientOptions {
    ClientOptions {
        control: ControlOptions {
            client_name: "phux-client-runtime-e2e".to_owned(),
            attach: Some(AttachTarget::ByName("main".to_owned())),
            viewport: (40, 8),
            ..ControlOptions::default()
        },
        connect: ConnectOptions {
            // A local socket comes back in well under a second.
            ladder: Ladder::AGENT_VERB,
            ..ConnectOptions::default()
        },
    }
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_status(client: &Client, status: Status) {
    wait_until(&format!("status {status:?}"), || client.status() == status).await;
}

/// Drain events until one matches, collecting nothing else.
async fn wait_for_event<T>(
    client: &Client,
    what: &str,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    let start = Instant::now();
    loop {
        for event in client.take_events() {
            if let Some(found) = pick(&event) {
                return found;
            }
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn spawn_cat(client: &Client) -> ResourceId {
    let request_id = client.spawn_terminal(SpawnRequest {
        command: Some(vec!["/bin/cat".to_owned()]),
        ..SpawnRequest::default()
    });
    let terminal = wait_for_event(client, "RESOURCE_SPAWNED", |event| match event {
        Event::TerminalSpawned {
            request_id: id,
            terminal_id: Some(terminal_id),
            ..
        } if *id == request_id => Some(terminal_id.clone()),
        Event::TerminalSpawned {
            request_id: id,
            error: Some(error),
            ..
        } if *id == request_id => panic!("spawn failed: {error}"),
        _ => None,
    })
    .await;
    wait_until("the spawned terminal to accept input", || {
        client.input_ready(&terminal)
    })
    .await;
    terminal
}

#[cfg(feature = "engine")]
async fn wait_for_text(client: &Client, terminal: &ResourceId, needle: &str) {
    wait_until(&format!("the frame to show {needle:?}"), || {
        client
            .acquire(terminal)
            .is_some_and(|frame| frame.text().contains(needle))
    })
    .await;
}

#[cfg(not(feature = "engine"))]
async fn wait_for_text(client: &Client, terminal: &ResourceId, needle: &str) {
    let mut seen = Vec::new();
    wait_until(&format!("the output to carry {needle:?}"), || {
        seen.extend(client.take_output(terminal));
        String::from_utf8_lossy(&seen).contains(needle)
    })
    .await;
}

/// Counts wakes; the runtime must never call it while holding a lock, so
/// the callback may call back into the client.
struct Wakes {
    count: AtomicUsize,
}

impl Listener for Wakes {
    fn on_activity(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn attaches_spawns_types_and_observes_the_published_frame() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let client = Runtime::connect(Target::uds(&socket), options()).expect("connect");
        let wakes = Arc::new(Wakes {
            count: AtomicUsize::new(0),
        });
        client.set_listener(wakes.clone());
        wait_for_status(&client, Status::Attached).await;
        assert!(
            wakes.count.load(Ordering::SeqCst) >= 1,
            "the attach woke the listener"
        );
        let woken = wakes.count.load(Ordering::SeqCst);
        // Edge-triggered: nothing more until the consumer drains.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(wakes.count.load(Ordering::SeqCst), woken);
        let _ = client.take_events();
        let server_info = client.server().expect("negotiated");
        assert!(!server_info.id.is_empty());
        let topology = client.topology().expect("topology");
        assert!(topology.session_named("main").is_some());

        let terminal = spawn_cat(&client).await;
        #[cfg(feature = "engine")]
        let before = client.generation(&terminal).expect("published");
        assert!(client.send_text(&terminal, "hello"));
        wait_for_text(&client, &terminal, "hello").await;
        #[cfg(feature = "engine")]
        {
            let frame = client.acquire(&terminal).expect("frame");
            assert!(frame.generation > before, "output advanced the generation");
            assert!(frame.dirty_rows().count() >= 1);
            assert_eq!((frame.cols, frame.rows), (40, 8));
            // An unchanged terminal keeps its generation.
            let settled = client.generation(&terminal).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(client.generation(&terminal), Some(settled));
            let slot = client.slot(&terminal).expect("slot");
            assert_eq!(slot.generation(), settled);
        }

        client.close();
        wait_for_status(&client, Status::Closed).await;
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linear real-server scenario keeps connection-count evidence and cleanup together"
)]
fn switches_sessions_and_picks_up_a_foreign_pane_without_redialing() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let server_socket = tmp.path().join("server.sock");
        let proxy_socket = tmp.path().join("proxy.sock");
        let (shutdown, server) = spawn_server(server_socket.clone(), Some("main"));

        // A second real client creates and owns beta. Its spawn below is
        // foreign to the runtime client, so only ATTACH_RESOURCE can open
        // that pane's stream on the runtime's existing socket.
        let mut controller = wait_for_socket(&server_socket, DEADLINE).await;
        send_frame(
            &mut controller,
            &FrameKind::Attach {
                attach_id: 77,
                target: AttachTarget::CreateIfMissing {
                    name: "beta".to_owned(),
                    command: None,
                    cwd: None,
                },
                viewport: ViewportInfo::new(40, 8),
                request_scrollback: false,
                scrollback_limit_lines: 0,
                role_policy: None,
            },
        )
        .await;
        recv_until(&mut controller, |_, frame| {
            matches!(frame, FrameKind::Attached { .. }).then_some(())
        })
        .await;
        recv_until(&mut controller, |_, frame| {
            matches!(frame, FrameKind::AttachReady { attach_id: 77 }).then_some(())
        })
        .await;
        send_frame(
            &mut controller,
            &FrameKind::SpawnResource {
                request_id: 87,
                group: GroupId::new(1),
                command: Some(vec!["/bin/cat".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                resource: None,
                initial_size: Some((40, 8)),
            },
        )
        .await;
        let beta_live = recv_until(&mut controller, |_, frame| match frame {
            FrameKind::ResourceSpawned {
                request_id: 87,
                result: SpawnResult::Ok(id),
            } => Some(id),
            _ => None,
        })
        .await;

        let dials = Arc::new(AtomicUsize::new(0));
        let proxy_listener = UnixListener::bind(&proxy_socket).unwrap();
        let proxy_dials = Arc::clone(&dials);
        let upstream_path = server_socket.clone();
        let proxy = tokio::task::spawn_local(async move {
            while let Ok((mut downstream, _)) = proxy_listener.accept().await {
                proxy_dials.fetch_add(1, Ordering::SeqCst);
                let Ok(mut upstream) = UnixStream::connect(&upstream_path).await else {
                    continue;
                };
                tokio::task::spawn_local(async move {
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });

        let client = Runtime::connect(Target::uds(&proxy_socket), options()).expect("connect");
        wait_for_status(&client, Status::Attached).await;
        wait_until("both sessions in topology", || {
            client
                .topology()
                .is_some_and(|topology| topology.session_named("beta").is_some())
        })
        .await;
        assert_eq!(dials.load(Ordering::SeqCst), 1);

        client.attach_session(AttachTarget::ByName("beta".to_owned()));
        wait_until("beta's per-terminal stream", || {
            client.selected_session() == Some(2) && client.input_ready(&beta_live)
        })
        .await;
        assert!(client.send_text(&beta_live, "BETA-READY\n"));
        wait_for_text(&client, &beta_live, "BETA-READY").await;
        assert_eq!(dials.load(Ordering::SeqCst), 1, "switch must not redial");

        client.attach_session(AttachTarget::ByName("main".to_owned()));
        wait_until("main selected again", || {
            client.selected_session() == Some(1)
        })
        .await;

        send_frame(
            &mut controller,
            &FrameKind::SpawnResource {
                request_id: 88,
                group: GroupId::new(1),
                command: Some(vec!["/bin/cat".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                resource: None,
                initial_size: Some((40, 8)),
            },
        )
        .await;
        let foreign = recv_until(&mut controller, |_, frame| match frame {
            FrameKind::ResourceSpawned {
                request_id: 88,
                result: SpawnResult::Ok(id),
            } => Some(id),
            _ => None,
        })
        .await;
        wait_until("foreign pane pickup", || client.input_ready(&foreign)).await;
        assert!(client.send_text(&foreign, "FOREIGN-PANE\n"));
        wait_for_text(&client, &foreign, "FOREIGN-PANE").await;
        assert_eq!(dials.load(Ordering::SeqCst), 1, "pickup must not redial");

        client.close();
        proxy.abort();
        drop(controller);
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}

#[test]
fn reconnects_through_the_ladder_after_a_server_restart() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let client = Runtime::connect(Target::uds(&socket), options()).expect("connect");
        wait_for_status(&client, Status::Attached).await;

        drop(shutdown);
        server.await.unwrap().unwrap();
        wait_for_status(&client, Status::Connecting).await;
        let lost = wait_for_event(&client, "ConnectionLost", |event| {
            matches!(event, Event::ConnectionLost { .. }).then_some(())
        })
        .await;
        assert_eq!(lost, ());

        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        wait_for_status(&client, Status::Attached).await;
        assert!(client.attached_once());
        let terminal = spawn_cat(&client).await;
        assert!(client.send_text(&terminal, "back"));
        wait_for_text(&client, &terminal, "back").await;

        client.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}

/// A listener that answers every WebSocket upgrade with 401: the pairing
/// gate refusing a token, which no retry can change. Returns the port and
/// the count of connections it accepted.
async fn refuse_with_401() -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::task::spawn_local(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            // Read the whole upgrade request before answering, so the
            // client never sees a reset instead of the status line.
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buf[..read]),
                }
            }
            let _ = stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            drop(stream);
        }
    });
    (port, accepted)
}

#[test]
fn a_401_refusal_ends_the_session_without_walking_the_ladder() {
    run_local(async {
        let (port, accepted) = refuse_with_401().await;
        let client = Runtime::connect(Target::ws(format!("ws://127.0.0.1:{port}")), options())
            .expect("connect");
        wait_for_status(&client, Status::Failed).await;
        let error = client.last_error().expect("refusal message");
        assert!(
            error.contains("401"),
            "the status stays in the message: {error}"
        );
        // One dial, no retry: the gate saw exactly one connection.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        drop(client);
    });
}
