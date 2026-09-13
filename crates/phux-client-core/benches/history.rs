#![allow(
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::too_many_lines,
    missing_docs,
    reason = "assertion-bearing benchmark binary"
)]

#[path = "../../../benchmarks/support.rs"]
pub(crate) mod support;

use std::collections::VecDeque;
use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use phux_client_core::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, HistoryApplyOutcome,
};
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::{EffectBuffer, KernelEffect, KernelInput, SessionKernel};
use phux_protocol::caps::{
    BootstrapProfile, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use phux_protocol::{BootstrapId, ResourceId, StreamId};
use support::{
    Comparison, Corpus, HISTORY_PAGE_LIMIT, MEASURED_SAMPLES, Threshold, WARMUP_SAMPLES,
    deterministic_page, percentile,
};

#[cfg(feature = "native-engine")]
use libghostty_vt::{
    Terminal as GhosttyTerminal,
    snapshot::{CaptureEvent, CaptureOptions},
};
#[cfg(feature = "native-engine")]
use phux_client_core::engine::ghostty::GhosttyAdapter;

#[derive(Debug, Default)]
struct BenchReplica {
    active: Vec<u8>,
    active_limit: usize,
    active_cursor: usize,
    history: VecDeque<Vec<u8>>,
    history_bytes: usize,
    history_budget: usize,
    page_copies: usize,
}

#[derive(Debug, Default)]
struct BenchAdapter;

impl EngineAdapter for BenchAdapter {
    type Replica = BenchReplica;
    type Error = std::io::Error;

    fn start_replica(
        &mut self,
        _profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        Ok(BenchReplica {
            active_limit: usize::from(geometry.cols)
                .saturating_mul(usize::from(geometry.rows))
                .saturating_mul(4)
                .max(1),
            ..BenchReplica::default()
        })
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        replica.active.extend_from_slice(payload);
        Ok(BootstrapProgress::Ready)
    }

    fn configure_history_budget(
        &mut self,
        replica: &mut Self::Replica,
        max_bytes: usize,
        _max_rows: usize,
    ) -> Result<(), Self::Error> {
        replica.history_budget = max_bytes;
        Ok(())
    }

    fn finish_bootstrap(
        &mut self,
        _replica: &mut Self::Replica,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        Ok(BootstrapProgress::Ready)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        while replica.history_bytes.saturating_add(payload.len()) > replica.history_budget {
            let Some(evicted) = replica.history.pop_front() else {
                break;
            };
            replica.history_bytes = replica.history_bytes.saturating_sub(evicted.len());
        }
        let page = payload.to_vec();
        replica.history_bytes = replica.history_bytes.saturating_add(page.len());
        replica.history.push_back(page);
        replica.page_copies = replica.page_copies.saturating_add(1);
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(HistoryApplyOutcome {
            progress: BootstrapProgress::Ready,
            retained: true,
            authenticated_rows: declared_rows as usize,
        })
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        for byte in payload {
            if replica.active.len() < replica.active_limit {
                replica.active.push(*byte);
            } else {
                replica.active[replica.active_cursor] = *byte;
                replica.active_cursor = (replica.active_cursor + 1) % replica.active_limit;
            }
        }
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}

const fn native_profile() -> BootstrapProfile {
    BootstrapProfile::NativeState {
        codec: EngineCodec::LibghosttySnapshotV1,
        features: EngineFeatureSet::required_native(),
    }
}

const fn stream_profile() -> BootstrapStreamProfile {
    BootstrapStreamProfile::NativeState {
        codec: EngineCodec::LibghosttySnapshotV1,
    }
}

fn fixture(
    corpus: Corpus,
) -> (
    SessionKernel<BenchAdapter>,
    ResourceId,
    StreamId,
    BootstrapId,
    EffectBuffer,
) {
    let terminal = ResourceId::local(7);
    let stream = StreamId::new(11).expect("stream id");
    let bootstrap = BootstrapId::new(13).expect("bootstrap id");
    let mut kernel = SessionKernel::with_history_config(
        BenchAdapter,
        native_profile(),
        HistoryCacheConfig::default(),
    );
    let mut effects = EffectBuffer::with_capacity(8);
    let (cols, rows) = corpus.geometry();
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                profile: stream_profile(),
                geometry: CanonicalGeometry::new(cols, rows).expect("fixed geometry"),
                base_seq: 0,
            },
            &mut effects,
        )
        .expect("bootstrap begin");
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                chunk_seq: 0,
                payload: b"authenticated-ready",
            },
            &mut effects,
        )
        .expect("bootstrap chunk");
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                history_cursor: Some(b"cursor-0"),
            },
            &mut effects,
        )
        .expect("protocol READY");
    (kernel, terminal, stream, bootstrap, effects)
}

