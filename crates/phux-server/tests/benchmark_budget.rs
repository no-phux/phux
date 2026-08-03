#![cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#![allow(clippy::expect_used, missing_docs)]

#[path = "../benches/server_measure.rs"]
pub(crate) mod server_measure;
#[path = "../../../benchmarks/support.rs"]
pub(crate) mod support;

use bytes::Bytes;
use server_measure::{
    build_terminal, fanout_measurement, native_ready_measurement, retained_budget_holds,
    synthesized_measurement,
};
use support::{Comparison, Corpus, HISTORY_PAGE_LIMIT, Threshold, deterministic_page};

#[test]
fn small_capture_and_fanout_accounting_gate() {
    let mut terminal = build_terminal(Corpus::Shell80x24);
    let synthesized = synthesized_measurement(&terminal);
    assert!(
        synthesized.ready_bytes > 0,
        "metric=synthesized-ready-bytes corpus=shell-80x24 clients=1 observed=0"
    );

    let native = native_ready_measurement(&mut terminal);
    assert!(
        native.ready_bytes > 0,
        "metric=native-ready-bytes corpus=shell-80x24 clients=1 observed=0"
    );
    assert_eq!(
        native.payload_copies, 0,
        "metric=native-prefix-payload-copies corpus=shell-80x24 clients=1"
    );

    let payload = Bytes::from(deterministic_page(3, 4096));
    for clients in [1_usize, 2, 8] {
        let fanout = fanout_measurement(clients, 8, &payload);
        assert_eq!(
            fanout.delivered_bytes,
            payload.len() * 8 * clients,
            "metric=fanout-delivered-bytes corpus=shell-80x24 clients={clients}"
        );
        assert_eq!(
            fanout.payload_copies, 0,
            "metric=payload-copies-per-extra-native-subscriber corpus=shell-80x24 clients={clients}"
        );
        assert!(
            retained_budget_holds(
                native.ready_bytes,
                64 * 1024,
                HISTORY_PAGE_LIMIT,
                fanout.peak_retained_bytes,
            ),
            "metric=peak-retained-memory corpus=shell-80x24 clients={clients} observed={} active={} cache={} two_chunks={}",
            fanout.peak_retained_bytes,
            native.ready_bytes,
            64 * 1024,
            HISTORY_PAGE_LIMIT * 2,
        );
    }
}

#[test]
fn threshold_diagnostic_names_metric_corpus_and_client_count() {
    let error = Threshold {
        metric: "raw-throughput-ratio",
        corpus: Corpus::Unicode50k,
        clients: 8,
        comparison: Comparison::AtLeast,
        observed: 0.5,
        limit: 0.9,
        unit: "ratio",
    }
    .check()
    .expect_err("deliberate threshold failure");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("metric=raw-throughput-ratio"));
    assert!(diagnostic.contains("corpus=unicode-50k"));
    assert!(diagnostic.contains("clients=8"));
}
