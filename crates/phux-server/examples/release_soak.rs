//! Opt-in 24-hour release soak using a real server process, PTY, and wire clients.
//! Not included in the default test suite. Short runs require an explicit flag/env.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "standalone operator-facing harness reports failures directly"
)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_protocol::{BootstrapId, PROTOCOL_VERSION, StreamId, TerminalId};
use phux_server::{ServerConfig, ServerRuntime};
use portable_pty::CommandBuilder;
use serde_json::json;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, timeout};

const DAY: Duration = Duration::from_secs(86_400);
const CLIENTS: usize = 8;
const STALLED: usize = 7;
const HISTORY_LINES: u32 = 50_000;

#[derive(Debug)]
struct Config {
    duration: Duration,
    artifacts: PathBuf,
    server_rss_mb: u64,
    client_rss_mb: u64,
    resync_limit: Duration,
    history_latency_limit: Duration,
    convergence_limit: Duration,
    stall: Duration,
}

#[derive(Debug)]
enum Event {
    Output {
        client: usize,
        seq: u64,
    },
    History {
        client: usize,
        page: u64,
        rows: u32,
        latency_ms: u128,
    },
    Bootstrap {
        client: usize,
        latency_ms: u128,
    },
    Reconnect {
        client: usize,
        latency_ms: u128,
    },
    Fault {
        client: usize,
        message: String,
    },
}

#[derive(Debug)]
struct Generation {
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    next_seq: u64,
    next_page: u64,
    history_cursor: Option<bytes::Bytes>,
    history_requested: Option<Instant>,
    started: Instant,
    ready: bool,
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--server-child") {
        return server_child();
    }
    let config = match parse_config() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("release-soak: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = fs::create_dir_all(&config.artifacts) {
        eprintln!("release-soak: cannot create artifact directory: {error}");
        return ExitCode::FAILURE;
    }
    install_failure_hook(config.artifacts.clone());
    let failure_dir = config.artifacts.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("soak runtime");
    let local = tokio::task::LocalSet::new();
    match local.block_on(&runtime, run(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = first_failure(
                &failure_dir,
                Duration::ZERO,
                &error,
                &[0; CLIENTS],
                &[0; CLIENTS],
            );
            eprintln!("release-soak: {error}");
            ExitCode::FAILURE
        }
    }
}

fn install_failure_hook(artifact_dir: PathBuf) {
    std::panic::set_hook(Box::new(move |panic| {
        let path = artifact_dir.join("first-failure.json");
        if let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(path) {
            let _ = serde_json::to_writer_pretty(
                &mut file,
                &json!({"message": panic.to_string(), "kind": "panic"}),
            );
            let _ = file.sync_all();
        }
        eprintln!("release-soak panic: {panic}");
    }));
}

