//! Reproducible synthesized/native capture, fanout, UDS READY, and recovery gates.

#![allow(
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::too_many_lines,
    missing_docs,
    reason = "assertion-bearing benchmark binary"
)]

#[allow(
    unused_imports,
    reason = "the benchmark includes the complete integration harness but uses only its wire helpers"
)]
#[path = "../tests/common/mod.rs"]
mod common;
pub(crate) mod server_measure;
#[path = "../../../benchmarks/support.rs"]
pub(crate) mod support;

use std::hint::black_box;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput};
use libghostty_vt::build_info::{self, OptimizeMode};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
    LayerSet,
};
use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK,
    TYPE_BOOTSTRAP_READY, TYPE_DETACHED, TYPE_HELLO_OK, ViewportInfo,
};
use portable_pty::CommandBuilder;
use server_measure::{
    build_terminal, build_unicode_ready_control, fanout_measurement, native_full_measurement,
    native_ready_measurement, retained_budget_holds, synthesized_measurement,
};
use support::{
    Comparison, Corpus, HISTORY_PAGE_LIMIT, MEASURED_SAMPLES, Threshold, WARMUP_SAMPLES,
    deterministic_page, percentile,
};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn criterion_capture(c: &mut Criterion) {
    let mut group = c.benchmark_group("server-capture");
    for corpus in Corpus::ALL {
        let mut terminal = build_terminal(corpus);
        for _ in 0..WARMUP_SAMPLES {
            black_box(synthesized_measurement(&terminal));
            black_box(native_ready_measurement(&mut terminal));
        }
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("synthesized-ready-and-full-history", corpus.label()),
            &corpus,
            |b, _| b.iter(|| black_box(synthesized_measurement(black_box(&terminal)))),
        );
        group.bench_with_input(
            BenchmarkId::new("native-prefix-through-ready", corpus.label()),
            &corpus,
            |b, _| b.iter(|| black_box(native_ready_measurement(black_box(&mut terminal)))),
        );
        group.bench_with_input(
            BenchmarkId::new("native-full-history", corpus.label()),
            &corpus,
            |b, _| b.iter(|| black_box(native_full_measurement(black_box(&mut terminal)))),
        );
    }
    group.finish();
}