const fn page_bytes(corpus: Corpus) -> usize {
    match corpus {
        Corpus::Shell80x24 => 8 * 1024,
        Corpus::Tui200x60 => 64 * 1024,
        Corpus::Unicode50k => HISTORY_PAGE_LIMIT - 64,
    }
}

fn apply_first_page(
    kernel: &mut SessionKernel<BenchAdapter>,
    terminal: &ResourceId,
    stream: StreamId,
    bootstrap: BootstrapId,
    payload: &[u8],
    effects: &mut EffectBuffer,
) {
    kernel
        .update(
            KernelInput::HistoryPage {
                terminal_id: terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                page_seq: 1,
                rows: 256,
                payload,
                cursor: b"cursor-0",
                next_cursor: Some(b"cursor-1"),
            },
            effects,
        )
        .expect("history page");
}
fn replace_ready(
    kernel: &mut SessionKernel<BenchAdapter>,
    terminal: &ResourceId,
    stream: StreamId,
    replacement: BootstrapId,
    base_seq: u64,
    effects: &mut EffectBuffer,
) {
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: terminal,
                stream_id: stream,
                bootstrap_id: replacement,
                profile: stream_profile(),
                geometry: CanonicalGeometry::new(200, 60).expect("fixed geometry"),
                base_seq,
            },
            effects,
        )
        .expect("resync begin");
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: terminal,
                stream_id: stream,
                bootstrap_id: replacement,
                chunk_seq: 0,
                payload: b"replacement-ready",
            },
            effects,
        )
        .expect("resync chunk");
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: terminal,
                stream_id: stream,
                bootstrap_id: replacement,
                history_cursor: Some(b"replacement-cursor"),
            },
            effects,
        )
        .expect("resync READY");
}