fn parse_config() -> Result<Config, String> {
    let mut duration = env_positive("PHUX_SOAK_DURATION_SECS")?.map_or(DAY, Duration::from_secs);
    let mut artifacts = std::env::var_os("PHUX_SOAK_ARTIFACT_DIR").map(PathBuf::from);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--duration-secs" => {
                duration = Duration::from_secs(positive(
                    "--duration-secs",
                    &args.next().ok_or("missing --duration-secs value")?,
                )?);
            }
            "--artifact-dir" => {
                artifacts = Some(PathBuf::from(
                    args.next().ok_or("missing --artifact-dir value")?,
                ));
            }
            "-h" | "--help" => {
                println!(
                    "release-soak [--duration-secs SECONDS] [--artifact-dir PATH]\ndefault: 86400 seconds; environment equivalents: PHUX_SOAK_DURATION_SECS and PHUX_SOAK_ARTIFACT_DIR"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    let artifacts = artifacts.unwrap_or_else(|| {
        PathBuf::from("target/release-soak").join(format!("{stamp}-{}", std::process::id()))
    });
    let default_stall = duration.as_secs().saturating_div(8).clamp(5, 300);
    Ok(Config {
        duration,
        artifacts,
        server_rss_mb: env_positive("PHUX_SOAK_SERVER_RSS_MB")?.unwrap_or(768),
        client_rss_mb: env_positive("PHUX_SOAK_CLIENT_RSS_MB")?.unwrap_or(512),
        resync_limit: Duration::from_secs(
            env_positive("PHUX_SOAK_RESYNC_LIMIT_SECS")?.unwrap_or(10),
        ),
        history_latency_limit: Duration::from_millis(
            env_positive("PHUX_SOAK_HISTORY_LATENCY_LIMIT_MS")?.unwrap_or(10_000),
        ),
        convergence_limit: Duration::from_secs(
            env_positive("PHUX_SOAK_CONVERGENCE_LIMIT_SECS")?.unwrap_or(5),
        ),
        stall: Duration::from_secs(env_positive("PHUX_SOAK_STALL_SECS")?.unwrap_or(default_stall)),
    })
}

fn env_positive(name: &str) -> Result<Option<u64>, String> {
    std::env::var(name)
        .ok()
        .map(|value| positive(name, &value))
        .transpose()
}
fn positive(name: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{name} must be a positive integer, got {value:?}"))
}

async fn run(config: Config) -> Result<(), String> {
    let runtime_dir = config.artifacts.join("runtime");
    fs::create_dir_all(&runtime_dir).map_err(|error| error.to_string())?;
    // The PTY child may inherit a shell-selected working directory. Keep its
    // readiness sentinel and the server socket independent of that cwd.
    let runtime_dir = fs::canonicalize(runtime_dir).map_err(|error| error.to_string())?;
    write_config_artifact(&config)?;
    let socket = runtime_dir.join("phux.sock");
    let warm = runtime_dir.join("warm.ready");
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let child = Command::new(executable)
        .arg("--server-child")
        .env("PHUX_SOAK_SOCKET", &socket)
        .env("PHUX_SOAK_WARM_READY", &warm)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(
            File::create(config.artifacts.join("server.stdout.log")).map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(
            File::create(config.artifacts.join("server.stderr.log")).map_err(|e| e.to_string())?,
        ))
        .spawn()
        .map_err(|error| format!("spawn server child: {error}"))?;
    let server_pid = child.id();
    let mut server = ChildGuard(child);
    wait_for_path(&socket, Duration::from_secs(15)).await?;
    wait_for_path(&warm, Duration::from_secs(60)).await?;

    let (tasks, mut rx) = spawn_clients(&config, &socket);
    let mut metrics = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.artifacts.join("metrics.jsonl"))
        .map_err(|e| e.to_string())?;
    let mut state = MonitorState::new();
    let result = monitor(
        &config,
        &mut server,
        server_pid,
        &mut rx,
        &mut metrics,
        &mut state,
    )
    .await;
    for task in tasks {
        task.abort();
    }
    let _ = server.0.kill();
    let _ = server.0.wait();
    match &result {
        Err(message) => first_failure(
            &config.artifacts,
            state.started.elapsed(),
            message,
            &state.raw,
            &state.pages,
        )?,
        Ok(()) => write_success(&config, server_pid, &state)?,
    }
    result
}

fn write_config_artifact(config: &Config) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(&json!({
        "duration_secs": config.duration.as_secs(),
        "clients": CLIENTS,
        "stalled_client": STALLED,
        "history_lines": HISTORY_LINES,
        "server_rss_mb": config.server_rss_mb,
        "client_rss_mb": config.client_rss_mb,
        "resync_limit_ms": config.resync_limit.as_millis(),
        "history_latency_limit_ms": config.history_latency_limit.as_millis(),
        "convergence_limit_ms": config.convergence_limit.as_millis(),
        "stall_secs": config.stall.as_secs(),
    }))
    .map_err(|error| error.to_string())?;
    fs::write(config.artifacts.join("config.json"), payload).map_err(|error| error.to_string())
}

fn spawn_clients(
    config: &Config,
    socket: &Path,
) -> (
    Vec<tokio::task::JoinHandle<()>>,
    mpsc::UnboundedReceiver<Event>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut tasks = Vec::with_capacity(CLIENTS);
    for client in 0..CLIENTS {
        let socket = socket.to_owned();
        let tx = tx.clone();
        let stall = config.stall;
        let limit = config.resync_limit;
        tasks.push(tokio::task::spawn_local(async move {
            if let Err(message) = client_loop(client, &socket, stall, limit, tx.clone()).await {
                let _ = tx.send(Event::Fault { client, message });
            }
        }));
    }
    drop(tx);
    (tasks, rx)
}

struct MonitorState {
    started: Instant,
    raw: [u64; CLIENTS],
    pages: [u64; CLIENTS],
    reconnects: [u64; CLIENTS],
    divergent_since: Option<Instant>,
}

