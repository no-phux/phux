//! The JSON shown in docs/build-a-client.md is what phux prints.
//!
//! Each `<!-- contract: NAME -->` block in that page is compared with the
//! document the CLI builds: every key the page shows must still be there with
//! the same JSON type. New keys are allowed (the `--json` rule); a removed,
//! renamed or retyped key fails here, and the page must change with it.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_client::watch::{WatchEvent, event_json};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, EventStamp};
use serde_json::Value;

use phux_client::agent::detect::{
    PaneEvidence, SessionEvidence, agents_document, infer_agent_state,
};

const PAGE: &str = include_str!("../../../../../docs/build-a-client.md");

/// The JSON block after `<!-- contract: NAME -->` on the page.
fn documented(name: &str) -> Value {
    let marker = format!("<!-- contract: {name} -->");
    let after = PAGE
        .split_once(&marker)
        .unwrap_or_else(|| panic!("docs/build-a-client.md has no `{marker}`"))
        .1;
    let body = after
        .trim_start()
        .strip_prefix("```json\n")
        .unwrap_or_else(|| panic!("`{marker}` must be followed by a ```json block"));
    let block = body.split_once("\n```").expect("unterminated json block").0;
    serde_json::from_str(block).unwrap_or_else(|err| panic!("{name}: invalid JSON: {err}"))
}

/// Every key in `doc` is in `actual` with the same JSON type. A `null` in the
/// page is a nullable key: any value passes. An array's elements are each
/// checked against the page's first element.
fn assert_shape(path: &str, doc: &Value, actual: &Value) {
    match (doc, actual) {
        (Value::Object(doc), Value::Object(actual)) => {
            for (key, doc_value) in doc {
                let actual_value = actual
                    .get(key)
                    .unwrap_or_else(|| panic!("{path}.{key} is documented but not printed"));
                assert_shape(&format!("{path}.{key}"), doc_value, actual_value);
            }
        }
        (Value::Array(doc), Value::Array(actual)) => {
            if let Some(first) = doc.first() {
                assert!(
                    !actual.is_empty(),
                    "{path}: documented elements, none printed"
                );
                for (index, element) in actual.iter().enumerate() {
                    assert_shape(&format!("{path}[{index}]"), first, element);
                }
            }
        }
        (Value::Null, _)
        | (Value::String(_), Value::String(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::Bool(_), Value::Bool(_)) => {}
        _ => panic!("{path}: documented as {doc}, printed as {actual}"),
    }
}

/// Check the page's `name` block against a document the CLI built.
pub(super) fn assert_documented(name: &str, actual: &Value) {
    assert_shape(name, &documented(name), actual);
}

#[test]
fn agent_list_matches_the_page() {
    let mut evidence = PaneEvidence::for_test(
        "@1",
        Some("phux-ask[deploy-7]:Deploy to staging??s=yes|no"),
        &[],
    );
    evidence.cwd = Some("/home/me/src/app".to_owned());
    evidence.record = Some(AgentRecord {
        name: "reviewer".to_owned(),
        kind: Some("claude".to_owned()),
        state: AgentMetaState::Blocked,
        ..AgentRecord::default()
    });
    evidence.agent_session = Some(SessionEvidence {
        resource: "@2".to_owned(),
        provider: "claude".to_owned(),
        native_id: Some("4f0c2a9e".to_owned()),
        state: AgentMetaState::Blocked,
    });
    let mut report = infer_agent_state(&evidence, &[]);
    report.set_address(Some(Ok("%reviewer".to_owned())));
    assert_documented("agent-list", &agents_document(&[report]));
}

#[test]
fn asked_event_matches_the_page() {
    let event = WatchEvent {
        terminal: Some(ResourceId::local(1)),
        event: AgentEvent::Asked {
            id: "deploy-7".to_owned(),
            question: "Deploy to staging?".to_owned(),
            suggestions: vec!["yes".to_owned(), "no".to_owned()],
            elapsed_seconds: None,
        },
        stamp: Some(EventStamp::new(12, 1_794_690_502_155)),
    };
    assert_documented("asked-event", &event_json(&event));
}

#[test]
fn watch_cursor_matches_the_page() {
    let line =
        crate::commands::watch::cursor_json(Some("2b5af13f10d9a1f69188f055261b9722:12"), false);
    assert_documented("watch-cursor", &line);
}

/// The checker itself: a documented key that disappears, or changes type,
/// fails.
#[test]
fn a_removed_or_retyped_key_fails() {
    let doc = serde_json::json!({ "a": 1, "b": ["x"], "c": null });
    assert_shape(
        "ok",
        &doc,
        &serde_json::json!({ "a": 2, "b": ["y"], "c": {}, "new": true }),
    );
    let missing = std::panic::catch_unwind(|| {
        assert_shape("missing", &doc, &serde_json::json!({ "b": [], "c": null }));
    });
    assert!(missing.is_err());
    let retyped = std::panic::catch_unwind(|| {
        assert_shape(
            "retyped",
            &doc,
            &serde_json::json!({ "a": "1", "b": [], "c": null }),
        );
    });
    assert!(retyped.is_err());
}
