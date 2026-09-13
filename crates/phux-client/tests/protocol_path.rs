//! Production-path protocol measurements for real PTYs over shaped QUIC.
//!
//! This is intentionally an ignored experiment rather than a CI latency gate.
//! Run the bounded smoke with:
//!
//! ```text
//! CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
//!   --test protocol_path shaped_quic_protocol_path_matrix \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Comma-separated `PHUX_PROTOCOL_PATH_TERMINALS`,
//! `PHUX_PROTOCOL_PATH_RTT_MS`, `PHUX_PROTOCOL_PATH_LOSS_PERCENT`, and
//! `PHUX_PROTOCOL_PATH_MBIT` values select a Cartesian matrix. Every case owns
//! one `ServerRuntime`, one UDP shaper, and one production `Connection`; all
//! Terminal streams in the case therefore share one QUIC connection.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "measurement harness"
)]
#![allow(
    clippy::print_stderr,
    reason = "the experiment emits machine-readable results"
)]
#![allow(clippy::future_not_send, reason = "ServerRuntime owns LocalSet actors")]

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_client::attach::connection::Connection;
use phux_client::attach::{CertTrust, QuicDial};
use phux_config::ConnectorConfigEntry;
use phux_perf::PerfReport;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfileKind, BootstrapProfileSet, ClientCapabilities,
    ColorSupport, LayerSet, OutputMode, ServerFeature,
};
use phux_protocol::ids::{BootstrapId, FileUploadId, ResourceId, StreamId};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, FrameKind, MAX_FILE_UPLOAD_CHUNK,
    SpawnResult, ViewportInfo,
};
use phux_relay::{BoundRelay, RelayConfig, RelayRuntime, cert_fingerprint};
use phux_server::{DEFAULT_GROUP_ID, ServerConfig, ServerRuntime};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const CASE_DEADLINE: Duration = Duration::from_secs(120);
const STEP_DEADLINE: Duration = Duration::from_secs(30);
const DEFAULT_TERMINALS: &[u64] = &[8];
const DEFAULT_RTT_MS: &[u64] = &[0];
const DEFAULT_LOSS_PERCENT: &[f64] = &[0.0];
const DEFAULT_MBIT: &[f64] = &[30.0];
const DEFAULT_SAMPLES: usize = 7;
const DEFAULT_FLOOD_BYTES: usize = 256 * 1024;
const UPLOAD_RTT_MS: u64 = 50;
const UPLOAD_FULL_BYTES: usize = MAX_FILE_UPLOAD_CHUNK;
const UPLOAD_THIN_BYTES: usize = 256 * 1024;
const UPLOAD_CHUNK_KIB: &[u64] = &[16, 64, 256, 8192];
const UPLOAD_MBIT: &[f64] = &[10.0];
const ROUTE_NAME: &str = "protocol-measure";
const TUNNEL_TOKEN: [u8; 32] = [0x11; 32];
const CONSUMER_TOKEN: [u8; 32] = [0x22; 32];
const STALL_LINE_BYTES: usize = b"STALL-00000000-abcdefghijklmnopqrstuvwxyz-0123456789\n".len();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Direct,
    Relay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WireMode {
    Raw,
    StateSync,
}

impl std::str::FromStr for WireMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "raw" => Ok(Self::Raw),
            "state-sync" => Ok(Self::StateSync),
            _ => Err(format!("mode must be raw or state-sync, got {value:?}")),
        }
    }
}