impl MonitorState {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            raw: [0; CLIENTS],
            pages: [0; CLIENTS],
            reconnects: [0; CLIENTS],
            divergent_since: None,
        }
    }
}

async fn monitor(
    config: &Config,
    server: &mut ChildGuard,
    server_pid: u32,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    metrics: &mut File,
    state: &mut MonitorState,
) -> Result<(), String> {
    let deadline = state.started + config.duration;
    let mut tick = interval(Duration::from_secs(10));
    loop {
        tokio::select! {
            biased;
            event = rx.recv() => record_event(config, metrics, state, event)?,
            _ = tick.tick() => sample_processes(config, server, server_pid, metrics, state)?,
            () = sleep_until(deadline) => return check_completion(state),
        }
    }
}

fn record_event(
    config: &Config,
    metrics: &mut File,
    state: &mut MonitorState,
    event: Option<Event>,
) -> Result<(), String> {
    match event {
        Some(Event::Output { client, seq }) => state.raw[client] = seq,
        Some(Event::History {
            client,
            page,
            rows,
            latency_ms,
        }) => {
            state.pages[client] = page;
            json_line(
                metrics,
                &json!({"elapsed_ms":state.started.elapsed().as_millis(),"kind":"history","client":client,"page":page,"rows":rows,"latency_ms":latency_ms}),
            )?;
            if latency_ms > config.history_latency_limit.as_millis() {
                return Err(format!(
                    "client {client} history page latency {latency_ms}ms exceeds {:?}",
                    config.history_latency_limit,
                ));
            }
        }
        Some(Event::Bootstrap { client, latency_ms }) => json_line(
            metrics,
            &json!({"elapsed_ms":state.started.elapsed().as_millis(),"kind":"resync","client":client,"latency_ms":latency_ms}),
        )?,
        Some(Event::Reconnect { client, latency_ms }) => {
            state.reconnects[client] += 1;
            json_line(
                metrics,
                &json!({"elapsed_ms":state.started.elapsed().as_millis(),"kind":"reconnect","client":client,"latency_ms":latency_ms}),
            )?;
        }
        Some(Event::Fault { client, message }) => {
            return Err(format!("client {client}: {message}"));
        }
        None => return Err("all client monitors exited before the deadline".to_owned()),
    }
    Ok(())
}

fn sample_processes(
    config: &Config,
    server: &mut ChildGuard,
    server_pid: u32,
    metrics: &mut File,
    state: &mut MonitorState,
) -> Result<(), String> {
    if let Some(status) = server.0.try_wait().map_err(|e| e.to_string())? {
        return Err(format!("server exited early with {status}"));
    }
    let server_rss = rss_kib(server_pid)?;
    let client_rss = rss_kib(std::process::id())?;
    json_line(
        metrics,
        &json!({"elapsed_ms":state.started.elapsed().as_millis(),"kind":"sample","server_rss_kib":server_rss,"client_rss_kib":client_rss,"raw_seq":state.raw,"history_page":state.pages,"reconnects":state.reconnects}),
    )?;
    if server_rss > config.server_rss_mb * 1024 {
        return Err(format!(
            "server RSS {server_rss} KiB exceeds {} MiB",
            config.server_rss_mb,
        ));
    }
    if client_rss > config.client_rss_mb * 1024 {
        return Err(format!(
            "client RSS {client_rss} KiB exceeds {} MiB",
            config.client_rss_mb,
        ));
    }
    let min = state.raw[..STALLED].iter().copied().min().unwrap_or(0);
    let max = state.raw[..STALLED].iter().copied().max().unwrap_or(0);
    if min > 0 && max.saturating_sub(min) > 1_024 {
        let since = state.divergent_since.get_or_insert_with(Instant::now);
        if since.elapsed() > config.convergence_limit {
            return Err(format!(
                "raw sequence spread {} failed to converge within {:?}",
                max - min,
                config.convergence_limit,
            ));
        }
    } else {
        state.divergent_since = None;
    }
    Ok(())
}

fn check_completion(state: &MonitorState) -> Result<(), String> {
    if state.raw[..STALLED].contains(&0) {
        return Err("one or more active clients observed no live raw output".to_owned());
    }
    if state.pages[..STALLED].contains(&0) {
        return Err("one or more active history caches received no page".to_owned());
    }
    if state.reconnects[STALLED] < 2 {
        return Err("stalled history client did not complete reconnect recovery".to_owned());
    }
    Ok(())
}