fn criterion_fanout(c: &mut Criterion) {
    let payload = Bytes::from(deterministic_page(0, 64 * 1024));
    let mut group = c.benchmark_group("server-native-raw-fanout");
    for clients in [1_usize, 2, 8] {
        group.throughput(Throughput::Bytes(
            u64::try_from(payload.len() * 256).expect("bounded throughput bytes"),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(clients),
            &clients,
            |b, &count| {
                b.iter(|| black_box(fanout_measurement(count, 256, &payload)));
            },
        );
    }
    group.finish();
}

fn checked_capture_gates() {
    let optimize_mode = build_info::optimize_mode().expect("libghostty build mode");
    assert_eq!(
        optimize_mode,
        OptimizeMode::ReleaseFast,
        "performance gates require a ReleaseFast libghostty build"
    );
    println!("metric=libghostty-optimize-mode mode={optimize_mode:?}");
    let ready_only = std::env::var_os("PHUX_CAPTURE_READY_ONLY").is_some();
    let mut history_ready_p95 = None;
    for corpus in Corpus::ALL {
        let mut terminal = build_terminal(corpus);
        for _ in 0..WARMUP_SAMPLES {
            black_box(native_ready_measurement(&mut terminal));
        }
        let mut native_ready = Vec::with_capacity(MEASURED_SAMPLES);
        let mut synthesized_ready = Vec::with_capacity(MEASURED_SAMPLES);
        let mut synthesized_full = Vec::with_capacity(MEASURED_SAMPLES);
        let mut ready_bytes = 0_usize;
        let mut synthesized_ready_bytes = 0_usize;
        let mut synthesized_full_bytes = 0_usize;
        for _ in 0..MEASURED_SAMPLES {
            let native = native_ready_measurement(&mut terminal);
            let synthesized = synthesized_measurement(&terminal);
            native_ready.push(native.ready);
            synthesized_ready.push(synthesized.ready);
            synthesized_full.push(synthesized.full_history);
            ready_bytes = native.ready_bytes;
            synthesized_ready_bytes = synthesized.ready_bytes;
            synthesized_full_bytes = synthesized.full_history_bytes;
            assert_eq!(
                native.payload_copies, 0,
                "native prefix copied payload bytes"
            );
        }
        let native_p95 = percentile(&mut native_ready, 95);
        let synth_p95 = percentile(&mut synthesized_ready, 95);
        let synth_full_p95 = percentile(&mut synthesized_full, 95);
        println!(
            "metric=codec-ready-p95 corpus={} clients=1 native_us={} synthesized_us={} native_bytes={ready_bytes} synthesized_ready_bytes={} synthesized_full_history_p95_us={} synthesized_full_history_bytes={}",
            corpus.label(),
            native_p95.as_micros(),
            synth_p95.as_micros(),
            synthesized_ready_bytes,
            synth_full_p95.as_micros(),
            synthesized_full_bytes,
        );
        if corpus == Corpus::Tui200x60 {
            Threshold {
                metric: "codec-ready-p95",
                corpus,
                clients: 1,
                comparison: Comparison::AtMost,
                observed: native_p95.as_secs_f64() * 1_000.0,
                limit: 25.0,
                unit: "ms",
            }
            .check()
            .unwrap_or_else(|error| panic!("{error}"));
        }
        if corpus == Corpus::Unicode50k {
            history_ready_p95 = Some(native_p95);
        }

        if corpus == Corpus::Unicode50k && !ready_only {
            let mut slice_latencies = Vec::with_capacity(WARMUP_SAMPLES);
            let mut full_latencies = Vec::with_capacity(WARMUP_SAMPLES);
            let mut max_slice_bytes = 0_usize;
            let mut full_bytes = 0_usize;
            let mut full_growths = 0_usize;
            let mut full_copies = 0_usize;
            for _ in 0..WARMUP_SAMPLES {
                let full = native_full_measurement(&mut terminal);
                slice_latencies.push(full.history_slice_max);
                full_latencies.push(full.full_history);
                max_slice_bytes = max_slice_bytes.max(full.history_slice_max_bytes);
                full_bytes = full.full_history_bytes;
                full_growths = full.caller_buffer_growths;
                full_copies = full.payload_copies;
                assert_eq!(
                    full_copies, 0,
                    "metric=native-full-payload-copies corpus=unicode-50k clients=1"
                );
            }
            let slice_p95 = percentile(&mut slice_latencies, 95);
            let full_p95 = percentile(&mut full_latencies, 95);
            println!(
                "metric=history-slice-p95 corpus={} clients=1 latency_us={} max_bytes={} full_history_p95_us={} full_history_bytes={} caller_buffer_growths={} payload_copies={}",
                corpus.label(),
                slice_p95.as_micros(),
                max_slice_bytes,
                full_p95.as_micros(),
                full_bytes,
                full_growths,
                full_copies,
            );
            Threshold {
                metric: "history-slice-p95",
                corpus,
                clients: 1,
                comparison: Comparison::AtMost,
                observed: slice_p95.as_secs_f64() * 1_000.0,
                limit: 4.0,
                unit: "ms",
            }
            .check()
            .unwrap_or_else(|error| panic!("{error}"));
            Threshold {
                metric: "history-slice-bytes",
                corpus,
                clients: 1,
                comparison: Comparison::AtMost,
                observed: max_slice_bytes as f64,
                limit: HISTORY_PAGE_LIMIT as f64,
                unit: "bytes",
            }
            .check()
            .unwrap_or_else(|error| panic!("{error}"));
        }
    }
    if ready_only {
        return;
    }

    let history = history_ready_p95.expect("50k history READY sample");
    let mut control_terminal = build_unicode_ready_control();
    for _ in 0..WARMUP_SAMPLES {
        black_box(native_ready_measurement(&mut control_terminal));
    }
    let mut control_samples = Vec::with_capacity(MEASURED_SAMPLES);
    for _ in 0..MEASURED_SAMPLES {
        control_samples.push(native_ready_measurement(&mut control_terminal).ready);
    }
    let control = percentile(&mut control_samples, 95);
    let history_ratio = history.as_secs_f64() / control.as_secs_f64().max(f64::EPSILON);
    println!(
        "metric=50k-ready-slowdown corpus={} clients=1 control_p95_us={} history_p95_us={} ratio={history_ratio:.3}",
        Corpus::Unicode50k.label(),
        control.as_micros(),
        history.as_micros(),
    );
    Threshold {
        metric: "50k-ready-slowdown",
        corpus: Corpus::Unicode50k,
        clients: 1,
        comparison: Comparison::LessThan,
        observed: history_ratio,
        limit: 1.10,
        unit: "ratio",
    }
    .check()
    .unwrap_or_else(|error| panic!("{error}"));
}

fn checked_fanout_gates() {
    let payload = Bytes::from(deterministic_page(1, 64 * 1024));
    let mut throughput = Vec::new();
    for clients in [1_usize, 2, 8] {
        for _ in 0..WARMUP_SAMPLES {
            black_box(fanout_measurement(clients, 256, &payload));
        }
        let mut elapsed = Vec::with_capacity(MEASURED_SAMPLES);
        let mut copies = 0_usize;
        let mut peak = 0_usize;
        for _ in 0..MEASURED_SAMPLES {
            let sample = fanout_measurement(clients, 256, &payload);
            elapsed.push(sample.elapsed);
            copies = copies.saturating_add(sample.payload_copies);
            peak = peak.max(sample.peak_retained_bytes);
            black_box(sample.delivered_bytes);
        }
        let p95 = percentile(&mut elapsed, 95);
        let bytes_per_second = (payload.len() * 256) as f64 / p95.as_secs_f64();
        println!(
            "metric=raw-fanout-throughput corpus={} clients={} bytes_per_second={:.0} copies={} peak_retained_bytes={peak}",
            Corpus::Tui200x60.label(),
            clients,
            bytes_per_second,
            copies,
        );
        Threshold {
            metric: "payload-copies-per-extra-native-subscriber",
            corpus: Corpus::Tui200x60,
            clients,
            comparison: Comparison::AtMost,
            observed: copies as f64,
            limit: 0.0,
            unit: "copies",
        }
        .check()
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            retained_budget_holds(payload.len(), 16 * 1024 * 1024, HISTORY_PAGE_LIMIT, peak,),
            "retained-memory budget failed: metric=peak-retained-memory corpus={} clients={} observed={} active={} cache={} two_chunks={}",
            Corpus::Tui200x60.label(),
            clients,
            peak,
            payload.len(),
            16 * 1024 * 1024,
            HISTORY_PAGE_LIMIT * 2,
        );
        throughput.push((clients, bytes_per_second));
    }
    let baseline = throughput[0].1;
    let eight = throughput[2].1;
    Threshold {
        metric: "eight-client-raw-throughput-ratio",
        corpus: Corpus::Tui200x60,
        clients: 8,
        comparison: Comparison::AtLeast,
        observed: eight / baseline,
        limit: 0.90,
        unit: "ratio",
    }
    .check()
    .unwrap_or_else(|error| panic!("{error}"));
}

