//! Production-UDS acceptance and measurements for screen-poll connection reuse.
//!
//! The ordinary test proves that a wait reconnects after a real socket loss
//! while retaining its deadline. The ignored experiment compares the unchanged
//! one-shot `get_screen_scrollback` helper with the production persistent wait
//! path against a real `ServerRuntime` and PTY-backed Terminal.
//!
//! ```text
//! CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
//!   --test polling_reuse polling_reuse_measurement_matrix \
//!   -- --ignored --nocapture --test-threads=1
//! ```

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "production-path measurement harness"
)]
#![allow(clippy::future_not_send, reason = "ServerRuntime owns LocalSet actors")]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use phux_client::send_keys::send_to;
use phux_client::snapshot::get_screen_scrollback;
use phux_client::wait::{Condition, WaitOutcome, poll_until};
use phux_perf::ProcessStats;
use phux_protocol::ResourceId;
use phux_server::{ServerConfig, ServerRuntime};
use tempfile::TempDir;
use tokio::io::copy_bidirectional;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

const TERMINAL: ResourceId = ResourceId::local(1);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const IDLE_RUN: Duration = Duration::from_millis(750);
const CHANGE_DELAY: Duration = Duration::from_millis(400);
const CHANGE_DEADLINE: Duration = Duration::from_secs(3);
const START_DEADLINE: Duration = Duration::from_secs(10);
const JOIN_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
enum Strategy {
    OneShot,
    Persistent,
}

impl Strategy {
    const fn label(self) -> &'static str {
        match self {
            Self::OneShot => "one-shot",
            Self::Persistent => "persistent",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Workload {
    Idle,
    Changing,
}

impl Workload {
    const fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Changing => "changing",
        }
    }
}

#[derive(Debug)]
struct AgentResult {
    polls: u32,
    elapsed: Duration,
    outcome: WaitOutcome,
}

struct Fixture {
    _dir: TempDir,
    backend: PathBuf,
    frontend: PathBuf,
    accepted: Arc<AtomicUsize>,
    shutdown: oneshot::Sender<()>,
    server: JoinHandle<Result<(), phux_server::ServerError>>,
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
        let proxy = tokio::task::spawn_local(async move {
            serve_counting_proxy(listener, proxy_backend, proxy_count, drop_first_after).await;
        });

        // Warm the PTY-backed screen before process-rusage measurement. This
        // direct backend read is deliberately outside the counted proxy.
        get_screen_scrollback(&backend, TERMINAL, None, false)
            .await
            .expect("warm production GET_SCREEN");