fn write_success(config: &Config, server_pid: u32, state: &MonitorState) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(&json!({
        "duration_secs": config.duration.as_secs(),
        "raw_seq": state.raw,
        "history_page": state.pages,
        "reconnects": state.reconnects,
        "server_pid": server_pid,
    }))
    .map_err(|error| error.to_string())?;
    fs::write(config.artifacts.join("success.json"), payload).map_err(|error| error.to_string())
}

async fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        sleep(deadline - now).await;
    }
}

async fn client_loop(
    client: usize,
    socket: &Path,
    stall: Duration,
    resync_limit: Duration,
    events: mpsc::UnboundedSender<Event>,
) -> Result<(), String> {
    let client_number = u64::try_from(client).expect("eight clients fit in u64");
    let reconnect_every = Duration::from_secs(600 + client_number * 13);
    loop {
        let began = Instant::now();
        let (mut stream, mut state) = connect(client, socket).await?;
        let latency = began.elapsed();
        if latency > resync_limit {
            return Err(format!("initial/reconnect bootstrap took {latency:?}"));
        }
        events
            .send(Event::Reconnect {
                client,
                latency_ms: latency.as_millis(),
            })
            .map_err(|_| "event sink closed".to_owned())?;
        request_history(&mut stream, &mut state).await;
        if client == STALLED {
            // A real server-side socket and history lease are deliberately left
            // unread; dropping it after the stall proves reconnect recovery.
            sleep(stall).await;
            drop(stream);
            continue;
        }
        let reconnect_at = Instant::now() + reconnect_every;
        loop {
            let remaining = reconnect_at.saturating_duration_since(Instant::now());
            let received = timeout(remaining, common::try_recv_typed(&mut stream)).await;
            let Some((_, frame)) = (match received {
                Ok(frame) => frame,
                Err(_) => break,
            }) else {
                return Err("server closed transport".to_owned());
            };
            handle_frame(
                client,
                &mut stream,
                &mut state,
                frame,
                resync_limit,
                &events,
            )
            .await?;
        }
    }
}

async fn connect(client: usize, socket: &Path) -> Result<(UnixStream, Generation), String> {
    let mut stream = common::wait_for_raw_socket(socket, Duration::from_secs(15)).await;
    let native = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    );
    common::send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: format!("phux-release-soak-{client}"),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new().with_bootstrap(native),
        },
    )
    .await;
    let (_, hello) = common::recv_typed(&mut stream).await;
    if !matches!(
        hello,
        FrameKind::HelloOk {
            selected_profile: BootstrapProfile::NativeState { .. },
            ..
        }
    ) {
        return Err(format!("expected native HELLO_OK, got {hello:?}"));
    }
    common::send_frame(
        &mut stream,
        &FrameKind::Attach {
            attach_id: u32::try_from(client).expect("eight clients fit in u32") + 1,
            target: AttachTarget::ByName("soak".to_owned()),
            viewport: ViewportInfo::new(200, 60),
            request_scrollback: true,
            scrollback_limit_lines: HISTORY_LINES,
        },
    )
    .await;
    let mut terminal = None;
    let mut state = None;
    loop {
        let (_, frame) = common::recv_typed(&mut stream).await;
        match frame {
            FrameKind::Attached { snapshot, .. } => {
                terminal = snapshot.panes.first().map(|pane| pane.id.clone());
            }
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                base_seq,
                ..
            } => {
                state = Some(Generation {
                    terminal_id: terminal.clone().ok_or("BOOTSTRAP_BEGIN before ATTACHED")?,
                    stream_id,
                    bootstrap_id,
                    next_seq: base_seq.checked_add(1).ok_or("base sequence overflow")?,
                    next_page: 1,
                    history_cursor: None,
                    history_requested: None,
                    started: Instant::now(),
                    ready: false,
                });
            }
            FrameKind::BootstrapChunk { .. } => {}
            FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                history_cursor,
                ..
            } => {
                let current = state.as_mut().ok_or("READY before BEGIN")?;
                if current.stream_id != stream_id || current.bootstrap_id != bootstrap_id {
                    return Err("READY generation mismatch".to_owned());
                }
                current.history_cursor = history_cursor;
                current.ready = true;
            }
            FrameKind::AttachReady { .. } if state.as_ref().is_some_and(|value| value.ready) => {
                return Ok((stream, state.unwrap()));
            }
            other => return Err(format!("unexpected attach frame {other:?}")),
        }
    }
}