impl std::str::FromStr for Route {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "direct" => Ok(Self::Direct),
            "relay" => Ok(Self::Relay),
            _ => Err(format!("route must be direct or relay, got {value:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Case {
    route: Route,
    mode: WireMode,
    terminals: usize,
    rtt_ms: u64,
    loss_percent: f64,
    mbit: f64,
    samples: usize,
    flood_bytes: usize,
}

#[derive(Debug, Default)]
struct Traffic {
    frames: u64,
    bootstrap_frames: u64,
    bootstrap_bytes: u64,
    output_frames: u64,
    output_bytes: u64,
    acks_sent: u64,
    tombstones: u64,
    tails: HashMap<ResourceId, Vec<u8>>,
}

#[derive(Debug)]
struct CaseResult {
    case: Case,
    profile: String,
    multistream: bool,
    ready_us: LatencySummary,
    echo_us: LatencySummary,
    control_us: LatencySummary,
    flood_terminals: usize,
    flood_requested_bytes: u64,
    traffic: Traffic,
    cancellation_us: u64,
    shaper_metrics: String,
    server_perf: String,
}

#[derive(Debug)]
struct LatencySummary {
    samples: usize,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

#[derive(Clone, Copy, Debug)]
struct UploadCase {
    configured_chunk_bytes: usize,
    file_bytes: usize,
    mbit: f64,
}

#[derive(Debug)]
struct UploadResult {
    case: UploadCase,
    actual_max_chunk_bytes: usize,
    chunks: usize,
    upload_us: u64,
    goodput_bps: u64,
    send_us: LatencySummary,
    chunk_ack_us: LatencySummary,
    echo_us: LatencySummary,
    control_us: LatencySummary,
    rss_before_bytes: u64,
    rss_after_bytes: u64,
    server_shutdown_us: u64,
    shaper_metrics: String,
    server_perf: String,
}

struct Attachment {
    ids: Vec<ResourceId>,
    bound_streams: HashMap<ResourceId, StreamId>,
}

struct RouteFixture {
    relay: Option<Relay>,
    target_addr: SocketAddr,
    server_name: String,
    token: Option<Vec<u8>>,
    trust: CertTrust,
    connectors: Vec<ConnectorConfigEntry>,
    consumer_tokens: Option<PathBuf>,
}

struct Server {
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<Result<(), phux_server::ServerError>>>,
}

struct Relay {
    stop: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<Result<(), phux_relay::RelayError>>>,
}

impl Relay {
    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).ok();
        timeout(STEP_DEADLINE, self.handle.take().unwrap())
            .await
            .expect("relay cancellation timed out")
            .expect("relay task panicked")
            .expect("relay shutdown failed");
    }
}

impl Server {
    async fn stop(mut self) -> u64 {
        let started = Instant::now();
        self.shutdown.take().unwrap().send(()).ok();
        timeout(STEP_DEADLINE, self.handle.take().unwrap())
            .await
            .expect("server cancellation timed out")
            .expect("server task panicked")
            .expect("server shutdown failed");
        micros(started.elapsed())
    }
}

struct Shaper {
    child: Option<Child>,
    metrics: PathBuf,
    addr: SocketAddr,
}

impl Shaper {
    fn stop(mut self) -> String {
        terminate_child(self.child.as_mut().unwrap());
        let status = self
            .child
            .take()
            .unwrap()
            .wait()
            .expect("wait for UDP shaper");
        assert!(
            status.success(),
            "UDP shaper invalidated the experiment: {status}"
        );
        let metrics = std::fs::read_to_string(&self.metrics).expect("read UDP shaper metrics");
        validate_shaper_metrics(&metrics);
        metrics
    }
}

impl Drop for Shaper {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            terminate_child(child);
            let _ = child.wait();
        }
    }
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn tls(cert: &Path, key: &Path, consumer_tokens: Option<&Path>) -> Self {
        phux_server::transport::tls::ensure_self_signed(cert, key)
            .expect("provision direct QUIC certificate fixture");
        let mut previous = vec![
            ("PHUX_WS_TLS_CERT", std::env::var_os("PHUX_WS_TLS_CERT")),
            ("PHUX_WS_TLS_KEY", std::env::var_os("PHUX_WS_TLS_KEY")),
        ];
        // Every test in this integration binary must run serially, so no peer
        // thread can observe these process-wide listener overrides.
        unsafe {
            std::env::set_var("PHUX_WS_TLS_CERT", cert);
            std::env::set_var("PHUX_WS_TLS_KEY", key);
            if let Some(tokens) = consumer_tokens {
                previous.push(("PHUX_WS_TOKENS", std::env::var_os("PHUX_WS_TOKENS")));
                std::env::set_var("PHUX_WS_TOKENS", tokens);
            }
        }
        Self { previous }
    }

    fn with_upload_dir(mut self, path: &Path) -> Self {
        self.previous
            .push(("PHUX_UPLOAD_DIR", std::env::var_os("PHUX_UPLOAD_DIR")));
        // This integration binary is required to run serially.
        unsafe { std::env::set_var("PHUX_UPLOAD_DIR", path) };
        self
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.previous {
            // See construction: this binary has no concurrent test.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

fn terminate_child(child: &Child) {
    let status = ProcessCommand::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM to UDP shaper");
    assert!(status.success(), "could not SIGTERM UDP shaper");
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn validate_shaper_metrics(raw: &str) {
    let metrics: serde_json::Value = serde_json::from_str(raw).expect("parse UDP shaper metrics");
    for direction in ["upstream", "downstream"] {
        let values = metrics
            .get(direction)
            .unwrap_or_else(|| panic!("shaper metrics missing {direction}"));
        for counter in ["queue_drops", "shutdown_drops"] {
            assert_eq!(
                values.get(counter).and_then(serde_json::Value::as_u64),
                Some(0),
                "UDP shaper {direction} {counter} invalidated the experiment"
            );
        }
    }
}

fn metric_value(raw: &str, name: &str) -> Option<u64> {
    let report: serde_json::Value = serde_json::from_str(raw).ok()?;
    report
        .get("metrics")?
        .as_array()?
        .iter()
        .find_map(|metric| {
            (metric.get("name")?.as_str()? == name)
                .then(|| metric.get("value")?.as_u64())
                .flatten()
        })
}

fn shaper_value(raw: &str, direction: &str, name: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(raw)
        .expect("parse UDP shaper metrics")
        .get(direction)
        .and_then(|values| values.get(name))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("shaper metrics missing {direction}.{name}"))
}

fn summarize(mut samples: Vec<u64>) -> LatencySummary {
    assert!(!samples.is_empty(), "cannot summarize zero samples");
    samples.sort_unstable();
    // Nearest-rank: rank=ceil(p*N), clamped to the non-empty sample. In
    // particular, p95/p99 of two observations is the maximum, not the minimum.
    let percentile = |value: usize| {
        let rank = samples.len().saturating_mul(value).div_ceil(100).max(1);
        samples[rank - 1]
    };
    LatencySummary {
        samples: samples.len(),
        p50: percentile(50),
        p95: percentile(95),
        p99: percentile(99),
        max: *samples.last().unwrap(),
    }
}

fn emit_result(result: &CaseResult) {
    let emit_latency = |name: &str, values: &LatencySummary| {
        eprintln!(
            "protocol_path_{name}_us={{samples:{},p50:{},p95:{},p99:{},max:{}}}",
            values.samples, values.p50, values.p95, values.p99, values.max
        );
    };
    eprintln!(
        "protocol_path_case={{route:{:?},mode:{:?},terminals:{},rtt_ms:{},loss_percent:{},mbit:{},flood_bytes:{},profile:{},multistream:{}}}",
        result.case.route,
        result.case.mode,
        result.case.terminals,
        result.case.rtt_ms,
        result.case.loss_percent,
        result.case.mbit,
        result.case.flood_bytes,
        result.profile,
        result.multistream,
    );
    emit_latency("ready", &result.ready_us);
    emit_latency("echo", &result.echo_us);
    emit_latency("control", &result.control_us);
    eprintln!(
        "protocol_path_traffic={{frames:{},bootstrap_frames:{},bootstrap_bytes:{},output_frames:{},output_bytes:{},acks_sent:{},tombstones:{}}}",
        result.traffic.frames,
        result.traffic.bootstrap_frames,
        result.traffic.bootstrap_bytes,
        result.traffic.output_frames,
        result.traffic.output_bytes,
        result.traffic.acks_sent,
        result.traffic.tombstones,
    );
    eprintln!(
        "protocol_path_flood={{terminals:{},requested_bytes:{}}}",
        result.flood_terminals, result.flood_requested_bytes
    );
    eprintln!("protocol_path_cancellation_us={}", result.cancellation_us);
    eprintln!("protocol_path_shaper={}", result.shaper_metrics);
    eprintln!("protocol_path_server_perf={}", result.server_perf);
}

fn free_udp_addr() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("reserve UDP port");
    socket.local_addr().expect("read UDP port")
}

fn parse_list<T>(name: &str, defaults: &[T]) -> Vec<T>
where
    T: std::str::FromStr + Clone,
    T::Err: std::fmt::Display,
{
    let Ok(raw) = std::env::var(name) else {
        return defaults.to_vec();
    };
    raw.split(',')
        .map(|value| {
            value
                .trim()
                .parse()
                .unwrap_or_else(|err| panic!("invalid {name} value {value:?}: {err}"))
        })
        .collect()
}

fn selected_cases() -> Vec<Case> {
    let routes = parse_list("PHUX_PROTOCOL_PATH_ROUTES", &[Route::Direct]);
    let modes = parse_list("PHUX_PROTOCOL_PATH_MODES", &[WireMode::Raw]);
    let terminals = parse_list("PHUX_PROTOCOL_PATH_TERMINALS", DEFAULT_TERMINALS);
    let rtts = parse_list("PHUX_PROTOCOL_PATH_RTT_MS", DEFAULT_RTT_MS);
    let losses = parse_list("PHUX_PROTOCOL_PATH_LOSS_PERCENT", DEFAULT_LOSS_PERCENT);
    let rates = parse_list("PHUX_PROTOCOL_PATH_MBIT", DEFAULT_MBIT);
    let samples = parse_list("PHUX_PROTOCOL_PATH_SAMPLES", &[DEFAULT_SAMPLES])[0];
    let flood_bytes = parse_list("PHUX_PROTOCOL_PATH_FLOOD_BYTES", &[DEFAULT_FLOOD_BYTES])[0];
    let mut cases = Vec::new();
    for &route in &routes {
        for &mode in &modes {
            for &terminal_count in &terminals {
                for &rtt_ms in &rtts {
                    for &loss_percent in &losses {
                        for &mbit in &rates {
                            cases.push(Case {
                                route,
                                mode,
                                terminals: usize::try_from(terminal_count)
                                    .expect("terminal count fits usize"),
                                rtt_ms,
                                loss_percent,
                                mbit,
                                samples,
                                flood_bytes,
                            });
                        }
                    }
                }
            }
        }
    }
    cases
}

#[allow(
    clippy::float_cmp,
    reason = "the upload matrix permits exact named rates only"
)]
fn selected_upload_cases() -> Vec<UploadCase> {
    let chunks = parse_list("PHUX_PROTOCOL_UPLOAD_CHUNK_KIB", UPLOAD_CHUNK_KIB);
    let rates = parse_list("PHUX_PROTOCOL_UPLOAD_MBIT", UPLOAD_MBIT);
    let mut cases = Vec::new();
    for chunk_kib in chunks {
        assert!(
            matches!(chunk_kib, 16 | 64 | 256 | 8192),
            "upload chunk KiB must be 16, 64, 256, or 8192"
        );
        for &mbit in &rates {
            assert!(
                matches!(mbit, 0.3 | 3.0 | 10.0),
                "upload rate must be 0.3, 3, or 10 Mbit/s"
            );
            cases.push(UploadCase {
                configured_chunk_bytes: usize::try_from(chunk_kib).unwrap() * 1024,
                file_bytes: if mbit == 10.0 {
                    UPLOAD_FULL_BYTES
                } else {
                    UPLOAD_THIN_BYTES
                },
                mbit,
            });
        }
    }
    cases
}

fn current_rss_bytes() -> u64 {
    let output = ProcessCommand::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("read current process RSS");
    assert!(output.status.success(), "ps failed while reading RSS");
    let kib: u64 = String::from_utf8(output.stdout)
        .expect("ps RSS is UTF-8")
        .trim()
        .parse()
        .expect("ps RSS is numeric KiB");
    kib.saturating_mul(1024)
}

fn upload_goodput_bps(bytes: usize, elapsed_us: u64) -> u64 {
    let bits = u128::try_from(bytes).unwrap().saturating_mul(8);
    let per_second = bits
        .saturating_mul(1_000_000)
        .checked_div(u128::from(elapsed_us.max(1)))
        .unwrap();
    u64::try_from(per_second).unwrap_or(u64::MAX)
}

