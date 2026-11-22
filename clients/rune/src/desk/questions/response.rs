//! What a draft submits, in each question kind's normalized shape. Ported
//! from the GPUI app's questions so both clients send the same thing, and
//! refuse the same things: nothing is coerced, guessed or preselected.

use std::collections::{BTreeMap, HashMap};

use phux_protocol::native_agent::{
    NativeDisposition, NativeOption, NativeQuestion, NativeQuestionItem,
};
use serde_json::{Value, json};

use super::Draft;

/// The response a Submit sends. Confirm, select and permission answer with
/// their own buttons, so they have none.
pub fn build(draft: &Draft) -> Result<Value, String> {
    let text = |id: &str| draft.text(id);
    match draft.question.kind.as_str() {
        "input" | "editor" => Ok(json!({"text": text("text")})),
        "ask" => ask(&draft.question.questions, &draft.choices, text),
        "form" => form(draft),
        _ => Err("Choose an explicit response above.".into()),
    }
}

/// Every item answered, with only options the provider offered.
pub fn ask(
    items: &[NativeQuestionItem],
    choices: &HashMap<String, Vec<String>>,
    text: impl Fn(&str) -> String,
) -> Result<Value, String> {
    let mut answers = Vec::with_capacity(items.len());
    for item in items {
        let selected = choices.get(&item.id).cloned().unwrap_or_default();
        let custom = text(&item.id);
        if selected.is_empty() && custom.trim().is_empty() {
            return Err(format!("Answer ‘{}’ before submitting.", item.question));
        }
        if (!item.multi && selected.len() > 1)
            || selected
                .iter()
                .any(|id| !item.options.iter().any(|option| &option.id == id))
        {
            return Err("The offered options changed. Select a current option.".into());
        }
        let mut answer = json!({"id": item.id, "selected_options": selected});
        if !custom.trim().is_empty() {
            answer["custom_input"] = Value::String(custom);
        }
        answers.push(answer);
    }
    Ok(json!({"answers": answers}))
}

fn form(draft: &Draft) -> Result<Value, String> {
    let properties = draft
        .question
        .fields
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("Provider form has no supported properties schema. Nothing was submitted.")?;
    let mut values = serde_json::Map::new();
    for (id, schema) in properties {
        if let Some(value) = form_field(draft, id, schema)? {
            values.insert(id.clone(), value);
        }
    }
    Ok(json!({"values": values}))
}

fn form_field(draft: &Draft, id: &str, schema: &Value) -> Result<Option<Value>, String> {
    let raw = draft.text(id);
    let selected = draft.choices.get(id);
    let array = schema.get("type").and_then(Value::as_str) == Some("array");
    if raw.is_empty() && selected.is_none() && !required(&draft.question, id) {
        return Ok(None);
    }
    let offered = draft.options.contains_key(id);
    if offered && selected.is_none_or(Vec::is_empty) && !array {
        return Err(format!("Choose an explicit option for ‘{id}’."));
    }
    let value = if offered && array {
        selected_array(selected.map(Vec::as_slice).unwrap_or(&[]))
    } else {
        let first = selected.and_then(|values| values.first());
        form_value(schema, &raw, first.map(String::as_str))
    };
    value.map(Some).map_err(|error| format!("{id}: {error}"))
}

/// One form value: an offered option as the exact JSON it was offered as,
/// a string as typed, and anything else only as valid JSON of its type.
pub fn form_value(schema: &Value, raw: &str, selected: Option<&str>) -> Result<Value, String> {
    if let Some(selected) = selected {
        return serde_json::from_str(selected).map_err(|_| "Invalid selected value".into());
    }
    let kind = schema.get("type").and_then(Value::as_str);
    match kind.unwrap_or("string") {
        "string" => Ok(Value::String(raw.to_owned())),
        "number" | "integer" | "boolean" | "object" | "array" => {
            let value: Value = serde_json::from_str(raw)
                .map_err(|_| "Enter a valid JSON value of the requested type".to_owned())?;
            let matches = match kind {
                Some("number") => value.is_number(),
                Some("integer") => value.is_i64() || value.is_u64(),
                Some("boolean") => value.is_boolean(),
                Some("object") => value.is_object(),
                _ => value.is_array(),
            };
            if matches {
                Ok(value)
            } else {
                Err("Value does not match the requested type".into())
            }
        }
        _ => Err("This provider field type is not supported. Nothing was submitted.".into()),
    }
}

pub fn required(question: &NativeQuestion, id: &str) -> bool {
    question
        .fields
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(id)))
}

/// A form's fields that offer a fixed set (`oneOf` consts, `enum`, or a
/// boolean), with each option's id the exact JSON of its value.
pub fn form_options(question: &NativeQuestion) -> BTreeMap<String, Vec<NativeOption>> {
    let properties = question.fields.get("properties").and_then(Value::as_object);
    properties
        .into_iter()
        .flatten()
        .filter_map(|(id, schema)| Some((id.clone(), schema_options(schema)?)))
        .collect()
}

pub fn schema_options(schema: &Value) -> Option<Vec<NativeOption>> {
    let schema = if schema.get("type").and_then(Value::as_str) == Some("array") {
        schema.get("items")?
    } else {
        schema
    };
    let shown = |value: &Value| {
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned)
    };
    let text =
        |variant: &Value, key: &str| variant.get(key).and_then(Value::as_str).map(str::to_owned);
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        return variants
            .iter()
            .map(|variant| {
                let value = variant.get("const")?;
                Some(NativeOption {
                    id: value.to_string(),
                    label: text(variant, "title").unwrap_or_else(|| shown(value)),
                    description: text(variant, "description"),
                })
            })
            .collect();
    }
    let values = if schema.get("type").and_then(Value::as_str) == Some("boolean") {
        vec![Value::Bool(true), Value::Bool(false)]
    } else {
        schema.get("enum").and_then(Value::as_array)?.clone()
    };
    let option = |value: Value| NativeOption {
        id: value.to_string(),
        label: shown(&value),
        description: None,
    };
    Some(values.into_iter().map(option).collect())
}

pub fn selected_array(selected: &[String]) -> Result<Value, String> {
    selected
        .iter()
        .map(|value| {
            serde_json::from_str::<Value>(value)
                .map_err(|_| "Invalid selected form option".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// How a submitted answer stands, from its receipt. Queued is submission,
/// not the provider taking it; nothing is ever resent automatically.
pub fn receipt(disposition: NativeDisposition, detail: Option<&str>) -> String {
    let stage = match disposition {
        NativeDisposition::Admitted => "Admitted by provider",
        NativeDisposition::Queued => "Queued, not yet confirmed consumed by provider",
        NativeDisposition::Completed => "Completed",
        NativeDisposition::Refused => "Refused. Nothing retried; draft preserved",
        NativeDisposition::Unknown => {
            "Outcome unknown. Read the agent before answering again; never retried"
        }
    };
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{stage}: {detail}"),
        None => stage.to_owned(),
    }
}