async fn request_history(stream: &mut UnixStream, state: &mut Generation) {
    if let Some(cursor) = state.history_cursor.clone() {
        state.history_requested = Some(Instant::now());
        common::send_frame(
            stream,
            &FrameKind::HistoryRequest {
                terminal_id: state.terminal_id.clone(),
                stream_id: state.stream_id,
                bootstrap_id: state.bootstrap_id,
                cursor,
                max_bytes: 256 * 1024,
                max_rows: 512,
            },
        )
        .await;
    }
}

async fn handle_frame(
    client: usize,
    stream: &mut UnixStream,
    state: &mut Generation,
    frame: FrameKind,
    resync_limit: Duration,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<(), String> {
    match frame {
        frame @ FrameKind::TerminalOutput { .. } => {
            handle_output_frame(client, state, &frame, events)?;
        }
        frame @ FrameKind::HistoryPage { .. } => {
            handle_history_frame(client, stream, state, frame, events).await?;
        }
        FrameKind::BootstrapTombstone {
            stream_id,
            bootstrap_id,
            ..
        } => {
            if stream_id != state.stream_id || bootstrap_id != state.bootstrap_id {
                return Err("tombstone in wrong generation".to_owned());
            }
            state.ready = false;
            state.started = Instant::now();
        }
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            base_seq,
            ..
        } => {
            if state.ready {
                return Err("replacement BEGIN without tombstone".to_owned());
            }
            state.terminal_id = terminal_id;
            state.stream_id = stream_id;
            state.bootstrap_id = bootstrap_id;
            state.next_seq = base_seq.checked_add(1).ok_or("base sequence overflow")?;
            state.next_page = 1;
            state.history_cursor = None;
            state.history_requested = None;
            state.started = Instant::now();
        }
        FrameKind::BootstrapChunk {
            stream_id,
            bootstrap_id,
            ..
        } => {
            if stream_id != state.stream_id || bootstrap_id != state.bootstrap_id {
                return Err("chunk in wrong generation".to_owned());
            }
        }
        FrameKind::BootstrapReady {
            stream_id,
            bootstrap_id,
            history_cursor,
            ..
        } => {
            if stream_id != state.stream_id || bootstrap_id != state.bootstrap_id {
                return Err("replacement READY mismatch".to_owned());
            }
            let latency = state.started.elapsed();
            if latency > resync_limit {
                return Err(format!("stuck resync: READY took {latency:?}"));
            }
            state.ready = true;
            state.history_cursor = history_cursor;
            events
                .send(Event::Bootstrap {
                    client,
                    latency_ms: latency.as_millis(),
                })
                .map_err(|_| "event sink closed".to_owned())?;
        }
        FrameKind::HistoryRejected { .. }
        | FrameKind::HistoryTombstone { .. }
        | FrameKind::AttachReady { .. } => {}
        FrameKind::Error { code, message, .. } => {
            return Err(format!("server error {code:?}: {message}"));
        }
        other => return Err(format!("unexpected steady-state frame {other:?}")),
    }
    Ok(())
}