fn emit_upload_result(result: &UploadResult) {
    let scenario = if result.case.configured_chunk_bytes == UPLOAD_FULL_BYTES {
        "legal-wire-ceiling"
    } else {
        "harness-proposal"
    };
    let rss_delta = i128::from(result.rss_after_bytes) - i128::from(result.rss_before_bytes);
    eprintln!(
        "protocol_upload_case={{scenario:{scenario},configured_chunk_bytes:{},actual_max_chunk_bytes:{},file_bytes:{},chunks:{},rtt_ms:{UPLOAD_RTT_MS},mbit:{}}}",
        result.case.configured_chunk_bytes,
        result.actual_max_chunk_bytes,
        result.case.file_bytes,
        result.chunks,
        result.case.mbit,
    );
    eprintln!(
        "protocol_upload_result={{upload_us:{},goodput_bps:{},rss_before_bytes:{},rss_after_bytes:{},rss_delta_bytes:{rss_delta},server_shutdown_us:{}}}",
        result.upload_us,
        result.goodput_bps,
        result.rss_before_bytes,
        result.rss_after_bytes,
        result.server_shutdown_us,
    );
    eprintln!("protocol_upload_send_us={:?}", result.send_us);
    eprintln!("protocol_upload_chunk_ack_us={:?}", result.chunk_ack_us);
    eprintln!("protocol_upload_echo_us={:?}", result.echo_us);
    eprintln!("protocol_upload_control_us={:?}", result.control_us);
    eprintln!("protocol_upload_shaper={}", result.shaper_metrics);
    eprintln!("protocol_upload_server_perf={}", result.server_perf);
}

#[allow(
    clippy::float_cmp,
    reason = "the measurement matrix permits exact named cases only"
)]
fn validate_case(case: Case) {
    assert!(
        matches!(case.terminals, 1 | 8 | 32),
        "terminals must be 1, 8, or 32"
    );
    assert!(matches!(case.rtt_ms, 0 | 50 | 150 | 300), "unsupported RTT");
    assert!(
        matches!(case.loss_percent, 0.0 | 1.0 | 3.0),
        "unsupported loss"
    );
    assert!(
        matches!(case.mbit, 0.3 | 3.0 | 30.0),
        "unsupported payload rate"
    );
    assert!(case.samples > 0, "at least one sample is required");
}

fn spawn_server(
    socket: PathBuf,
    quic_addr: Option<SocketAddr>,
    connectors: Vec<ConnectorConfigEntry>,
) -> Server {
    let (shutdown, stopped) = oneshot::channel();
    let config = ServerConfig {
        socket_path: socket,
        pre_seeded_session: Some("measure".to_owned()),
        seed_with_pty: true,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let mut runtime = ServerRuntime::new(config).connectors(connectors, None);
        if let Some(addr) = quic_addr {
            runtime = runtime.listen_quic(addr);
        }
        runtime
            .run_async(async move {
                let _ = stopped.await;
            })
            .await
    });
    Server {
        shutdown: Some(shutdown),
        handle: Some(handle),
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut encoded, byte| {
        write!(encoded, "{byte:02x}").expect("write to String");
        encoded
    })
}

#[cfg(unix)]
fn write_secret(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, contents).expect("write credential fixture");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("set credential fixture owner-only");
}

fn spawn_relay(dir: &Path) -> (Relay, SocketAddr, String, ConnectorConfigEntry, PathBuf) {
    let route_tokens = dir.join("relay-tokens");
    write_secret(
        &route_tokens,
        &format!("{} {ROUTE_NAME}\n", hex(&TUNNEL_TOKEN)),
    );
    let connector_token = dir.join("connector-token");
    write_secret(&connector_token, &format!("{}\n", hex(&TUNNEL_TOKEN)));
    let consumer_tokens = dir.join("consumer-tokens");
    write_secret(&consumer_tokens, &format!("{}\n", hex(&CONSUMER_TOKEN)));
    phux_server::auth::migrate_legacy_store(&consumer_tokens)
        .expect("migrate consumer credential fixture");
    let cert = dir.join("relay-cert.pem");
    let bound: BoundRelay = RelayRuntime::new(RelayConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        cert_path: cert.clone(),
        key_path: dir.join("relay-key.pem"),
        tokens_path: route_tokens,
        max_conns: 16,
    })
    .bind()
    .expect("bind production relay");
    let addr = bound.local_addr();
    let fingerprint = cert_fingerprint(&cert).expect("relay fingerprint");
    let (stop, stopped) = oneshot::channel();
    let handle = tokio::spawn(async move {
        bound
            .serve(async move {
                let _ = stopped.await;
            })
            .await
    });
    let connector = ConnectorConfigEntry {
        relay: addr.to_string(),
        token_file: Some(connector_token),
        cert_fingerprint: Some(fingerprint.clone()),
    };
    (
        Relay {
            stop: Some(stop),
            handle: Some(handle),
        },
        addr,
        fingerprint,
        connector,
        consumer_tokens,
    )
}

async fn wait_for_uds(path: &Path) -> Connection {
    let deadline = Instant::now() + STEP_DEADLINE;
    loop {
        match Connection::connect(path).await {
            Ok(connection) => return connection,
            Err(err) if Instant::now() < deadline => {
                let _ = err;
                sleep(Duration::from_millis(10)).await;
            }
            Err(err) => panic!("server UDS did not become ready: {err}"),
        }
    }
}