fn criterion_history(c: &mut Criterion) {
    let mut group = c.benchmark_group("client-kernel-history");
    for corpus in Corpus::ALL {
        let payload = deterministic_page(0, page_bytes(corpus));
        group.throughput(Throughput::Bytes(
            u64::try_from(payload.len()).expect("bounded history page"),
        ));
        group.bench_with_input(
            BenchmarkId::new("slice-apply", corpus.label()),
            &payload,
            |b, page| {
                b.iter_batched(
                    || fixture(corpus),
                    |(mut kernel, terminal, stream, bootstrap, mut effects)| {
                        apply_first_page(
                            &mut kernel,
                            &terminal,
                            stream,
                            bootstrap,
                            black_box(page),
                            &mut effects,
                        );
                        black_box(kernel);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_function(
            BenchmarkId::new("input-to-paint-while-paging", corpus.label()),
            |b| {
                let (mut kernel, terminal, stream, bootstrap, mut effects) = fixture(corpus);
                let mut seq = 1_u64;
                b.iter(|| {
                    kernel
                        .update(
                            KernelInput::ResourceOutput {
                                terminal_id: &terminal,
                                stream_id: stream,
                                bootstrap_id: bootstrap,
                                seq,
                                payload: black_box(b"typed"),
                            },
                            &mut effects,
                        )
                        .expect("live output while history request is outstanding");
                    seq = seq.saturating_add(1);
                    assert!(
                        effects
                            .as_slice()
                            .iter()
                            .any(|effect| matches!(effect, KernelEffect::Damage(_)))
                    );
                    black_box(effects.as_slice());
                });
            },
        );
    }
    group.finish();
}

#[cfg(feature = "native-engine")]
struct NativePageFixture {
    ready: Vec<u8>,
    page: Vec<u8>,
    rows: u32,
}

#[cfg(feature = "native-engine")]
fn native_page_fixture() -> NativePageFixture {
    let mut terminal = GhosttyTerminal::new(200, 3).expect("native benchmark terminal");
    terminal
        .set_scrollback_max_lines(Some(5_000))
        .expect("scrollback rows");
    terminal
        .set_scrollback_max_bytes(None)
        .expect("scrollback bytes");
    for row in 0..3_000 {
        terminal.vt_write(format!("native-history-{row:04}\r\n").as_bytes());
    }
    let record_bytes = HISTORY_PAGE_LIMIT;
    let mut capture = terminal
        .capture_snapshot(CaptureOptions {
            max_record_bytes: record_bytes,
            max_pages: 1_000,
        })
        .expect("native capture");
    let mut buffer = vec![0; record_bytes];
    let mut ready = Vec::new();
    loop {
        let event = capture.next(&mut buffer).expect("READY record");
        ready.extend_from_slice(&buffer[..event.written()]);
        if matches!(event, CaptureEvent::Ready { .. }) {
            break;
        }
    }
    let mut capture = capture.detach().expect("detached history");
    let mut page = Vec::new();
    let rows = loop {
        let event = capture
            .next(&mut terminal, &mut buffer)
            .expect("history record");
        page.extend_from_slice(&buffer[..event.written()]);
        match event {
            CaptureEvent::HistoryPage {
                rows, remaining, ..
            } => {
                assert!(remaining > 0, "fixture needs at least two history pages");
                break u32::try_from(rows).expect("bounded page rows");
            }
            CaptureEvent::Scan | CaptureEvent::Record { .. } => {}
            other => panic!("expected history page, got {other:?}"),
        }
    };
    NativePageFixture { ready, page, rows }
}

#[cfg(feature = "native-engine")]
fn native_kernel_fixture(
    fixture: &NativePageFixture,
) -> (
    SessionKernel<GhosttyAdapter>,
    ResourceId,
    StreamId,
    BootstrapId,
    EffectBuffer,
) {
    let terminal = ResourceId::local(17);
    let stream = StreamId::new(19).expect("stream");
    let bootstrap = BootstrapId::new(23).expect("bootstrap");
    let limits = phux_protocol::caps::BootstrapLimits::default();
    let mut kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(limits),
        native_profile(),
        HistoryCacheConfig::default(),
    );
    let mut effects = EffectBuffer::with_capacity(8);
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                profile: stream_profile(),
                geometry: CanonicalGeometry::new(200, 3).expect("geometry"),
                base_seq: 0,
            },
            &mut effects,
        )
        .expect("native begin");
    for (chunk_seq, payload) in fixture.ready.chunks(64 * 1024).enumerate() {
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id: &terminal,
                    stream_id: stream,
                    bootstrap_id: bootstrap,
                    chunk_seq: u32::try_from(chunk_seq).expect("chunk sequence"),
                    payload,
                },
                &mut effects,
            )
            .expect("native READY chunk");
    }
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &terminal,
                stream_id: stream,
                bootstrap_id: bootstrap,
                history_cursor: Some(b"native-cursor"),
            },
            &mut effects,
        )
        .expect("native READY");
    (kernel, terminal, stream, bootstrap, effects)
}

#[cfg(feature = "native-engine")]
fn criterion_native_history(c: &mut Criterion) {
    let fixture = native_page_fixture();
    c.bench_function("client-kernel-history/native-engine-page", |b| {
        b.iter_batched(
            || native_kernel_fixture(&fixture),
            |(mut kernel, terminal, stream, bootstrap, mut effects)| {
                kernel
                    .update(
                        KernelInput::HistoryPage {
                            terminal_id: &terminal,
                            stream_id: stream,
                            bootstrap_id: bootstrap,
                            page_seq: 1,
                            rows: fixture.rows,
                            payload: black_box(&fixture.page),
                            cursor: b"native-cursor",
                            next_cursor: Some(b"native-cursor"),
                        },
                        &mut effects,
                    )
                    .expect("real native history page");
                black_box(kernel);
            },
            BatchSize::SmallInput,
        );
    });
}

