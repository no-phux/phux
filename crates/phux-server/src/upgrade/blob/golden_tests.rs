//! Frozen upgrade blobs: what an already-released image hands this one.
//!
//! Field names are the format, and the reader cannot refuse after the
//! re-exec, so `tests/fixtures/upgrade-blob/` is append-only
//! (`scripts/check-upgrade-fixtures.sh`). Every fixture must read back with
//! nothing lost, which catches a renamed `#[serde(default)]` field: it parses
//! cleanly and drops its value. The newest must also re-serialize exactly,
//! so a new field fails here until a fixture freezes it.
//!
//! To freeze a change, `PHUX_BLESS_UPGRADE_BLOB=0002 cargo nextest
//! run -p phux-server --lib upgrade::blob::golden` writes the next fixture.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

use super::*;

/// Paths, indices dropped, that may stay null or empty in the newest fixture:
/// the rest must carry a value, or a rename of that field proves nothing.
/// Each entry here is a real gap in the proof: shrink it, never grow it.
const MAY_BE_ABSENT: &[&str] = &[
    "agent_sessions.native_publications.accepted.ask",
    "agent_sessions.native_publications.accepted.evidence",
    "native_agents.launch.guardian",
    "native_agents.launch.guardian_expected",
    "native_agents.launch.guardian_key",
    "native_agents.publications.view",
    "native_agents.view.questions",
    "org.resources.activation.outcome",
    "org.resources.provenance.created_by.client",
    "org.sessions.attrs.repository",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo and nextest"))
        .join("tests/fixtures/upgrade-blob")
}

/// Every frozen blob, oldest first.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let mut paths: Vec<_> = std::fs::read_dir(fixtures_dir())
        .expect("fixture directory")
        .map(|entry| entry.expect("fixture entry").path())
        .collect();
    paths.sort();
    let fixtures: Vec<_> = paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("file")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read(path).expect("read fixture"))
        })
        .collect();
    assert!(!fixtures.is_empty(), "no frozen upgrade blobs");
    fixtures
}

fn reread(bytes: &[u8]) -> Value {
    let blob = StateBlob::from_bytes(bytes).expect("a released blob must stay readable");
    serde_json::from_slice(&blob.to_bytes().expect("serialize")).expect("json")
}

/// Everything in `frozen` still present and equal in `current`, by path.
fn lost_values(frozen: &Value, current: &Value, path: &str, lost: &mut Vec<String>) {
    match (frozen, current) {
        (Value::Object(frozen), Value::Object(current)) => {
            for (key, value) in frozen {
                let path = format!("{path}.{key}");
                match current.get(key) {
                    Some(current) => lost_values(value, current, &path, lost),
                    None => lost.push(path),
                }
            }
        }
        (Value::Array(frozen), Value::Array(current)) if frozen.len() == current.len() => {
            for (index, value) in frozen.iter().enumerate() {
                lost_values(value, &current[index], &format!("{path}[{index}]"), lost);
            }
        }
        (frozen, current) if frozen == current => {}
        _ => lost.push(path.to_owned()),
    }
}

/// Sort each path, indices dropped, by whether anything fills it. One filled
/// occurrence exercises the field.
fn sort_paths(
    value: &Value,
    path: &str,
    filled: &mut BTreeSet<String>,
    absent: &mut BTreeSet<String>,
) {
    match value {
        Value::Null => drop(absent.insert(path.to_owned())),
        Value::Array(items) if items.is_empty() => drop(absent.insert(path.to_owned())),
        Value::Object(fields) if fields.is_empty() => drop(absent.insert(path.to_owned())),
        Value::Array(items) => {
            filled.insert(path.to_owned());
            for item in items {
                sort_paths(item, path, filled, absent);
            }
        }
        Value::Object(fields) => {
            filled.insert(path.to_owned());
            for (key, field) in fields {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                sort_paths(field, &child, filled, absent);
            }
        }
        _ => drop(filled.insert(path.to_owned())),
    }
}

#[test]
fn golden_blobs_read_back_with_nothing_lost() {
    for (name, bytes) in fixtures() {
        let frozen: Value = serde_json::from_slice(&bytes).expect("fixture is json");
        let mut lost = Vec::new();
        lost_values(&frozen, &reread(&bytes), "", &mut lost);
        assert!(
            lost.is_empty(),
            "{name}: a released image's values are lost: {lost:?}"
        );
    }
}

#[test]
fn golden_blobs_pass_the_preflight_a_resume_depends_on() {
    for (name, bytes) in fixtures() {
        preflight(&bytes).unwrap_or_else(|reason| panic!("{name}: {reason}"));
    }
}

#[test]
fn golden_newest_freezes_every_field_this_image_writes() {
    let (name, bytes) = fixtures().pop().expect("a fixture");
    let current = reread(&bytes);
    if let Ok(next) = std::env::var("PHUX_BLESS_UPGRADE_BLOB") {
        assert!(
            next.len() == 4 && next.bytes().all(|b| b.is_ascii_digit()),
            "fixtures sort by name, so number them like 0002"
        );
        let pretty = serde_json::to_vec_pretty(&current).expect("serialize");
        std::fs::write(fixtures_dir().join(format!("{next}.json")), pretty).expect("write");
        return;
    }
    let frozen: Value = serde_json::from_slice(&bytes).expect("fixture is json");
    assert_eq!(
        frozen, current,
        "{name} no longer matches what this image writes; freeze the change in a new fixture"
    );
}

#[test]
fn golden_newest_exercises_every_field() {
    let (name, bytes) = fixtures().pop().expect("a fixture");
    let (mut filled, mut absent) = (BTreeSet::new(), BTreeSet::new());
    let frozen = serde_json::from_slice(&bytes).expect("json");
    sort_paths(&frozen, "", &mut filled, &mut absent);
    let unexercised: Vec<_> = absent
        .difference(&filled)
        .filter(|path| !MAY_BE_ABSENT.contains(&path.as_str()))
        .collect();
    assert!(
        unexercised.is_empty(),
        "{name} leaves these unexercised: {unexercised:?}"
    );
}

#[test]
fn preflight_refuses_records_a_resume_would_die_on() {
    let (_, bytes) = fixtures().pop().expect("a fixture");
    let mut blob: Value = serde_json::from_slice(&bytes).expect("json");
    blob["org"]["resources"][0]["session"] = blob["org"]["authority"]["id"].clone();
    let broken = serde_json::to_vec(&blob).expect("serialize");
    let reason = preflight(&broken).expect_err("a resource outside any session");
    assert!(reason.contains("organization"), "{reason}");
}

#[test]
fn the_loss_check_catches_a_renamed_defaulted_field() {
    let frozen = serde_json::json!({"keep": 1, "old_name": {"value": 2}});
    let renamed = serde_json::json!({"keep": 1, "new_name": {"value": 2}});
    let mut lost = Vec::new();
    lost_values(&frozen, &renamed, "", &mut lost);
    assert_eq!(lost, [".old_name"]);
}