async fn spawn_flood_panes(connection: &mut Connection, count: usize) {
    for index in 1..count {
        let frame = FrameKind::SpawnResource {
            request_id: u32::try_from(index).expect("terminal count fits request id"),
            group: DEFAULT_GROUP_ID,
            command: Some(vec!["/bin/sh".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: Some((80, 24)),
            resource: None,
        };
        let (answer, interleaved) = connection
            .request_spawn(&frame)
            .await
            .expect("spawn request transport")
            .into_parts();
        // SPAWN_RESOURCE auto-subscribes its caller, so the production server
        // can push this new pane's bootstrap before RESOURCE_SPAWNED. This is
        // setup traffic, not a measured attach; consume it explicitly rather
        // than pretending the reply was ack-first.
        drop(interleaved);
        assert!(
            matches!(answer, Ok(SpawnResult::Ok(_))),
            "spawn failed: {answer:?}"
        );
    }
}

async fn start_shaper(dir: &Path, case: Case, listen: SocketAddr, target: SocketAddr) -> Shaper {
    let ready = dir.join("shaper.ready");
    let metrics = dir.join("shaper-metrics.json");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/bench/udp-delay.py");
    let child = ProcessCommand::new("python3")
        .arg(script)
        .args(["--listen", &listen.to_string()])
        .args(["--to", &target.to_string()])
        .args(["--delay-ms", &(case.rtt_ms / 2).to_string()])
        .args(["--loss-percent", &case.loss_percent.to_string()])
        .args(["--mbit", &case.mbit.to_string()])
        .args(["--seed", "20260913"])
        .args(["--max-bytes", "16777216", "--max-packets", "16384"])
        .args(["--ready-file", ready.to_str().unwrap()])
        .args(["--metrics-file", metrics.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start UDP shaper");
    let deadline = Instant::now() + STEP_DEADLINE;
    while !ready.exists() {
        assert!(Instant::now() < deadline, "UDP shaper did not become ready");
        sleep(Duration::from_millis(10)).await;
    }
    Shaper {
        child: Some(child),
        metrics,
        addr: listen,
    }
}

const fn client_caps(mode: WireMode) -> ClientCapabilities {
    let (output_mode, profile) = match mode {
        WireMode::Raw => (OutputMode::Raw, BootstrapProfileKind::SynthesizedVtRaw),
        WireMode::StateSync => (
            OutputMode::StateSync,
            BootstrapProfileKind::SynthesizedVtStateSync,
        ),
    };
    ClientCapabilities::new()
        .with_color_support(ColorSupport::TrueColor)
        .with_layers(LayerSet::all())
        .with_output_mode(output_mode)
        .with_bootstrap(
            BootstrapCapabilities::new().with_profiles(BootstrapProfileSet::with(&[profile])),
        )
}

async fn dial_shaped(
    addr: SocketAddr,
    server_name: &str,
    token: Option<Vec<u8>>,
    trust: CertTrust,
    mode: WireMode,
) -> Connection {
    let dial = QuicDial {
        addr,
        server_name: server_name.to_owned(),
        token,
        trust,
    };
    let deadline = Instant::now() + STEP_DEADLINE;
    loop {
        match Connection::connect_quic_with_hello(
            &dial,
            "protocol-path-measure".to_owned(),
            client_caps(mode),
        )
        .await
        {
            Ok(connection) => return connection,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("QUIC endpoint did not become ready: {error}"),
        }
    }
}

async fn begin_attach(
    connection: &mut Connection,
    traffic: &mut Traffic,
    mode: WireMode,
) -> Attachment {
    let attach_id = connection.next_attach_id();
    connection
        .send(&FrameKind::Attach {
            attach_id,
            target: AttachTarget::ByName("measure".to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: true,
            scrollback_limit_lines: 10_000,
        })
        .await
        .expect("send ATTACH");
    let snapshot = loop {
        let frame = recv_step(connection).await;
        record_received(connection, traffic, mode, &frame).await;
        if let FrameKind::Attached { snapshot, .. } = frame {
            break snapshot;
        }
        observe(frame, traffic, None, None);
    };
    let ids: Vec<_> = snapshot
        .resources
        .into_iter()
        .map(|resource| resource.id)
        .collect();
    let mut bound_streams = HashMap::new();
    for (index, id) in ids.iter().enumerate() {
        connection
            .bind_terminal(id)
            .await
            .expect("bind Terminal stream");
        if connection.multistream_enabled() {
            let number = u64::try_from(index + 1).expect("Terminal stream index fits u64");
            bound_streams.insert(id.clone(), StreamId::new(number).unwrap());
        }
    }
    Attachment { ids, bound_streams }
}

async fn acknowledge_output(
    connection: &mut Connection,
    traffic: &mut Traffic,
    mode: WireMode,
    frame: &FrameKind,
) {
    let FrameKind::ResourceOutput {
        terminal_id,
        stream_id,
        bootstrap_id,
        seq,
        ..
    } = frame
    else {
        return;
    };
    if mode == WireMode::Raw {
        return;
    }
    connection
        .send(&FrameKind::FrameAck {
            terminal_id: terminal_id.clone(),
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
        })
        .await
        .expect("send StateSync FRAME_ACK");
    traffic.acks_sent += 1;
}

async fn record_received(
    connection: &mut Connection,
    traffic: &mut Traffic,
    mode: WireMode,
    frame: &FrameKind,
) {
    traffic.frames += 1;
    acknowledge_output(connection, traffic, mode, frame).await;
}

async fn await_ready(
    connection: &mut Connection,
    traffic: &mut Traffic,
    attachment: &Attachment,
    started: Instant,
    mode: WireMode,
) -> Vec<u64> {
    let expected: HashSet<_> = attachment.ids.iter().cloned().collect();
    let mut generations: HashMap<ResourceId, (StreamId, BootstrapId)> = HashMap::new();
    let mut ready = HashMap::new();
    while ready.len() < expected.len() {
        let frame = recv_step(connection).await;
        record_received(connection, traffic, mode, &frame).await;
        match &frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if expected.contains(terminal_id) => {
                if let Some(bound) = attachment.bound_streams.get(terminal_id) {
                    assert_eq!(stream_id, bound, "READY generation uses actual binding");
                }
                generations.insert(terminal_id.clone(), (*stream_id, *bootstrap_id));
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if generations.get(terminal_id) == Some(&(*stream_id, *bootstrap_id)) => {
                ready
                    .entry(terminal_id.clone())
                    .or_insert_with(|| micros(started.elapsed()));
            }
            _ => {}
        }
        observe(frame, traffic, None, None);
    }
    attachment.ids.iter().map(|id| ready[id]).collect()
}

async fn recv_step(connection: &mut Connection) -> FrameKind {
    timeout(STEP_DEADLINE, connection.recv())
        .await
        .expect("protocol receive timed out")
        .expect("protocol receive failed")
}

fn observe(
    frame: FrameKind,
    traffic: &mut Traffic,
    target: Option<&ResourceId>,
    needle: Option<&[u8]>,
) -> bool {
    match frame {
        FrameKind::BootstrapBegin { .. } | FrameKind::BootstrapReady { .. } => {
            traffic.bootstrap_frames += 1;
        }
        FrameKind::BootstrapChunk { payload, .. } => {
            traffic.bootstrap_frames += 1;
            traffic.bootstrap_bytes += u64::try_from(payload.len()).unwrap();
        }
        FrameKind::ResourceOutput {
            terminal_id, bytes, ..
        } => {
            traffic.output_frames += 1;
            traffic.output_bytes += u64::try_from(bytes.len()).unwrap();
            let tail = traffic.tails.entry(terminal_id.clone()).or_default();
            tail.extend_from_slice(&bytes);
            if tail.len() > 4096 {
                tail.drain(..tail.len() - 4096);
            }
            return target == Some(&terminal_id)
                && needle.is_some_and(|value| tail.windows(value.len()).any(|part| part == value));
        }
        FrameKind::BootstrapTombstone { .. } | FrameKind::HistoryTombstone { .. } => {
            traffic.tombstones += 1;
        }
        _ => {}
    }
    false
}

async fn wait_for_bytes(
    connection: &mut Connection,
    traffic: &mut Traffic,
    terminal: &ResourceId,
    needle: &[u8],
    mode: WireMode,
) {
    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let frame = timeout(remaining, connection.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for PTY bytes {needle:?}; tail={:?}",
                    traffic.tails.get(terminal)
                )
            })
            .expect("protocol receive failed while waiting for PTY bytes");
        record_received(connection, traffic, mode, &frame).await;
        if observe(frame, traffic, Some(terminal), Some(needle)) {
            return;
        }
    }
    panic!(
        "deadline waiting for PTY bytes {needle:?}; tail={:?}",
        traffic.tails.get(terminal)
    );
}

async fn send_paste(connection: &mut Connection, terminal: &ResourceId, bytes: Vec<u8>) {
    connection
        .send(&FrameKind::InputPaste {
            terminal_id: terminal.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: bytes,
            },
        })
        .await
        .expect("send PTY input");
}

const fn enter_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

fn text_key(character: char) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Unidentified,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some(character.to_string()),
        unshifted_codepoint: Some(character as u32),
    }
}

async fn send_line(connection: &mut Connection, terminal: &ResourceId, line: &[u8]) {
    assert!(!line.contains(&b'\n'), "send_line accepts one logical line");
    send_paste(connection, terminal, line.to_vec()).await;
    connection
        .send(&FrameKind::InputKey {
            terminal_id: terminal.clone(),
            event: enter_key(),
        })
        .await
        .expect("send Enter key");
}

async fn type_line(connection: &mut Connection, terminal: &ResourceId, line: &str) {
    for character in line.chars() {
        connection
            .send(&FrameKind::InputKey {
                terminal_id: terminal.clone(),
                event: text_key(character),
            })
            .await
            .expect("send text key");
    }
    connection
        .send(&FrameKind::InputKey {
            terminal_id: terminal.clone(),
            event: enter_key(),
        })
        .await
        .expect("send Enter key");
}

async fn prepare_quiet_probe(
    connection: &mut Connection,
    traffic: &mut Traffic,
    terminal: &ResourceId,
    mode: WireMode,
) {
    // Assemble the marker at execution time so the shell's echoed command
    // cannot satisfy the readiness wait before `stty -echo` takes effect.
    let command = b"stty -echo; printf 'PHUX_PROBE_%s\\n' READY; while IFS= read -r line; do printf 'PHUX_RESPONSE_%s\\n' \"$line\"; done\n";
    send_line(connection, terminal, command.strip_suffix(b"\n").unwrap()).await;
    wait_for_bytes(connection, traffic, terminal, b"PHUX_PROBE_READY", mode).await;
    traffic.tails.entry(terminal.clone()).or_default().clear();
}

