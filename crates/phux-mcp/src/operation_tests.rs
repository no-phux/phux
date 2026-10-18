//! Regression probes for adapter-scoped mutation delivery.
use crate::{Dispatcher, operations::Operations, tools::ToolError};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;
const KEY: &str = "0123456789abcdef0123456789abcdef";
fn args(background: bool) -> Value {
    json!({ "resource_id": "@7", "text": "hello", "idempotency_key": KEY, "background": background })
}
struct Worker {
    execute: Dispatcher,
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}
fn worker() -> Worker {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (start, gate, count) = (started.clone(), release.clone(), calls.clone());
    let execute: Dispatcher = Arc::new(move |name, args| {
        let (start, gate, count) = (start.clone(), gate.clone(), count.clone());
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            start.notify_one();
            gate.notified().await;
            Ok(json!({ "tool": name, "args": args, "sent": true, "pane": "@7" }))
        })
    });
    Worker {
        execute,
        started,
        release,
        calls,
    }
}
#[tokio::test]
async fn background_mutation_is_observable_and_retries_execute_once() {
    let ledger = Arc::new(Operations::default());
    let worker = worker();
    let pending = ledger
        .call("phux_paste".into(), args(true), worker.execute.clone())
        .await
        .unwrap();
    assert_eq!(pending["operation_id"], KEY);
    assert_eq!(pending["state"], "pending");
    worker.started.notified().await;
    let observed = ledger
        .call(
            "phux_operation_status".into(),
            json!({"idempotency_key": KEY}),
            worker.execute.clone(),
        )
        .await
        .unwrap();
    assert_eq!(observed["state"], "pending");
    assert_eq!(observed["approval_check"]["tool"], "phux_approvals");
    worker.release.notify_one();
    let result = ledger
        .call("phux_paste".into(), args(false), worker.execute.clone())
        .await
        .unwrap();
    assert_eq!(result["args"]["target"], "@7");
    assert!(result["args"].get("idempotency_key").is_none());
    let replay = ledger
        .call("phux_paste".into(), args(false), worker.execute.clone())
        .await
        .unwrap();
    assert_eq!(result, replay);
    assert_eq!(worker.calls.load(Ordering::SeqCst), 1);
    let status = ledger
        .call(
            "phux_operation_status".into(),
            json!({"idempotency_key": KEY}),
            worker.execute.clone(),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "completed");
    assert_eq!(status["result"], result);
}
#[tokio::test]
async fn conflicts_refuse_and_cancelled_observers_do_not_cancel_keyed_mutations() {
    let ledger = Arc::new(Operations::default());
    let worker = worker();
    let (l, execute) = (ledger.clone(), worker.execute.clone());
    let observer =
        tokio::spawn(async move { l.call("phux_paste".into(), args(false), execute).await });
    worker.started.notified().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let mut changed = args(true);
    changed["text"] = json!("different");
    let conflict = ledger
        .call("phux_paste".into(), changed, worker.execute.clone())
        .await
        .unwrap_err();
    assert_eq!(conflict.document()["error"]["code"], "idempotency_conflict");
    worker.release.notify_one();
    assert!(
        ledger
            .call("phux_paste".into(), args(false), worker.execute.clone())
            .await
            .is_ok()
    );
    assert_eq!(worker.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn native_keys_are_forwarded_and_failures_are_cached_as_documents() {
    let ledger = Arc::new(Operations::default());
    let execute: Dispatcher = Arc::new(|_, args| Box::pin(async move { Ok(args) }));
    let result = ledger
        .call(
            "phux_spawn".into(),
            json!({ "idempotency_key": KEY }),
            execute,
        )
        .await
        .unwrap();
    assert_eq!(result["idempotency_key"], KEY);
    let ledger = Arc::new(Operations::default());
    let execute: Dispatcher = Arc::new(|_, _| Box::pin(async { Err(ToolError::new("refused")) }));
    assert!(
        ledger
            .call("phux_paste".into(), args(false), execute.clone())
            .await
            .is_err()
    );
    let result = ledger
        .call(
            "phux_operation_status".into(),
            json!({ "idempotency_key": KEY }),
            execute,
        )
        .await
        .unwrap();
    assert_eq!(result["state"], "failed");
    assert_eq!(result["error"]["error"]["code"], "tool_error");
}
#[tokio::test]
async fn full_ledger_never_evicts_an_earlier_mutation() {
    let ledger = Arc::new(Operations::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let execute: Dispatcher = Arc::new(move |_, args| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(args) })
    });
    let request = |key: String| json!({"target":"@7","text":"hello","idempotency_key":key});
    for number in 1..=128 {
        ledger
            .call(
                "phux_paste".into(),
                request(format!("{number:032x}")),
                execute.clone(),
            )
            .await
            .unwrap();
    }
    let replay = ledger
        .call(
            "phux_paste".into(),
            request(format!("{:032x}", 1)),
            execute.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay["text"], "hello");
    let refused = ledger
        .call(
            "phux_paste".into(),
            request("ffffffffffffffffffffffffffffffff".into()),
            execute,
        )
        .await
        .unwrap_err();
    assert_eq!(refused.document()["error"]["code"], "idempotency_capacity");
    assert_eq!(calls.load(Ordering::SeqCst), 128);
}

#[tokio::test]
async fn confirmation_and_invalid_arguments_never_start_a_worker() {
    let ledger = Arc::new(Operations::default());
    let worker = worker();
    let error = ledger
        .call(
            "phux_kill".into(),
            json!({ "resource_id":"@7", "background": true, "idempotency_key": KEY }),
            worker.execute.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.document()["error"]["code"], "confirmation_required");
    assert_eq!(
        error.document()["approval"]["retry"]["arguments"]["target"],
        "@7"
    );
    assert_eq!(
        error.document()["approval"]["retry"]["arguments"]["confirm"],
        true
    );
    for args in [
        json!({"background":true}),
        json!({"idempotency_key":null}),
        json!({"idempotency_key":"00000000000000000000000000000000"}),
        json!({"background":"yes"}),
    ] {
        assert!(
            ledger
                .call("phux_paste".into(), args, worker.execute.clone())
                .await
                .is_err()
        );
    }
    assert!(
        ledger
            .call(
                "phux_ls".into(),
                json!({"idempotency_key":KEY}),
                worker.execute.clone()
            )
            .await
            .is_err()
    );
    assert!(
        ledger
            .call(
                "phux_operation_status".into(),
                json!({"idempotency_key":KEY}),
                worker.execute.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(worker.calls.load(Ordering::SeqCst), 0);
}
