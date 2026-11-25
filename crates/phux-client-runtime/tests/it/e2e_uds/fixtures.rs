//! Shared real-server fixtures for the Unix-socket end-to-end tests.

use super::*;

/// Spawn `/bin/cat` in group 1 from a raw `controller` connection, as
/// `request_id`; returns the new pane.
pub(super) async fn controller_spawn_cat(
    controller: &mut UnixStream,
    request_id: u32,
) -> ResourceId {
    let spawn = Spawn {
        initial_size: Some((40, 8)),
        ..Spawn::command(&["/bin/cat"])
    };
    send_frame(controller, &spawn.frame(request_id)).await;
    recv_until(controller, |_, frame| match frame {
        FrameKind::ResourceSpawned {
            request_id: id,
            result: SpawnResult::Ok(terminal),
        } if id == request_id => Some(terminal),
        _ => None,
    })
    .await
}

/// A server seeded with `main` whose organization store lives in `dir`, so a
/// restart is a new incarnation of the same durable authority. Recovery
/// follows `main`'s exact authority and key; a memory store would make the
/// restart a different authority whose reused name a client never follows.
pub(super) fn spawn_durable_server(
    socket: &std::path::Path,
    dir: &std::path::Path,
) -> phux_server_testkit::ServerHandles {
    let store = dir.join("org.v1.sqlite3");
    phux_server_testkit::spawn_server_with(socket.to_path_buf(), Some("main"), move |config| {
        config.org_store = phux_server::OrgStoreLocation::Path(store);
    })
}

/// A real server on a fresh socket, seeded with session `main`.
pub(super) struct Live {
    _tmp: TempDir,
    pub(super) socket: std::path::PathBuf,
    handles: phux_server_testkit::ServerHandles,
}

impl Live {
    pub(super) fn start() -> Self {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let handles = spawn_server(socket.clone(), Some("main"));
        Self {
            _tmp: tmp,
            socket,
            handles,
        }
    }

    /// A runtime client on this server with `options`, once it attached.
    pub(super) async fn attached(&self, options: ClientOptions) -> Client {
        let client = Runtime::connect(Target::uds(&self.socket), options).expect("connect");
        wait_for_status(&client, Status::Attached).await;
        client
    }

    /// Shut the server down and wait for it to exit cleanly.
    pub(super) async fn stop(self) {
        let (shutdown, server) = self.handles;
        drop(shutdown);
        server.await.unwrap().unwrap();
    }
}

/// A Unix-socket relay to `server` that can turn every connection it has
/// already accepted into a black hole: still open, never answering, the
/// half-open link a dead NAT mapping or a stale USB forward leaves behind.
/// Connections accepted after the freeze relay normally.
pub(super) fn freezable_relay(
    server: std::path::PathBuf,
    listen: &std::path::Path,
) -> Arc<AtomicUsize> {
    // Connections with an index below this value are black-holed.
    let frozen_below = Arc::new(AtomicUsize::new(0));
    let listener = UnixListener::bind(listen).unwrap();
    let freeze = Arc::clone(&frozen_below);
    tokio::task::spawn_local(async move {
        let mut index = 0usize;
        while let Ok((client, _)) = listener.accept().await {
            let Ok(upstream) = UnixStream::connect(&server).await else {
                continue;
            };
            let this = index;
            index += 1;
            let (client_read, client_write) = client.into_split();
            let (upstream_read, upstream_write) = upstream.into_split();
            for (mut from, mut to) in [
                (
                    Box::new(client_read) as Box<dyn tokio::io::AsyncRead + Unpin>,
                    Box::new(upstream_write) as Box<dyn tokio::io::AsyncWrite + Unpin>,
                ),
                (Box::new(upstream_read) as _, Box::new(client_write) as _),
            ] {
                let freeze = Arc::clone(&freeze);
                tokio::task::spawn_local(async move {
                    let mut buf = vec![0u8; 16 * 1024];
                    loop {
                        let Ok(n) = from.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        // Frozen: swallow the bytes and keep the socket open.
                        if this < freeze.load(Ordering::SeqCst) {
                            continue;
                        }
                        if to.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        }
    });
    frozen_below
}

/// A listener that answers every WebSocket upgrade with 401: the pairing
/// gate refusing a token, which no retry can change. Returns the port and
/// the count of connections it accepted.
pub(super) async fn refuse_with_401() -> (u16, Arc<AtomicUsize>) {
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