        Self {
            _dir: dir,
            backend,
            frontend,
            accepted,
            shutdown,
            server,
            proxy,
        }
    }

    async fn stop(self) {
        self.proxy.abort();
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
) {
    loop {
        let (frontend, _) = listener.accept().await.expect("accept polling connection");
        let sequence = accepted.fetch_add(1, Ordering::Relaxed);
        let backend = backend.clone();
        tokio::task::spawn_local(async move {
            proxy_connection(
                frontend,
                &backend,
                (sequence == 0).then_some(drop_first_after).flatten(),
            )
            .await;
        });
    }
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

async fn persistent_agent(socket: &Path, workload: Workload, marker: &str) -> AgentResult {
    let condition = Condition::Contains(marker.to_owned());
    let budget = match workload {
        Workload::Idle => IDLE_RUN,
        Workload::Changing => CHANGE_DEADLINE,
    };
    let start = Instant::now();
    let result = poll_until(socket, TERMINAL, &condition, Some(budget), POLL_INTERVAL)
        .await
        .expect("persistent production wait");
    AgentResult {
        polls: result.polls,
        elapsed: start.elapsed(),
        outcome: result.outcome,
    }
}

async fn one_shot_agent(socket: &Path, workload: Workload, marker: &str) -> AgentResult {
    let budget = match workload {
        Workload::Idle => IDLE_RUN,
        Workload::Changing => CHANGE_DEADLINE,
    };
    let start = Instant::now();
    let mut polls = 0_u32;
    loop {
        let screen = get_screen_scrollback(socket, TERMINAL, None, false)
            .await
            .expect("one-shot production GET_SCREEN");
        polls = polls.saturating_add(1);
        if screen
            .unwrapped_rows()
            .iter()
            .any(|line| line.contains(marker))
        {
            return AgentResult {
                polls,
                elapsed: start.elapsed(),
                outcome: WaitOutcome::Met,
            };
        }
        let Some(remaining) = budget.checked_sub(start.elapsed()) else {
            return AgentResult {
                polls,
                elapsed: start.elapsed(),
                outcome: WaitOutcome::TimedOut,
            };
        };
        sleep(POLL_INTERVAL.min(remaining)).await;
    }
}

async fn mutate_terminal_after(socket: PathBuf, marker: String) {
    sleep(CHANGE_DELAY).await;
    send_to(
        &socket,
        TERMINAL,
        &[format!("printf {marker}"), "Enter".to_owned()],
    )
    .await
    .expect("write marker through production ROUTE_INPUT");
}

async fn run_case(strategy: Strategy, workload: Workload, agents: usize) {
    let fixture = Fixture::start(None).await;
    let marker = format!("PHUX_POLL_REUSE_{}_{}", workload.label(), agents);
    let mutation = matches!(workload, Workload::Changing).then(|| {
        tokio::task::spawn_local(mutate_terminal_after(
            fixture.backend.clone(),
            marker.clone(),
        ))
    });
    let cpu_before = ProcessStats::capture().expect("getrusage before case");
    let wall_start = Instant::now();

    let mut tasks = Vec::with_capacity(agents);
    for _ in 0..agents {
        let socket = fixture.frontend.clone();
        let marker = marker.clone();
        tasks.push(tokio::task::spawn_local(async move {
            match strategy {
                Strategy::OneShot => one_shot_agent(&socket, workload, &marker).await,
                Strategy::Persistent => persistent_agent(&socket, workload, &marker).await,
            }
        }));
    }
    let mut results = Vec::with_capacity(agents);
    for task in tasks {
        results.push(task.await.expect("polling agent joined"));
    }
    if let Some(task) = mutation {
        task.await.expect("terminal mutator joined");
    }

    let wall = wall_start.elapsed();
    let cpu = ProcessStats::capture()
        .expect("getrusage after case")
        .delta(&cpu_before);
    let connections = fixture.accepted.load(Ordering::Relaxed);
    let polls: u32 = results.iter().map(|result| result.polls).sum();
    let max_latency = results
        .iter()
        .map(|result| result.elapsed)
        .max()
        .unwrap_or_default();
    let expected = match workload {
        Workload::Idle => WaitOutcome::TimedOut,
        Workload::Changing => WaitOutcome::Met,
    };
    assert!(results.iter().all(|result| result.outcome == expected));
    assert_eq!(
        connections,
        match strategy {
            Strategy::OneShot => usize::try_from(polls).expect("poll count fits usize"),
            Strategy::Persistent => agents,
        },
        "the counting proxy observes the actual production client connections"
    );
    println!(
        "{{\"strategy\":\"{}\",\"workload\":\"{}\",\"agents\":{},\"polls\":{},\"connections\":{},\"cpu_us\":{},\"wall_us\":{},\"max_detection_us\":{}}}",
        strategy.label(),
        workload.label(),
        agents,
        polls,
        connections,
        cpu.cpu_total_us(),
        wall.as_micros(),
        max_latency.as_micros(),
    );
    fixture.stop().await;
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

#[test]
#[ignore = "loaded-host production-UDS CPU and connection measurement"]
fn polling_reuse_measurement_matrix() {
    run_local(async {
        for workload in [Workload::Idle, Workload::Changing] {
            for agents in [1, 8, 32] {
                run_case(Strategy::OneShot, workload, agents).await;
                run_case(Strategy::Persistent, workload, agents).await;
            }
        }
    });
}