async fn start_floods(
    connection: &mut Connection,
    traffic: &mut Traffic,
    ids: &[ResourceId],
    flood_bytes: usize,
    mode: WireMode,
) -> u64 {
    const FLOOD_LINE_BYTES: usize = b"FLOOD-00000000-abcdefghijklmnopqrstuvwxyz-0123456789\n".len();
    let lines = flood_bytes.div_ceil(FLOOD_LINE_BYTES);
    let generated_bytes = u64::try_from(lines.saturating_mul(FLOOD_LINE_BYTES)).unwrap();
    for (index, terminal) in ids[1..].iter().enumerate() {
        let started_marker = format!("PHUX_FLOOD_STARTED_{index}").into_bytes();
        let command = format!(
            "stty -echo; printf 'PHUX_FLOOD_%s_{index}\\n' STARTED; IFS= read -r go; i=0; while [ \"$i\" -lt {lines} ]; do printf 'FLOOD-%08d-abcdefghijklmnopqrstuvwxyz-0123456789\\n' \"$i\"; i=$((i+1)); done",
        );
        send_line(connection, terminal, command.as_bytes()).await;
        wait_for_bytes(connection, traffic, terminal, &started_marker, mode).await;
    }
    for terminal in &ids[1..] {
        send_line(connection, terminal, b"go").await;
    }
    generated_bytes.saturating_mul(u64::try_from(ids.len().saturating_sub(1)).unwrap())
}

async fn echo_samples(
    connection: &mut Connection,
    traffic: &mut Traffic,
    terminal: &ResourceId,
    samples: usize,
    mode: WireMode,
) -> Vec<u64> {
    let mut latencies = Vec::with_capacity(samples);
    for sample in 0..samples {
        let token = format!("nonce-{sample:04}-{}", 0x5eed_u64 + sample as u64);
        let response = format!("PHUX_RESPONSE_{token}");
        let started = Instant::now();
        type_line(connection, terminal, &token).await;
        wait_for_bytes(connection, traffic, terminal, response.as_bytes(), mode).await;
        latencies.push(micros(started.elapsed()));
    }
    latencies
}

async fn control_samples(
    connection: &mut Connection,
    traffic: &mut Traffic,
    samples: usize,
    mode: WireMode,
) -> Vec<u64> {
    let mut latencies = Vec::with_capacity(samples);
    for sample in 0..samples {
        let nonce = 0xc011_0000_u64 + sample as u64;
        let started = Instant::now();
        connection
            .send(&FrameKind::Ping { nonce })
            .await
            .expect("send PING");
        loop {
            let frame = recv_step(connection).await;
            record_received(connection, traffic, mode, &frame).await;
            if matches!(frame, FrameKind::Pong { nonce: got } if got == nonce) {
                latencies.push(micros(started.elapsed()));
                break;
            }
            observe(frame, traffic, None, None);
        }
    }
    latencies
}

async fn server_perf(connection: &mut Connection, traffic: &mut Traffic, mode: WireMode) -> String {
    let (result, interleaved) = connection
        .request(900_000, Command::GetPerf { reset: false })
        .await
        .expect("GET_PERF transport")
        .into_parts();
    for frame in interleaved {
        record_received(connection, traffic, mode, &frame).await;
        observe(frame, traffic, None, None);
    }
    let CommandResult::OkWith(CommandValue::Json(json)) = result else {
        panic!("GET_PERF failed: {result:?}");
    };
    PerfReport::from_json(&json)
        .expect("GET_PERF JSON")
        .to_json()
}

struct UploadChunkResult {
    send_us: u64,
    ack_us: u64,
    echo_us: u64,
    control_us: u64,
    path: Option<String>,
}

struct UploadChunk<'a> {
    request_id: u32,
    upload_id: FileUploadId,
    terminal_id: &'a ResourceId,
    quiet_terminal_id: &'a ResourceId,
    offset: u64,
    data: Vec<u8>,
    final_chunk: bool,
    digest: [u8; 32],
}

struct UploadSamples {
    actual_max_chunk_bytes: usize,
    send_us: Vec<u64>,
    chunk_ack_us: Vec<u64>,
    echo_us: Vec<u64>,
    control_us: Vec<u64>,
    completed_path: String,
}

async fn upload_chunk_probe(
    connection: &mut Connection,
    traffic: &mut Traffic,
    chunk: UploadChunk<'_>,
) -> UploadChunkResult {
    let expected_offset = chunk.offset + u64::try_from(chunk.data.len()).unwrap();
    let upload_started = Instant::now();
    connection
        .send(&FrameKind::Command {
            request_id: chunk.request_id,
            command: Command::PutFile {
                upload_id: chunk.upload_id,
                terminal_id: chunk.terminal_id.clone(),
                extension: "bin".to_owned(),
                offset: chunk.offset,
                data: chunk.data,
                final_chunk: chunk.final_chunk,
                sha256: chunk.final_chunk.then_some(chunk.digest),
            },
        })
        .await
        .expect("send production PUT_FILE chunk");
    let send_us = micros(upload_started.elapsed());

    let ping_nonce = 0xfeed_0000_u64 + u64::from(chunk.request_id);
    let control_started = Instant::now();
    connection
        .send(&FrameKind::Ping { nonce: ping_nonce })
        .await
        .expect("send upload-adjacent PING");
    let echo_token = format!("upload-{:08}", chunk.request_id);
    let echo_needle = format!("PHUX_RESPONSE_{echo_token}");
    let echo_started = Instant::now();
    type_line(connection, chunk.quiet_terminal_id, &echo_token).await;

    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    let mut ack = None;
    let mut echo_us = None;
    let mut control_us = None;
    while ack.is_none() || echo_us.is_none() || control_us.is_none() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = timeout(remaining, connection.recv())
            .await
            .expect("PUT_FILE observation timed out")
            .expect("PUT_FILE observation receive failed");
        record_received(connection, traffic, WireMode::Raw, &frame).await;
        match &frame {
            FrameKind::CommandResult { request_id, result } if *request_id == chunk.request_id => {
                let CommandResult::OkWith(CommandValue::FileUpload(upload_ack)) = result else {
                    panic!("PUT_FILE failed: {result:?}");
                };
                assert_eq!(
                    upload_ack.next_offset, expected_offset,
                    "PUT_FILE next offset"
                );
                assert_eq!(
                    upload_ack.path.is_some(),
                    chunk.final_chunk,
                    "only the final PUT_FILE chunk publishes a path"
                );
                ack = Some((micros(upload_started.elapsed()), upload_ack.path.clone()));
            }
            FrameKind::Pong { nonce } if *nonce == ping_nonce => {
                control_us = Some(micros(control_started.elapsed()));
            }
            _ => {
                if echo_us.is_none()
                    && observe(
                        frame,
                        traffic,
                        Some(chunk.quiet_terminal_id),
                        Some(echo_needle.as_bytes()),
                    )
                {
                    echo_us = Some(micros(echo_started.elapsed()));
                }
                continue;
            }
        }
        observe(frame, traffic, None, None);
    }
    let (ack_us, path) = ack.unwrap();
    UploadChunkResult {
        send_us,
        ack_us,
        echo_us: echo_us.unwrap(),
        control_us: control_us.unwrap(),
        path,
    }
}

fn upload_payload(bytes: usize) -> Vec<u8> {
    (0..bytes)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect()
}

async fn connect_upload_case(
    temp: &Path,
    socket: &Path,
    server_addr: SocketAddr,
    shaper_addr: SocketAddr,
    case: UploadCase,
) -> (Server, Shaper, Connection, Traffic, Vec<ResourceId>) {
    let server = spawn_server(socket.to_owned(), Some(server_addr), Vec::new());
    let mut setup = wait_for_uds(socket).await;
    spawn_flood_panes(&mut setup, 2).await;
    drop(setup);
    dial_shaped(
        server_addr,
        "localhost",
        None,
        CertTrust::SkipVerify,
        WireMode::Raw,
    )
    .await
    .shutdown()
    .await;

    let shape = Case {
        route: Route::Direct,
        mode: WireMode::Raw,
        terminals: 2,
        rtt_ms: UPLOAD_RTT_MS,
        loss_percent: 0.0,
        mbit: case.mbit,
        samples: 1,
        flood_bytes: 0,
    };
    let shaper = start_shaper(temp, shape, shaper_addr, server_addr).await;
    let mut connection = dial_shaped(
        shaper_addr,
        "localhost",
        None,
        CertTrust::SkipVerify,
        WireMode::Raw,
    )
    .await;
    let mut traffic = Traffic::default();
    let attachment = begin_attach(&mut connection, &mut traffic, WireMode::Raw).await;
    assert_eq!(
        attachment.ids.len(),
        2,
        "upload experiment needs a second quiet Terminal"
    );
    await_ready(
        &mut connection,
        &mut traffic,
        &attachment,
        Instant::now(),
        WireMode::Raw,
    )
    .await;
    prepare_quiet_probe(
        &mut connection,
        &mut traffic,
        &attachment.ids[1],
        WireMode::Raw,
    )
    .await;
    (server, shaper, connection, traffic, attachment.ids)
}

