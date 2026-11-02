//! The warm attach gate: HELLO through `ATTACH_READY` over a real Unix socket
//! against a running server, with native bootstrap selected. This is the host
//! half of the capture gates; the engine half is `phux-terminal`'s `capture`
//! bench.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    missing_docs,
    reason = "assertion-bearing benchmark binary"
)]

use phux_server_testkit as common;
#[path = "../../../benchmarks/support.rs"]
pub(crate) mod support;

use std::hint::black_box;
use std::time::{Duration, Instant};

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
    LayerSet,
};
use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, TYPE_ATTACH_READY, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN,
    TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY, TYPE_DETACHED, TYPE_HELLO_OK, ViewportInfo,
};
use portable_pty::CommandBuilder;
use support::{Comparison, Corpus, MEASURED_SAMPLES, Threshold, WARMUP_SAMPLES, percentile};

async fn native_warm_attach(socket: &std::path::Path) -> Duration {
    let started = Instant::now();
    let mut stream = common::wait_for_raw_socket(socket, common::SOCKET_CONNECT_DEADLINE).await;
    negotiate_native_hello(&mut stream).await;
    attach_until_ready(&mut stream).await;
    let ready = started.elapsed();
    detach(&mut stream).await;
    ready
}

/// HELLO with native bootstrap capabilities; the server must select the
/// native-state profile.
async fn negotiate_native_hello(stream: &mut tokio::net::UnixStream) {
    let native = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttySnapshotV1,
        EngineFeatureSet::required_native(),
    );
    common::send_frame(
        stream,
        &FrameKind::Hello {
            client_name: "phux-native-ready-benchmark".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_layers(LayerSet::all())
                .with_bootstrap(native),
        },
    )
    .await;
    let (kind, hello) = common::recv_typed(stream).await;
    assert_eq!(kind, TYPE_HELLO_OK);
    assert!(matches!(
        hello,
        FrameKind::HelloOk {
            selected_profile: BootstrapProfile::NativeState { .. },
            ..
        }
    ));
}

/// ATTACH the benchmark session and read through `ATTACH_READY`.
async fn attach_until_ready(stream: &mut tokio::net::UnixStream) {
    common::send_frame(
        stream,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::ByName("benchmark".to_owned()),
            viewport: ViewportInfo::new(200, 60),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        },
    )
    .await;
    let (kind, _) = common::recv_typed(stream).await;
    assert_eq!(kind, TYPE_ATTACHED);
    let (kind, begin) = common::recv_typed(stream).await;
    assert_eq!(kind, TYPE_BOOTSTRAP_BEGIN);
    assert!(matches!(begin, FrameKind::BootstrapBegin { .. }));
    drain_bootstrap_chunks(stream).await;
    let (kind, attached) = common::recv_typed(stream).await;
    assert_eq!(kind, TYPE_ATTACH_READY);
    assert!(matches!(attached, FrameKind::AttachReady { .. }));
}

/// Read `BOOTSTRAP_CHUNK`s through `BOOTSTRAP_READY`.
async fn drain_bootstrap_chunks(stream: &mut tokio::net::UnixStream) {
    loop {
        let (kind, frame) = common::recv_typed(stream).await;
        match frame {
            FrameKind::BootstrapChunk { .. } => assert_eq!(kind, TYPE_BOOTSTRAP_CHUNK),
            FrameKind::BootstrapReady { .. } => {
                assert_eq!(kind, TYPE_BOOTSTRAP_READY);
                return;
            }
            other => panic!("unexpected warm native attach frame: {other:?}"),
        }
    }
}

/// Session DETACH, answered by `DETACHED`.
async fn detach(stream: &mut tokio::net::UnixStream) {
    common::send_frame(stream, &FrameKind::Detach).await;
    let (kind, detached) = common::recv_typed(stream).await;
    assert_eq!(kind, TYPE_DETACHED);
    assert!(matches!(detached, FrameKind::Detached { .. }));
}

fn checked_warm_uds_and_resync_gate() {
    common::run_local(async {
        let mut command = CommandBuilder::new("/bin/cat");
        command.arg("-");
        let harness = common::builder::E2eBuilder::new()
            .session("benchmark")
            .seed_cmd(command)
            .viewport(200, 60)
            .spawn()
            .await;
        for _ in 0..WARMUP_SAMPLES {
            black_box(native_warm_attach(&harness.socket_path).await);
        }
        let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
        for _ in 0..MEASURED_SAMPLES {
            samples.push(native_warm_attach(&harness.socket_path).await);
        }
        let p95 = percentile(&mut samples.clone(), 95);
        let p99 = percentile(&mut samples, 99);
        println!(
            "metric=warm-uds-ready corpus={} clients=1 p95_us={} p99_us={}",
            Corpus::Tui200x60.label(),
            p95.as_micros(),
            p99.as_micros(),
        );
        for (metric, observed, limit) in [
            ("warm-uds-ready-p95", p95, 100.0),
            ("warm-uds-ready-p99", p99, 250.0),
        ] {
            Threshold {
                metric,
                corpus: Corpus::Tui200x60,
                clients: 1,
                comparison: Comparison::AtMost,
                observed: observed.as_secs_f64() * 1_000.0,
                limit,
                unit: "ms",
            }
            .check()
            .unwrap_or_else(|error| panic!("{error}"));
        }
        harness.shutdown().await;
    });
}

fn main() {
    checked_warm_uds_and_resync_gate();
}