async fn native_warm_attach(socket: &std::path::Path) -> Duration {
    let started = Instant::now();
    let mut stream = common::wait_for_raw_socket(socket, common::SOCKET_CONNECT_DEADLINE).await;
    let native = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    );
    common::send_frame(
        &mut stream,
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
    let (kind, hello) = common::recv_typed(&mut stream).await;
    assert_eq!(kind, TYPE_HELLO_OK);
    assert!(matches!(
        hello,
        FrameKind::HelloOk {
            selected_profile: BootstrapProfile::NativeState { .. },
            ..
        }
    ));
    common::send_frame(
        &mut stream,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::ByName("benchmark".to_owned()),
            viewport: ViewportInfo::new(200, 60),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        },
    )
    .await;
    let (kind, _) = common::recv_typed(&mut stream).await;
    assert_eq!(kind, TYPE_ATTACHED);
    let (kind, begin) = common::recv_typed(&mut stream).await;
    assert_eq!(kind, TYPE_BOOTSTRAP_BEGIN);
    assert!(matches!(begin, FrameKind::BootstrapBegin { .. }));
    loop {
        let (kind, frame) = common::recv_typed(&mut stream).await;
        match frame {
            FrameKind::BootstrapChunk { .. } => assert_eq!(kind, TYPE_BOOTSTRAP_CHUNK),
            FrameKind::BootstrapReady { .. } => {
                assert_eq!(kind, TYPE_BOOTSTRAP_READY);
                break;
            }
            other => panic!("unexpected warm native attach frame: {other:?}"),
        }
    }
    let ready = started.elapsed();
    common::send_frame(&mut stream, &FrameKind::Detach).await;
    let (kind, detached) = common::recv_typed(&mut stream).await;
    assert_eq!(kind, TYPE_DETACHED);
    assert!(matches!(detached, FrameKind::Detached));
    ready
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

fn dhat_allocation_probe() {
    let _profiler = dhat::Profiler::builder().testing().build();
    let before = dhat::HeapStats::get();
    let mut terminal = build_terminal(Corpus::Unicode50k);
    let capture = native_full_measurement(&mut terminal);
    let after = dhat::HeapStats::get();
    println!(
        "metric=rust-allocations corpus={} clients=1 blocks={} bytes={} peak_bytes={} caller_buffer_growths={} payload_copies={}",
        Corpus::Unicode50k.label(),
        after.total_blocks.saturating_sub(before.total_blocks),
        after.total_bytes.saturating_sub(before.total_bytes),
        after.max_bytes.saturating_sub(before.curr_bytes),
        capture.caller_buffer_growths,
        capture.payload_copies,
    );
}

fn main() {
    if std::env::var_os("PHUX_CAPTURE_DHAT").is_some() {
        dhat_allocation_probe();
        return;
    }
    if std::env::var_os("PHUX_CAPTURE_READY_ONLY").is_some() {
        checked_capture_gates();
        return;
    }
    if std::env::var_os("PHUX_CAPTURE_CHECK_ONLY").is_some() {
        checked_capture_gates();
        checked_fanout_gates();
        checked_warm_uds_and_resync_gate();
        return;
    }
    let mut criterion = Criterion::default()
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(5))
        .sample_size(30)
        .configure_from_args();
    criterion_capture(&mut criterion);
    criterion_fanout(&mut criterion);
    criterion.final_summary();
    drop(criterion);
    checked_capture_gates();
    checked_fanout_gates();
    checked_warm_uds_and_resync_gate();
}