async fn upload_payload_chunks(
    connection: &mut Connection,
    traffic: &mut Traffic,
    ids: &[ResourceId],
    case: UploadCase,
    payload: &[u8],
    digest: [u8; 32],
) -> UploadSamples {
    let upload_id = FileUploadId::new([0x5a; 16]).unwrap();
    let mut send_us = Vec::new();
    let mut chunk_ack_us = Vec::new();
    let mut echo_us = Vec::new();
    let mut control_us = Vec::new();
    let mut completed_path = None;
    let mut actual_max_chunk_bytes = 0;
    for (index, bytes) in payload.chunks(case.configured_chunk_bytes).enumerate() {
        actual_max_chunk_bytes = actual_max_chunk_bytes.max(bytes.len());
        let offset = u64::try_from(index.saturating_mul(case.configured_chunk_bytes)).unwrap();
        let final_chunk =
            offset + u64::try_from(bytes.len()).unwrap() == u64::try_from(payload.len()).unwrap();
        let result = upload_chunk_probe(
            connection,
            traffic,
            UploadChunk {
                request_id: 2_000_000 + u32::try_from(index).unwrap(),
                upload_id,
                terminal_id: &ids[0],
                quiet_terminal_id: &ids[1],
                offset,
                data: bytes.to_vec(),
                final_chunk,
                digest,
            },
        )
        .await;
        send_us.push(result.send_us);
        chunk_ack_us.push(result.ack_us);
        echo_us.push(result.echo_us);
        control_us.push(result.control_us);
        if result.path.is_some() {
            completed_path = result.path;
        }
    }
    UploadSamples {
        actual_max_chunk_bytes,
        send_us,
        chunk_ack_us,
        echo_us,
        control_us,
        completed_path: completed_path.expect("final PUT_FILE ack publishes path"),
    }
}

async fn run_upload_case(case: UploadCase) -> UploadResult {
    let temp = TempDir::new().expect("upload case tempdir");
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");
    let upload_dir = temp.path().join("uploads");
    let _env = EnvGuard::tls(&cert, &key, None).with_upload_dir(&upload_dir);
    let socket = temp.path().join("phux.sock");
    let server_addr = free_udp_addr();
    let shaper_addr = free_udp_addr();
    let (server, shaper, mut connection, mut traffic, ids) =
        connect_upload_case(temp.path(), &socket, server_addr, shaper_addr, case).await;

    let rss_before_bytes = current_rss_bytes();
    let payload = upload_payload(case.file_bytes);
    let digest: [u8; 32] = Sha256::digest(&payload).into();
    let upload_started = Instant::now();
    let samples =
        upload_payload_chunks(&mut connection, &mut traffic, &ids, case, &payload, digest).await;
    let upload_us = micros(upload_started.elapsed());
    assert_eq!(
        std::fs::read(&samples.completed_path).expect("read completed production upload"),
        payload,
        "published upload must match the sent bytes"
    );
    drop(payload);
    sleep(Duration::from_millis(100)).await;
    let rss_after_bytes = current_rss_bytes();
    let server_perf = server_perf(&mut connection, &mut traffic, WireMode::Raw).await;
    connection.shutdown().await;
    let shaper_metrics = shaper.stop();
    let server_shutdown_us = server.stop().await;

    UploadResult {
        case,
        actual_max_chunk_bytes: samples.actual_max_chunk_bytes,
        chunks: samples.chunk_ack_us.len(),
        upload_us,
        goodput_bps: upload_goodput_bps(case.file_bytes, upload_us),
        send_us: summarize(samples.send_us),
        chunk_ack_us: summarize(samples.chunk_ack_us),
        echo_us: summarize(samples.echo_us),
        control_us: summarize(samples.control_us),
        rss_before_bytes,
        rss_after_bytes,
        server_shutdown_us,
        shaper_metrics,
        server_perf,
    }
}

