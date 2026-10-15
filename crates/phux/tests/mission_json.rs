//! `phux mission --json` goldens, checked against the `phux.mission/v1` schema.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in an integration test"
)]

#[path = "common/runner.rs"]
mod runner;

#[path = "common/ambient.rs"]
mod common;

use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;

#[test]
fn bare_mission_prints_help() {
    let xdg = common::XdgDirs::fresh("mission");
    let help = mission(&xdg, &["mission"]);
    assert_eq!(help.status.code(), Some(0), "{}", stderr(&help));
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("Usage:"), "{text}");
    assert!(text.contains("phux.mission/v1"), "{text}");
    assert!(text.contains("add"), "{text}");
    assert!(!text.to_ascii_lowercase().contains("delegate"), "{text}");

    let flagged = mission(&xdg, &["mission", "--help"]);
    assert_eq!(flagged.status.code(), Some(0), "{}", stderr(&flagged));
    let flagged_text = String::from_utf8_lossy(&flagged.stdout);
    assert!(flagged_text.contains("phux.mission/v1"), "{flagged_text}");
    assert!(!flagged_text.to_ascii_lowercase().contains("delegate"));
}

#[test]
fn mission_refuses_a_socket_because_it_never_dials() {
    let xdg = common::XdgDirs::fresh("mission");
    let output = mission(
        &xdg,
        &[
            "mission",
            "list",
            "--socket",
            "/tmp/phux-mission-unused.sock",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("mission"), "{err}");
    assert!(err.contains("--socket"), "{err}");
}

#[test]
fn every_verb_json_matches_its_golden_and_the_schema() {
    let schema = schema_validator();
    let xdg = common::XdgDirs::fresh("mission");
    let add = mission(
        &xdg,
        &[
            "mission",
            "add",
            "Fix the prompt",
            "--repo",
            "phux",
            "--description",
            "details",
            "--operation-id",
            &"a".repeat(32),
            "--json",
        ],
    );
    assert_success("add", &add);
    check(&schema, "add", &add);

    let list = mission(&xdg, &["mission", "list", "--json"]);
    assert_success("list", &list);
    check(&schema, "list", &list);

    let show = mission(&xdg, &["mission", "show", "phux/1", "--json"]);
    assert_success("show", &show);
    check(&schema, "show", &show);

    let describe = mission(
        &xdg,
        &[
            "mission",
            "describe",
            "phux/1",
            "--description",
            "more detail",
            "--operation-id",
            &"b".repeat(32),
            "--json",
        ],
    );
    assert_success("describe", &describe);
    check(&schema, "describe", &describe);

    let archive = mission(
        &xdg,
        &[
            "mission",
            "archive",
            "phux/1",
            "--operation-id",
            &"c".repeat(32),
            "--json",
        ],
    );
    assert_success("archive", &archive);
    check(&schema, "archive", &archive);

    let restore = mission(
        &xdg,
        &[
            "mission",
            "restore",
            "phux/1",
            "--operation-id",
            &"d".repeat(32),
            "--json",
        ],
    );
    assert_success("restore", &restore);
    check(&schema, "restore", &restore);
    errors_and_replay(&schema, &xdg);
}

fn errors_and_replay(schema: &jsonschema::Validator, xdg: &common::XdgDirs) {
    let missing = mission(xdg, &["mission", "show", "phux/99", "--json"]);
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
    assert!(missing.stdout.is_empty());
    let error = serde_json::from_slice::<Value>(&missing.stderr).expect("error json");
    assert_eq!(error["schema"], "phux.mission/v1");
    assert_eq!(error["error"]["code"], "mission_not_found");
    assert_valid(schema, &error);

    let conflict = mission(
        xdg,
        &[
            "mission",
            "describe",
            "phux/1",
            "--title",
            "nope",
            "--expected-revision",
            "1",
            "--operation-id",
            &"e".repeat(32),
            "--json",
        ],
    );
    assert_eq!(conflict.status.code(), Some(1), "{}", stderr(&conflict));
    let conflict_doc = serde_json::from_slice::<Value>(&conflict.stderr).expect("error json");
    assert_eq!(conflict_doc["error"]["code"], "mission_revision_conflict");
    assert_valid(schema, &conflict_doc);

    let replay = mission(
        xdg,
        &[
            "mission",
            "add",
            "Fix the prompt",
            "--repo",
            "phux",
            "--description",
            "details",
            "--operation-id",
            &"a".repeat(32),
            "--json",
        ],
    );
    assert_success("replay", &replay);
    let replay_doc = serde_json::from_slice::<Value>(&replay.stdout).expect("replay json");
    assert_eq!(replay_doc["idempotent_replay"], true);
    assert_eq!(replay_doc["task"]["alias"], "phux/1");
    assert_valid(schema, &replay_doc);
}

fn mission(xdg: &common::XdgDirs, args: &[&str]) -> Output {
    let mut command = common::phux_cmd(runner::phux_bin());
    xdg.apply(&mut command);
    command.env("HOME", &xdg.state);
    command.args(args).output().expect("spawn phux")
}

fn assert_success(verb: &str, output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{verb} failed\nstdout {}\nstderr {}",
        String::from_utf8_lossy(&output.stdout),
        stderr(output)
    );
    assert!(output.stderr.is_empty(), "{verb} stderr {}", stderr(output));
}

fn check(schema: &jsonschema::Validator, verb: &str, output: &Output) {
    let value = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "{verb} stdout is not JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["schema"], "phux.mission/v1", "{verb}");
    assert_valid(schema, &value);
    let mut normalized = value;
    normalize(&mut normalized);
    let pretty = format!(
        "{}\n",
        serde_json::to_string_pretty(&normalized).expect("pretty")
    );
    let path = golden_dir().join(format!("{verb}.json"));
    if std::env::var_os("PHUX_UPDATE_MISSION_GOLDENS").is_some() {
        std::fs::create_dir_all(golden_dir()).expect("golden dir");
        std::fs::write(&path, &pretty).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("golden {} missing ({err}). Got:\n{pretty}", path.display()));
    assert_eq!(pretty, expected, "{verb} golden drifted");
}

fn assert_valid(schema: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|err| err.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{value} does not match the schema: {errors:?}"
    );
}

fn schema_validator() -> jsonschema::Validator {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../phux-mission/schema/phux.mission.v1.schema.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let schema: Value = serde_json::from_str(&text).expect("schema json");
    jsonschema::validator_for(&schema).expect("compile schema")
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mission_goldens")
}

fn normalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(id) = map.get("id").and_then(Value::as_str)
                && is_hex32(id)
            {
                map.insert("id".to_owned(), Value::String("<id>".to_owned()));
            }
            for key in ["created_at", "updated_at"] {
                if map.contains_key(key) {
                    map.insert(key.to_owned(), Value::String("<timestamp>".to_owned()));
                }
            }
            for child in map.values_mut() {
                normalize(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_hex32(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