fn handle_output_frame(
    client: usize,
    state: &mut Generation,
    frame: &FrameKind,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<(), String> {
    let FrameKind::TerminalOutput {
        stream_id,
        bootstrap_id,
        seq,
        ..
    } = frame
    else {
        return Err("output handler received a non-output frame".to_owned());
    };
    let stream_id = *stream_id;
    let bootstrap_id = *bootstrap_id;
    let seq = *seq;
    if stream_id != state.stream_id || bootstrap_id != state.bootstrap_id {
        return Err("raw output in wrong generation".to_owned());
    }
    if seq != state.next_seq {
        let kind = if seq < state.next_seq {
            "duplicate/reordered"
        } else {
            "gap"
        };
        return Err(format!(
            "raw {kind}: expected {}, received {seq}",
            state.next_seq
        ));
    }
    state.next_seq = seq.checked_add(1).ok_or("raw sequence overflow")?;
    events
        .send(Event::Output { client, seq })
        .map_err(|_| "event sink closed".to_owned())
}

async fn handle_history_frame(
    client: usize,
    stream: &mut UnixStream,
    state: &mut Generation,
    frame: FrameKind,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<(), String> {
    let FrameKind::HistoryPage {
        stream_id,
        bootstrap_id,
        page_seq,
        next_cursor,
        rows,
        ..
    } = frame
    else {
        return Err("history handler received a non-history frame".to_owned());
    };
    if stream_id != state.stream_id || bootstrap_id != state.bootstrap_id {
        return Err("history in wrong generation".to_owned());
    }
    if page_seq != state.next_page {
        return Err(format!(
            "history duplicate/gap/reorder: expected {}, received {page_seq}",
            state.next_page
        ));
    }
    let latency_ms = state
        .history_requested
        .take()
        .ok_or("history page arrived without an outstanding request")?
        .elapsed()
        .as_millis();
    events
        .send(Event::History {
            client,
            page: page_seq,
            rows,
            latency_ms,
        })
        .map_err(|_| "event sink closed".to_owned())?;
    state.next_page += 1;
    if let Some(cursor) = next_cursor {
        state.history_requested = Some(Instant::now());
        common::send_frame(
            stream,
            &FrameKind::HistoryRequest {
                terminal_id: state.terminal_id.clone(),
                stream_id: state.stream_id,
                bootstrap_id: state.bootstrap_id,
                cursor,
                max_bytes: 256 * 1024,
                max_rows: 512,
            },
        )
        .await;
    }
    Ok(())
}

fn server_child() -> ExitCode {
    let Some(socket) = std::env::var_os("PHUX_SOAK_SOCKET").map(PathBuf::from) else {
        return ExitCode::from(2);
    };
    let Some(warm) = std::env::var_os("PHUX_SOAK_WARM_READY").map(PathBuf::from) else {
        return ExitCode::from(2);
    };
    let script = format!(
        "i=1; while [ $i -le {HISTORY_LINES} ]; do printf 'warm-%08d α界\\r\\n' \"$i\"; i=$((i+1)); done; printf '\\033[?1049h\\033[?2026h\\033[2J\\033[1;1H\\033[38;2;40;200;120mFULL-SCREEN-SOAK\\033[0m\\033[?2026l'; : > '{}'; i=1; while :; do printf '\\033[2;1Hlive-%012d\\r\\n' \"$i\"; i=$((i+1)); sleep 0.02; done",
        warm.display()
    );
    let mut command = CommandBuilder::new("/bin/sh");
    command.arg("-c");
    command.arg(script);
    let config = ServerConfig {
        socket_path: socket,
        pre_seeded_session: Some("soak".to_owned()),
        seed_with_pty: true,
        seed_command: Some(command),
        history_limit: HISTORY_LINES,
        ..ServerConfig::with_default_socket()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("server runtime");
    let local = tokio::task::LocalSet::new();
    let parent_closed = async {
        let _ = tokio::task::spawn_blocking(|| {
            let mut stdin = std::io::stdin();
            let mut byte = [0_u8; 1];
            loop {
                match stdin.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await;
    };
    match local.block_on(
        &runtime,
        ServerRuntime::new(config).run_async(parent_closed),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soak server child: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn wait_for_path(path: &Path, deadline: Duration) -> Result<(), String> {
    let began = Instant::now();
    while began.elapsed() < deadline {
        if path.exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(20)).await;
    }
    Err(format!(
        "{} did not appear within {deadline:?}",
        path.display()
    ))
}

fn rss_kib(pid: u32) -> Result<u64, String> {
    if let Ok(text) = fs::read_to_string(format!("/proc/{pid}/status"))
        && let Some(line) = text.lines().find(|line| line.starts_with("VmRSS:"))
    {
        return line
            .split_whitespace()
            .nth(1)
            .ok_or("VmRSS missing value")?
            .parse::<u64>()
            .map_err(|e| e.to_string());
    }
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .map_err(|e| format!("sample RSS: {e}"))?;
    if !output.status.success() {
        return Err(format!("ps failed for pid {pid}"));
    }
    String::from_utf8(output.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .parse::<u64>()
        .map_err(|e| e.to_string())
}

fn json_line(file: &mut File, value: &serde_json::Value) -> Result<(), String> {
    serde_json::to_writer(&mut *file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())
}

fn first_failure(
    dir: &Path,
    elapsed: Duration,
    message: &str,
    raw: &[u64],
    pages: &[u64],
) -> Result<(), String> {
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("first-failure.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    file.write_all(&serde_json::to_vec_pretty(&json!({"elapsed_ms":elapsed.as_millis(),"message":message,"raw_seq":raw,"history_page":pages})).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}