async fn await_connection_cleanup(
    connection: &mut Connection,
    traffic: &mut Traffic,
    aborted_at: Instant,
) -> u64 {
    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    loop {
        let report = server_perf(connection, traffic, WireMode::Raw).await;
        if metric_value(&report, "proc.clients") == Some(1) {
            return micros(aborted_at.elapsed());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "aborted upload connection remained in proc.clients"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

async fn abort_backpressured_upload(
    mut connection: Connection,
    terminal_id: ResourceId,
) -> Instant {
    let (started_tx, started_rx) = oneshot::channel();
    let send_task = tokio::task::spawn_local(async move {
        started_tx.send(()).ok();
        connection
            .send(&FrameKind::Command {
                request_id: 3_000_000,
                command: Command::PutFile {
                    upload_id: FileUploadId::new([0x6b; 16]).unwrap(),
                    terminal_id,
                    extension: "bin".to_owned(),
                    offset: 0,
                    data: vec![0x5a; MAX_FILE_UPLOAD_CHUNK],
                    final_chunk: false,
                    sha256: None,
                },
            })
            .await
    });
    started_rx.await.expect("upload send task started");
    sleep(Duration::from_millis(500)).await;
    assert!(
        !send_task.is_finished(),
        "legal wire-ceiling frame did not remain backpressured on the thin path"
    );
    send_task.abort();
    let error = timeout(STEP_DEADLINE, send_task)
        .await
        .expect("aborted upload task did not join")
        .expect_err("aborted upload task unexpectedly completed");
    assert!(error.is_cancelled(), "upload task did not cancel: {error}");
    Instant::now()
}

async fn write_quic_frame(send: &mut quinn::SendStream, frame: &FrameKind) {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    timeout(STEP_DEADLINE, send.write_all(&encoded))
        .await
        .expect("QUIC frame write timed out")
        .expect("QUIC frame write failed");
}

async fn raw_send_line(send: &mut quinn::SendStream, terminal_id: &ResourceId, line: &[u8]) {
    assert!(
        !line.contains(&b'\n'),
        "raw_send_line accepts one logical line"
    );
    write_quic_frame(
        send,
        &FrameKind::InputPaste {
            terminal_id: terminal_id.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: line.to_vec(),
            },
        },
    )
    .await;
    write_quic_frame(
        send,
        &FrameKind::InputKey {
            terminal_id: terminal_id.clone(),
            event: enter_key(),
        },
    )
    .await;
}

async fn raw_type_line(send: &mut quinn::SendStream, terminal_id: &ResourceId, line: &str) {
    for character in line.chars() {
        write_quic_frame(
            send,
            &FrameKind::InputKey {
                terminal_id: terminal_id.clone(),
                event: text_key(character),
            },
        )
        .await;
    }
    write_quic_frame(
        send,
        &FrameKind::InputKey {
            terminal_id: terminal_id.clone(),
            event: enter_key(),
        },
    )
    .await;
}

async fn read_quic_frame(recv: &mut quinn::RecvStream) -> FrameKind {
    let mut header = [0_u8; 4];
    timeout(STEP_DEADLINE, recv.read_exact(&mut header))
        .await
        .expect("QUIC frame header timed out")
        .expect("QUIC frame header failed");
    let body_len = u32::from_be_bytes(header) as usize;
    let mut framed = header.to_vec();
    framed.resize(4 + body_len, 0);
    timeout(STEP_DEADLINE, recv.read_exact(&mut framed[4..]))
        .await
        .expect("QUIC frame body timed out")
        .expect("QUIC frame body failed");
    let (frame, rest) = FrameKind::decode(&framed).expect("decode QUIC frame");
    assert!(rest.is_empty(), "QUIC frame decoder left trailing bytes");
    frame
}

async fn raw_negotiate(send: &mut quinn::SendStream, recv: &mut quinn::RecvStream) {
    write_quic_frame(
        send,
        &FrameKind::Hello {
            client_name: "protocol-path-selective-stall".to_owned(),
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            client_caps: client_caps(WireMode::Raw).with_quic_streams(true),
        },
    )
    .await;
    let hello = read_quic_frame(recv).await;
    assert!(
        matches!(
            hello,
            FrameKind::HelloOk { server_caps, .. }
                if server_caps.features.contains(ServerFeature::QuicStreams)
        ),
        "production server did not negotiate Terminal streams: {hello:?}"
    );
}

async fn raw_attach(send: &mut quinn::SendStream, recv: &mut quinn::RecvStream) -> Vec<ResourceId> {
    write_quic_frame(
        send,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::ByName("measure".to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        },
    )
    .await;
    loop {
        if let FrameKind::Attached { snapshot, .. } = read_quic_frame(recv).await {
            return snapshot
                .resources
                .into_iter()
                .map(|resource| resource.id)
                .collect();
        }
    }
}

async fn raw_bind(
    connection: &quinn::Connection,
    terminal_id: &ResourceId,
    stream_number: u64,
) -> (quinn::SendStream, quinn::RecvStream) {
    let (mut send, recv) = timeout(STEP_DEADLINE, connection.open_bi())
        .await
        .expect("opening Terminal stream timed out")
        .expect("opening Terminal stream failed");
    let mut bind = BytesMut::new();
    phux_protocol::wire::stream_bind::encode(
        &phux_protocol::wire::stream_bind::StreamBind {
            terminal_id: terminal_id.clone(),
            stream_id: phux_protocol::StreamId::new(stream_number).unwrap(),
        },
        &mut bind,
    );
    send.write_all(&bind).await.expect("write STREAM_BIND");
    (send, recv)
}

async fn raw_wait_ready(recv: &mut quinn::RecvStream, terminal_id: &ResourceId) {
    let mut generation = None;
    loop {
        match read_quic_frame(recv).await {
            FrameKind::BootstrapBegin {
                terminal_id: begin,
                stream_id,
                bootstrap_id,
                ..
            } if begin == *terminal_id => generation = Some((stream_id, bootstrap_id)),
            FrameKind::BootstrapReady {
                terminal_id: ready,
                stream_id,
                bootstrap_id,
                ..
            } if ready == *terminal_id && generation == Some((stream_id, bootstrap_id)) => return,
            _ => {}
        }
    }
}

async fn raw_wait_for_output(
    recv: &mut quinn::RecvStream,
    terminal_id: &ResourceId,
    needle: &[u8],
) {
    let mut tail = Vec::new();
    loop {
        if let FrameKind::ResourceOutput {
            terminal_id: output_terminal,
            bytes,
            ..
        } = read_quic_frame(recv).await
        {
            assert_eq!(output_terminal, *terminal_id, "bound stream provenance");
            tail.extend_from_slice(&bytes);
            if tail.windows(needle.len()).any(|part| part == needle) {
                return;
            }
            if tail.len() > 4096 {
                tail.drain(..tail.len() - 4096);
            }
        }
    }
}

async fn raw_wait_pong(recv: &mut quinn::RecvStream, nonce: u64) {
    loop {
        if matches!(read_quic_frame(recv).await, FrameKind::Pong { nonce: got } if got == nonce) {
            return;
        }
    }
}

fn route_fixture(route: Route, temp: &Path) -> RouteFixture {
    match route {
        Route::Direct => RouteFixture {
            relay: None,
            target_addr: free_udp_addr(),
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            connectors: Vec::new(),
            consumer_tokens: None,
        },
        Route::Relay => {
            let (relay, target_addr, fingerprint, connector, consumer_tokens) = spawn_relay(temp);
            RouteFixture {
                relay: Some(relay),
                target_addr,
                server_name: ROUTE_NAME.to_owned(),
                token: Some(CONSUMER_TOKEN.to_vec()),
                trust: CertTrust::Pinned(fingerprint),
                connectors: vec![connector],
                consumer_tokens: Some(consumer_tokens),
            }
        }
    }
}

async fn verify_direct_listener(case: Case, route: &RouteFixture) {
    if case.route != Route::Direct {
        return;
    }
    assert!(
        UdpSocket::bind(route.target_addr).is_err(),
        "server reported UDS ready but did not bind requested QUIC address {}",
        route.target_addr
    );
    // UDS readiness does not prove the independently built QUIC listener.
    dial_shaped(
        route.target_addr,
        &route.server_name,
        route.token.clone(),
        route.trust.clone(),
        case.mode,
    )
    .await
    .shutdown()
    .await;
}

async fn run_case(case: Case) -> CaseResult {
    validate_case(case);
    let temp = TempDir::new().expect("case tempdir");
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");
    let socket = temp.path().join("phux.sock");
    let shaper_addr = free_udp_addr();
    let mut route = route_fixture(case.route, temp.path());
    let _env = EnvGuard::tls(&cert, &key, route.consumer_tokens.as_deref());
    let direct_addr = (case.route == Route::Direct).then_some(route.target_addr);
    let server = spawn_server(
        socket.clone(),
        direct_addr,
        std::mem::take(&mut route.connectors),
    );

    let mut setup = wait_for_uds(&socket).await;
    spawn_flood_panes(&mut setup, case.terminals).await;
    drop(setup);

    verify_direct_listener(case, &route).await;

    let shaper = start_shaper(temp.path(), case, shaper_addr, route.target_addr).await;
    let mut connection = dial_shaped(
        shaper_addr,
        &route.server_name,
        route.token,
        route.trust,
        case.mode,
    )
    .await;
    let profile = format!("{:?}", connection.negotiated_bootstrap().unwrap().profile);
    let multistream = connection.multistream_enabled();
    assert_eq!(
        multistream,
        case.route == Route::Direct,
        "direct QUIC must bind Terminal streams while relay fallback stays single-stream"
    );
    let mut traffic = Traffic::default();
    let attach_started = Instant::now();
    let attachment = begin_attach(&mut connection, &mut traffic, case.mode).await;
    assert_eq!(
        attachment.ids.len(),
        case.terminals,
        "ATTACHED resource count"
    );
    let ready_us = await_ready(
        &mut connection,
        &mut traffic,
        &attachment,
        attach_started,
        case.mode,
    )
    .await;
    let ids = attachment.ids;

    prepare_quiet_probe(&mut connection, &mut traffic, &ids[0], case.mode).await;
    let flood_requested_bytes = start_floods(
        &mut connection,
        &mut traffic,
        &ids,
        case.flood_bytes,
        case.mode,
    )
    .await;
    let echo_us = echo_samples(
        &mut connection,
        &mut traffic,
        &ids[0],
        case.samples,
        case.mode,
    )
    .await;
    let control_us = control_samples(&mut connection, &mut traffic, case.samples, case.mode).await;
    let server_perf = server_perf(&mut connection, &mut traffic, case.mode).await;
    match case.mode {
        WireMode::Raw => assert_eq!(traffic.acks_sent, 0, "raw output must not be ACKed"),
        WireMode::StateSync => assert!(traffic.acks_sent > 0, "StateSync output must be ACKed"),
    }
    connection.shutdown().await;
    let shaper_metrics = shaper.stop();
    let cancellation_us = server.stop().await;
    if let Some(relay) = route.relay {
        relay.stop().await;
    }

    CaseResult {
        case,
        profile,
        multistream,
        ready_us: summarize(ready_us),
        echo_us: summarize(echo_us),
        control_us: summarize(control_us),
        flood_terminals: ids.len().saturating_sub(1),
        flood_requested_bytes,
        traffic,
        cancellation_us,
        shaper_metrics,
        server_perf,
    }
}

#[test]
fn nearest_rank_keeps_small_sample_tails() {
    let summary = summarize(vec![10, 20]);
    assert_eq!(summary.p50, 10);
    assert_eq!(summary.p95, 20);
    assert_eq!(summary.p99, 20);
    assert_eq!(summary.max, 20);
}

#[test]
#[should_panic(expected = "UDP shaper upstream queue_drops invalidated the experiment")]
fn shaper_queue_drop_invalidates_sample() {
    validate_shaper_metrics(
        r#"{
            "upstream":{"queue_drops":1,"shutdown_drops":0},
            "downstream":{"queue_drops":0,"shutdown_drops":0}
        }"#,
    );
}

#[test]
#[should_panic(expected = "UDP shaper downstream shutdown_drops invalidated the experiment")]
fn shaper_shutdown_drop_invalidates_sample() {
    validate_shaper_metrics(
        r#"{
            "upstream":{"queue_drops":0,"shutdown_drops":0},
            "downstream":{"queue_drops":0,"shutdown_drops":2}
        }"#,
    );
}

