//! Editor schemas for `config.toml` and `phux-plugin.toml`, derived from the
//! serde types. `just docs-gen` writes them to `docs/spec/`, and a test fails
//! when the checked-in copies drift from the types.

use serde_json::{Map, Value};

/// The JSON Schema (draft-07, what taplo and VS Code read) for `config.toml`.
#[must_use]
pub fn config_schema() -> String {
    let mut schema = root::<crate::Config>();
    // Consumed by the layer merge before the schema sees the file.
    schema["properties"]["extends"] = serde_json::json!({
        "description": "Config layers merged under this file, in order: a path, or a \
                        name for `layers/<name>.toml` beside it.",
        "type": "array",
        "items": { "type": "string" },
    });
    render(&schema)
}

/// The JSON Schema for a plugin's `phux-plugin.toml`.
#[must_use]
pub fn plugin_schema() -> String {
    render(&crate::plugin::manifest_schema())
}

/// `T`'s root schema, with every array key's `-append` spelling beside it.
pub(crate) fn root<T: schemars::JsonSchema>() -> Value {
    let schema = schemars::generate::SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut value = schema.to_value();
    add_append_keys(&mut value);
    value
}

/// `x-append` appends to the inherited array `x` (`docs/config.md`), so each
/// array property gets an `-append` twin with the same shape.
fn add_append_keys(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::Object(properties)) = object.get_mut("properties") {
                let twins: Map<String, Value> = properties
                    .iter()
                    .filter(|(_, schema)| is_array(schema))
                    .map(|(key, schema)| (format!("{key}-append"), schema.clone()))
                    .collect();
                properties.extend(twins);
            }
            object.values_mut().for_each(add_append_keys);
        }
        Value::Array(items) => items.iter_mut().for_each(add_append_keys),
        _ => {}
    }
}

/// Whether a property schema's `type` is or includes `array`.
fn is_array(schema: &Value) -> bool {
    match &schema["type"] {
        Value::String(kind) => kind == "array",
        Value::Array(kinds) => kinds.iter().any(|kind| kind == "array"),
        _ => false,
    }
}

/// Pretty JSON with sorted keys and a trailing newline, as checked in.
/// Sorted by hand: whether `serde_json` keeps insertion order depends on
/// which features the rest of the build turned on.
fn render(schema: &Value) -> String {
    let mut text = serde_json::to_string_pretty(&sorted(schema)).unwrap_or_default();
    text.push('\n');
    text
}

/// `value` with every object's keys in byte order.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by_key(|(key, _)| *key);
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.clone(), sorted(v)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// The checked-in schemas match the types. Remedy: `just docs-gen`.
    #[test]
    fn checked_in_schemas_match_the_types() {
        let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
        for (file, schema) in [
            ("../../docs/spec/config.schema.json", super::config_schema()),
            ("../../docs/spec/plugin.schema.json", super::plugin_schema()),
        ] {
            let on_disk = std::fs::read_to_string(root.join(file)).unwrap_or_default();
            assert!(
                on_disk == schema,
                "{file} is stale; run `just docs-gen` and commit it"
            );
        }
    }

    #[test]
    fn the_config_schema_knows_defaults_keys_and_rejects_unknown_ones() {
        let schema: serde_json::Value =
            serde_json::from_str(&super::config_schema()).expect("valid JSON");
        let defaults = &schema["definitions"]["DefaultsCfg"];
        assert_eq!(defaults["additionalProperties"], false);
        assert!(defaults["properties"]["attach-session"].is_object());
        assert!(schema["properties"]["extends"].is_object());
        assert!(schema["properties"]["plugins-append"].is_object());
    }
}