#[cfg(feature = "native-engine")]
fn checked_native_history_gate() {
    let fixture = native_page_fixture();
    for _ in 0..WARMUP_SAMPLES {
        let (mut kernel, terminal, stream, bootstrap, mut effects) =
            native_kernel_fixture(&fixture);
        kernel
            .update(
                KernelInput::HistoryPage {
                    terminal_id: &terminal,
                    stream_id: stream,
                    bootstrap_id: bootstrap,
                    page_seq: 1,
                    rows: fixture.rows,
                    payload: &fixture.page,
                    cursor: b"native-cursor",
                    next_cursor: Some(b"native-cursor"),
                },
                &mut effects,
            )
            .expect("native history warmup");
    }
    let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
    for _ in 0..MEASURED_SAMPLES {
        let (mut kernel, terminal, stream, bootstrap, mut effects) =
            native_kernel_fixture(&fixture);
        let started = Instant::now();
        kernel
            .update(
                KernelInput::HistoryPage {
                    terminal_id: &terminal,
                    stream_id: stream,
                    bootstrap_id: bootstrap,
                    page_seq: 1,
                    rows: fixture.rows,
                    payload: &fixture.page,
                    cursor: b"native-cursor",
                    next_cursor: Some(b"native-cursor"),
                },
                &mut effects,
            )
            .expect("native history measurement");
        samples.push(started.elapsed());
    }
    let p95 = percentile(&mut samples, 95);
    println!(
        "metric=client-native-history corpus=real-ghostsnp-v1 clients=1 page_p95_us={} page_bytes={} authenticated_rows={} history_deferred=true",
        p95.as_micros(),
        fixture.page.len(),
        fixture.rows,
    );
    Threshold {
        metric: "client-native-history-page-p95",
        corpus: Corpus::Unicode50k,
        clients: 1,
        comparison: Comparison::AtMost,
        observed: p95.as_secs_f64() * 1_000.0,
        limit: 4.0,
        unit: "ms",
    }
    .check()
    .unwrap_or_else(|error| panic!("{error}"));
}

fn checked_history_gates() {
    for corpus in Corpus::ALL {
        let payload = deterministic_page(0, page_bytes(corpus));
        for _ in 0..WARMUP_SAMPLES {
            let (mut kernel, terminal, stream, bootstrap, mut effects) = fixture(corpus);
            apply_first_page(
                &mut kernel,
                &terminal,
                stream,
                bootstrap,
                &payload,
                &mut effects,
            );
            black_box(kernel);
        }
        let mut history_latencies = Vec::with_capacity(MEASURED_SAMPLES);
        let mut paint_latencies = Vec::with_capacity(MEASURED_SAMPLES);
        for _ in 0..MEASURED_SAMPLES {
            let (mut kernel, terminal, stream, bootstrap, mut effects) = fixture(corpus);
            let page_started = Instant::now();
            apply_first_page(
                &mut kernel,
                &terminal,
                stream,
                bootstrap,
                &payload,
                &mut effects,
            );
            history_latencies.push(page_started.elapsed());
            assert!(kernel.prefetch_history(&terminal, 0, &mut effects));

            let paint_started = Instant::now();
            kernel
                .update(
                    KernelInput::ResourceOutput {
                        terminal_id: &terminal,
                        stream_id: stream,
                        bootstrap_id: bootstrap,
                        seq: 1,
                        payload: b"typed",
                    },
                    &mut effects,
                )
                .expect("input echo while paging");
            assert!(
                effects
                    .as_slice()
                    .iter()
                    .any(|effect| matches!(effect, KernelEffect::Damage(_)))
            );
            paint_latencies.push(paint_started.elapsed());
        }
        let history_p95 = percentile(&mut history_latencies, 95);
        let paint_p99 = percentile(&mut paint_latencies, 99);
        println!(
            "metric=client-history corpus={} clients=1 slice_p95_us={} slice_bytes={} input_to_paint_p99_us={}",
            corpus.label(),
            history_p95.as_micros(),
            payload.len(),
            paint_p99.as_micros(),
        );
        for (metric, observed, limit) in [
            (
                "history-slice-p95",
                history_p95.as_secs_f64() * 1_000.0,
                4.0,
            ),
            (
                "input-to-paint-p99-while-paging",
                paint_p99.as_secs_f64() * 1_000.0,
                50.0,
            ),
        ] {
            Threshold {
                metric,
                corpus,
                clients: 1,
                comparison: Comparison::AtMost,
                observed,
                limit,
                unit: "ms",
            }
            .check()
            .unwrap_or_else(|error| panic!("{error}"));
        }
        Threshold {
            metric: "history-slice-bytes",
            corpus,
            clients: 1,
            comparison: Comparison::AtMost,
            observed: payload.len() as f64,
            limit: HISTORY_PAGE_LIMIT as f64,
            unit: "bytes",
        }
        .check()
        .unwrap_or_else(|error| panic!("{error}"));
    }
}