#[test]
#[ignore = "real PTY + shaped QUIC measurement; run explicitly and serially"]
fn shaped_quic_protocol_path_matrix() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        for case in selected_cases() {
            let result = timeout(CASE_DEADLINE, run_case(case))
                .await
                .expect("protocol path case exceeded its deadline");
            emit_result(&result);
        }
    });
}

#[test]
#[ignore = "real PUT_FILE + shaped QUIC measurement; run explicitly and serially"]
fn put_file_chunk_matrix() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        for case in selected_upload_cases() {
            let result = timeout(CASE_DEADLINE, run_upload_case(case))
                .await
                .expect("PUT_FILE path case exceeded its deadline");
            emit_upload_result(&result);
        }
    });
}

#[test]
#[ignore = "real in-flight PUT_FILE disconnect over thin shaped QUIC; run explicitly and serially"]
fn inflight_upload_disconnect_releases_server_connection() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        timeout(CASE_DEADLINE, async {
            let temp = TempDir::new().expect("upload cancellation tempdir");
            let cert = temp.path().join("cert.pem");
            let key = temp.path().join("key.pem");
            let upload_dir = temp.path().join("uploads");
            let _env = EnvGuard::tls(&cert, &key, None).with_upload_dir(&upload_dir);
            let socket = temp.path().join("phux.sock");
            let case = UploadCase {
                configured_chunk_bytes: MAX_FILE_UPLOAD_CHUNK,
                file_bytes: MAX_FILE_UPLOAD_CHUNK,
                mbit: 0.3,
            };
            let (server, shaper, connection, _traffic, ids) = connect_upload_case(
                temp.path(),
                &socket,
                free_udp_addr(),
                free_udp_addr(),
                case,
            )
            .await;
            let shaper_addr = shaper.addr;
            let aborted_at = abort_backpressured_upload(connection, ids[0].clone()).await;

            let mut probe = dial_shaped(
                shaper_addr,
                "localhost",
                None,
                CertTrust::SkipVerify,
                WireMode::Raw,
            )
            .await;
            let mut probe_traffic = Traffic::default();
            let cleanup_us =
                await_connection_cleanup(&mut probe, &mut probe_traffic, aborted_at).await;
            probe.shutdown().await;
            sleep(Duration::from_millis(100)).await;
            let metrics = shaper.stop();
            let sent_bytes = shaper_value(&metrics, "upstream", "sent_bytes");
            assert!(
                sent_bytes > 8 * 1024,
                "shaped case sent no meaningful upstream traffic"
            );
            assert!(
                sent_bytes < u64::try_from(MAX_FILE_UPLOAD_CHUNK).unwrap(),
                "entire wire-ceiling payload crossed before cancellation"
            );
            eprintln!(
                "protocol_upload_disconnect={{frame_bytes:{MAX_FILE_UPLOAD_CHUNK},mbit:0.3,send_backpressured:true,upstream_total_sent_bytes:{sent_bytes},server_connection_cleanup_us:{cleanup_us}}}"
            );
            server.stop().await;
        })
        .await
        .expect("in-flight upload disconnect case exceeded its deadline");
    });
}

#[test]
#[ignore = "real PTY + selective QUIC stream stall; run explicitly and serially"]
#[allow(
    clippy::too_many_lines,
    reason = "the linear setup/workload/teardown narrative is the acceptance experiment"
)]
fn stalled_terminal_reader_preserves_quiet_echo_and_control() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        timeout(CASE_DEADLINE, async {
            let temp = TempDir::new().expect("stall tempdir");
            let cert = temp.path().join("cert.pem");
            let key = temp.path().join("key.pem");
            let _env = EnvGuard::tls(&cert, &key, None);
            let socket = temp.path().join("phux.sock");
            let server_addr = free_udp_addr();
            let shaper_addr = free_udp_addr();
            let server = spawn_server(socket.clone(), Some(server_addr), Vec::new());
            let mut setup = wait_for_uds(&socket).await;
            spawn_flood_panes(&mut setup, 8).await;
            drop(setup);

            let case = Case {
                route: Route::Direct,
                mode: WireMode::Raw,
                terminals: 8,
                rtt_ms: 0,
                loss_percent: 0.0,
                mbit: 30.0,
                samples: 3,
                flood_bytes: 16 * 1024 * 1024,
            };
            let shaper = start_shaper(temp.path(), case, shaper_addr, server_addr).await;
            let dial = QuicDial {
                addr: shaper_addr,
                server_name: "localhost".to_owned(),
                token: None,
                trust: CertTrust::SkipVerify,
            };
            let (endpoint, connection, mut control_send, mut control_recv) =
                phux_dial::quic::dial(&dial).await.expect("raw production QUIC dial");
            raw_negotiate(&mut control_send, &mut control_recv).await;
            let ids = raw_attach(&mut control_send, &mut control_recv).await;
            assert_eq!(ids.len(), 8, "stall topology resource count");

            let (mut quiet_send, mut quiet_recv) = raw_bind(&connection, &ids[0], 1).await;
            let (mut stalled_send, mut stalled_recv) = raw_bind(&connection, &ids[1], 2).await;
            raw_wait_ready(&mut quiet_recv, &ids[0]).await;

            let quiet_setup = b"stty -echo; printf 'PHUX_PROBE_%s\\n' READY; while IFS= read -r line; do printf 'PHUX_RESPONSE_%s\\n' \"$line\"; done\n";
            raw_send_line(
                &mut quiet_send,
                &ids[0],
                quiet_setup.strip_suffix(b"\n").unwrap(),
            )
            .await;
            raw_wait_for_output(&mut quiet_recv, &ids[0], b"PHUX_PROBE_READY").await;

            let lines = case.flood_bytes.div_ceil(STALL_LINE_BYTES);
            let flood = format!(
                "stty -echo; printf 'PHUX_STALL_%s\\n' STARTED; IFS= read -r go; i=0; while [ \"$i\" -lt {lines} ]; do printf 'STALL-%08d-abcdefghijklmnopqrstuvwxyz-0123456789\\n' \"$i\"; i=$((i+1)); done\n"
            );
            raw_send_line(&mut stalled_send, &ids[1], flood.trim_end().as_bytes()).await;
            raw_wait_for_output(&mut stalled_recv, &ids[1], b"PHUX_STALL_STARTED").await;
            raw_send_line(&mut stalled_send, &ids[1], b"go").await;
            // Keep this stream alive but deliberately stop polling it here.
            let _stalled_recv = stalled_recv;
            sleep(Duration::from_millis(250)).await;

            let control_started = Instant::now();
            write_quic_frame(&mut control_send, &FrameKind::Ping { nonce: 0x51a11 }).await;
            raw_wait_pong(&mut control_recv, 0x51a11).await;
            let control_us = micros(control_started.elapsed());

            let token = "stall-isolated-nonce";
            let echo_started = Instant::now();
            raw_type_line(&mut quiet_send, &ids[0], token).await;
            raw_wait_for_output(
                &mut quiet_recv,
                &ids[0],
                b"PHUX_RESPONSE_stall-isolated-nonce",
            )
            .await;
            let echo_us = micros(echo_started.elapsed());
            eprintln!(
                "protocol_path_stall_result={{control_us:{control_us},echo_us:{echo_us},requested_bytes:{},flood_started:true,stalled_terminal:{:?},quiet_terminal:{:?}}}",
                lines * STALL_LINE_BYTES,
                ids[1],
                ids[0]
            );

            connection.close(0_u32.into(), b"measurement complete");
            endpoint.wait_idle().await;
            let metrics = shaper.stop();
            eprintln!("protocol_path_stall_shaper={metrics}");
            let cancellation_us = server.stop().await;
            eprintln!("protocol_path_stall_cancellation_us={cancellation_us}");
        })
        .await
        .expect("selective stalled-reader case exceeded its deadline");
    });
}
