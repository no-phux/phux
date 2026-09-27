//! Production-UDS acceptance for screen-poll connection reuse: a wait
//! reconnects after a real socket loss while retaining its deadline.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "production-path test harness"
)]
#![allow(clippy::future_not_send, reason = "ServerRuntime owns LocalSet actors")]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use phux_client::snapshot::get_screen_scrollback;
use phux_client::wait::{Condition, WaitOutcome, poll_until};
use phux_protocol::ResourceId;
use phux_server::{ServerConfig, ServerRuntime};
use tempfile::TempDir;
use tokio::io::copy_bidirectional;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, sleep, timeout};

const TERMINAL: ResourceId = ResourceId::local(1);
const START_DEADLINE: Duration = Duration::from_secs(10);
const JOIN_DEADLINE: Duration = Duration::from_secs(10);

struct Fixture {
    _dir: TempDir,
    frontend: PathBuf,
    accepted: Arc<AtomicUsize>,
    shutdown: oneshot::Sender<()>,
    server: JoinHandle<Result<(), phux_server::ServerError>>,
    proxy_stop: oneshot::Sender<()>,
    proxy: JoinHandle<()>,
}

impl Fixture {
    async fn start(drop_first_after: Option<Duration>) -> Self {
        let dir = tempfile::tempdir().expect("fixture tempdir");
        let backend = dir.path().join("server.sock");
        let frontend = dir.path().join("counted.sock");
        let listener = UnixListener::bind(&frontend).expect("bind counting UDS proxy");
        let accepted = Arc::new(AtomicUsize::new(0));

        let (shutdown, stop) = oneshot::channel();
        let config = ServerConfig {
            socket_path: backend.clone(),
            pre_seeded_session: Some("polling-reuse".to_owned()),
            seed_with_pty: true,
            ..ServerConfig::with_default_socket()
        };
        let server = tokio::task::spawn_local(async move {
            ServerRuntime::new(config)
                .run_async(async move {
                    let _ = stop.await;
                })
                .await
        });
        wait_for_server(&backend).await;

        let proxy_backend = backend.clone();
        let proxy_count = Arc::clone(&accepted);
        let (proxy_stop, proxy_stopped) = oneshot::channel();
        let proxy = tokio::task::spawn_local(async move {
            serve_counting_proxy(
                listener,
                proxy_backend,
                proxy_count,
                drop_first_after,
                proxy_stopped,
            )
            .await;
        });

        // Warm the PTY-backed screen outside the counted proxy.
        get_screen_scrollback(&backend, TERMINAL, None, false)
            .await
            .expect("warm production GET_SCREEN");

        Self {
            _dir: dir,
            frontend,
            accepted,
            shutdown,
            server,
            proxy_stop,
            proxy,
        }
    }

    async fn stop(self) {
        let _ = self.proxy_stop.send(());
        timeout(JOIN_DEADLINE, self.proxy)
            .await
            .expect("proxy stopped before deadline")
            .expect("proxy and connection pumps joined");
        let _ = self.shutdown.send(());
        timeout(JOIN_DEADLINE, self.server)
            .await
            .expect("ServerRuntime stopped before deadline")
            .expect("ServerRuntime task joined")
            .expect("ServerRuntime stopped cleanly");
    }
}

async fn wait_for_server(path: &Path) {
    let deadline = Instant::now() + START_DEADLINE;
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => {
                drop(stream);
                return;
            }
            Err(_error) if Instant::now() < deadline => sleep(Duration::from_millis(5)).await,
            Err(error) => panic!("server socket did not appear: {error}"),
        }
    }
}

async fn serve_counting_proxy(
    listener: UnixListener,
    backend: PathBuf,
    accepted: Arc<AtomicUsize>,
    drop_first_after: Option<Duration>,
    mut stopped: oneshot::Receiver<()>,
) {
    let mut pumps = JoinSet::new();
    loop {
        let (frontend, _) = tokio::select! {
            _ = &mut stopped => break,
            _ = pumps.join_next(), if !pumps.is_empty() => continue,
            accepted = listener.accept() => accepted.expect("accept polling connection"),
        };
        let sequence = accepted.fetch_add(1, Ordering::Relaxed);
        let backend = backend.clone();
        pumps.spawn_local(async move {
            proxy_connection(
                frontend,
                &backend,
                (sequence == 0).then_some(drop_first_after).flatten(),
            )
            .await;
        });
    }
    pumps.shutdown().await;
}

async fn proxy_connection(
    mut frontend: UnixStream,
    backend_path: &Path,
    drop_after: Option<Duration>,
) {
    let mut backend = UnixStream::connect(backend_path)
        .await
        .expect("connect proxy to ServerRuntime");
    let pump = copy_bidirectional(&mut frontend, &mut backend);
    if let Some(after) = drop_after {
        let _ = timeout(after, pump).await;
    } else {
        let _ = pump.await;
    }
}

fn run_local(future: impl Future<Output = ()>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    tokio::task::LocalSet::new().block_on(&runtime, future);
}

#[test]
fn persistent_wait_reconnects_once_and_retains_its_deadline() {
    run_local(async {
        let fixture = Fixture::start(Some(Duration::from_millis(60))).await;
        let started = Instant::now();
        let result = poll_until(
            &fixture.frontend,
            TERMINAL,
            &Condition::Idle(Duration::from_millis(100)),
            Some(Duration::from_secs(2)),
            Duration::from_millis(150),
        )
        .await
        .expect("wait recovers from one transport loss");
        assert_eq!(result.outcome, WaitOutcome::Met);
        assert!(result.polls >= 2, "idle needs repeated completed reads");
        assert_eq!(
            fixture.accepted.load(Ordering::Relaxed),
            2,
            "one initial connection plus one bounded recovery"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        fixture.stop().await;
    });
}