fn checked_memory_and_resync_gate() {
    let corpus = Corpus::Unicode50k;
    let config = HistoryCacheConfig::default();
    let (mut kernel, terminal, stream, bootstrap, mut effects) = fixture(corpus);
    let mut cursor = b"cursor-0".to_vec();
    let payload = deterministic_page(1, HISTORY_PAGE_LIMIT - 64);
    for page_index in 1..=64_u64 {
        let next = format!("cursor-{page_index}").into_bytes();
        kernel
            .update(
                KernelInput::HistoryPage {
                    terminal_id: &terminal,
                    stream_id: stream,
                    bootstrap_id: bootstrap,
                    page_seq: 1,
                    rows: 256,
                    payload: &payload,
                    cursor: &cursor,
                    next_cursor: Some(&next),
                },
                &mut effects,
            )
            .expect("bounded history page");
        cursor = next;
        if page_index != 64 {
            assert!(kernel.prefetch_history(&terminal, 0, &mut effects));
        }
    }
    let cache_bytes = kernel
        .history_cache(&terminal)
        .expect("history cache")
        .status()
        .loaded_bytes;
    let replica = kernel
        .published_engine(&terminal)
        .expect("published replica");
    let active_bytes = replica
        .active
        .capacity()
        .saturating_add(replica.history_bytes);
    let observed_peak = active_bytes
        .saturating_add(cache_bytes)
        .saturating_add(payload.len().saturating_mul(2));
    let allowed_peak = active_bytes
        .saturating_add(config.max_bytes)
        .saturating_add(HISTORY_PAGE_LIMIT.saturating_mul(2));
    assert!(
        observed_peak <= allowed_peak,
        "metric=peak-retained-memory corpus={} clients=1 observed={} active={} cache_limit={} two_chunks={}",
        corpus.label(),
        observed_peak,
        active_bytes,
        config.max_bytes,
        HISTORY_PAGE_LIMIT * 2,
    );
    println!(
        "metric=client-retained-memory corpus={} clients=1 active_bytes={} cache_bytes={} transient_two_chunks={} peak_bytes={} page_copies={}",
        corpus.label(),
        active_bytes,
        cache_bytes,
        payload.len() * 2,
        observed_peak,
        replica.page_copies,
    );

    let (mut warm_kernel, warm_terminal, warm_stream, _warm_bootstrap, mut warm_effects) =
        fixture(corpus);
    let warmup_samples = u64::try_from(WARMUP_SAMPLES).expect("warmup count fits u64");
    for generation in 1..=warmup_samples {
        replace_ready(
            &mut warm_kernel,
            &warm_terminal,
            warm_stream,
            BootstrapId::new(100 + generation).expect("warmup replacement id"),
            generation,
            &mut warm_effects,
        );
    }
    let mut convergence = Vec::with_capacity(MEASURED_SAMPLES);
    let measured_samples = u64::try_from(MEASURED_SAMPLES).expect("sample count fits u64");
    for generation in 1..=measured_samples {
        let replacement = BootstrapId::new(1_000 + generation).expect("replacement id");
        let started = Instant::now();
        replace_ready(
            &mut kernel,
            &terminal,
            stream,
            replacement,
            1_000 + generation,
            &mut effects,
        );
        convergence.push(started.elapsed());
    }
    let p99 = percentile(&mut convergence, 99);
    println!(
        "metric=client-resync-convergence corpus={} clients=1 p99_us={}",
        corpus.label(),
        p99.as_micros(),
    );
    Threshold {
        metric: "client-resync-convergence",
        corpus,
        clients: 1,
        comparison: Comparison::AtMost,
        observed: p99.as_secs_f64() * 1_000.0,
        limit: 250.0,
        unit: "ms",
    }
    .check()
    .unwrap_or_else(|error| panic!("{error}"));
}

fn main() {
    let mut criterion = Criterion::default()
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(5))
        .sample_size(40)
        .configure_from_args();
    criterion_history(&mut criterion);
    #[cfg(feature = "native-engine")]
    criterion_native_history(&mut criterion);
    criterion.final_summary();
    drop(criterion);
    checked_history_gates();
    checked_memory_and_resync_gate();
    #[cfg(feature = "native-engine")]
    checked_native_history_gate();
}
