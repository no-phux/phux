//! Schema checks and deterministic golden rendering for mission JSON.

#![expect(
    clippy::redundant_pub_crate,
    reason = "parent-only test helpers; pub would violate unreachable_pub"
)]

use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;

use super::manifest_dir;

pub(super) fn check(schema: &jsonschema::Validator, verb: &str, output: &Output) {
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
    // Goldens pin fields and values, not object insertion order. Sort every
    // object explicitly, including objects nested in arrays, before rendering.
    normalized.sort_all_objects();
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

pub(super) fn assert_valid(schema: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|err| err.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{value} does not match the schema: {errors:?}"
    );
}

pub(super) fn schema_validator() -> jsonschema::Validator {
    let path = manifest_dir().join("../phux-mission/schema/phux.mission.v1.schema.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let schema: Value = serde_json::from_str(&text).expect("schema json");
    jsonschema::validator_for(&schema).expect("compile schema")
}

fn golden_dir() -> PathBuf {
    manifest_dir().join("tests/mission_goldens")
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
