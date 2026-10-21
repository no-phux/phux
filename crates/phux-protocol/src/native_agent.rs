//! Dedicated SDK-owned agent control and structured publication contract.
//!
//! Queued means transport/SDK submission only, never provider consumption.

use crate::ids::ResourceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Reserved publication schema for the native controller projection.
pub const NATIVE_SCHEMA: &str = "phux.native-agent/v1";
/// Maximum serialized native request or publication payload.
pub const MAX_NATIVE_PAYLOAD_BYTES: usize = 16 * 1024;
/// Reserved explicit public launch recipe; never a binary/argv heuristic.
pub const NATIVE_HOST_SPEC_ENV: &str = "PHUX_NATIVE_HOST_SPEC";
/// Maximum retained operation identities per native owner.
pub const MAX_NATIVE_OPERATIONS: usize = 1024;
/// Maximum unresolved operations admitted to a controller queue.
pub const MAX_NATIVE_PENDING: usize = 32;
/// Maximum simultaneously open provider questions.
pub const MAX_NATIVE_QUESTIONS: usize = 32;

include!("native_agent/contracts.rs");

mod strict;

/// An immutable native request and its admitted canonical wire bytes.
///
/// Fallible serialization and the size limit are resolved before a request can
/// enter a queue. The infallible frame encoder only copies these bytes.
#[derive(Clone)]
pub struct NativeRequestPayload {
    inner: std::sync::Arc<NativeRequestData>,
}

struct NativeRequestData {
    request: NativeRequest,
    encoded: bytes::Bytes,
}

impl NativeRequestPayload {
    /// Serialize and admit a request before scheduling or reserving anything.
    ///
    /// # Errors
    ///
    /// Returns a serialization error or a payload larger than 16 KiB.
    pub fn new(request: NativeRequest) -> Result<Self, String> {
        let encoded = encode_payload(&request)?.into();
        Ok(Self {
            inner: std::sync::Arc::new(NativeRequestData { request, encoded }),
        })
    }

    /// Borrow the exact request represented by the immutable wire bytes.
    #[must_use]
    pub fn request(&self) -> &NativeRequest {
        &self.inner.request
    }

    /// Consume the wire carrier after admission and dispatch.
    #[must_use]
    pub fn into_request(self) -> NativeRequest {
        match std::sync::Arc::try_unwrap(self.inner) {
            Ok(data) => data.request,
            Err(data) => data.request.clone(),
        }
    }

    pub(crate) fn encoded(&self) -> &[u8] {
        &self.inner.encoded
    }

    pub(crate) fn decode(encoded: &[u8]) -> Result<Self, String> {
        if encoded.len() > MAX_NATIVE_PAYLOAD_BYTES {
            return Err("native payload exceeds 16 KiB".into());
        }
        let request: NativeRequest =
            serde_json::from_slice(encoded).map_err(|error| error.to_string())?;
        // The request side is strict: a field or value the tolerant DTOs
        // dropped or folded is refused, never run without it.
        let original: serde_json::Value =
            serde_json::from_slice(encoded).map_err(|error| error.to_string())?;
        let canonical = serde_json::to_value(&request).map_err(|error| error.to_string())?;
        if !strict::survives_round_trip(&original, &canonical) {
            return Err(strict::UNKNOWN_FIELD_OR_VALUE.to_owned());
        }
        // Canonicalize the parsed DTO: the wire data is exactly the request
        // inspected by policy.
        Self::new(request)
    }
}

impl std::fmt::Debug for NativeRequestPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner.request, formatter)
    }
}

impl PartialEq for NativeRequestPayload {
    fn eq(&self, other: &Self) -> bool {
        self.inner.request == other.inner.request
    }
}

impl Eq for NativeRequestPayload {}

/// Validate before allocating a queue slot or reserving a question responder.
///
/// # Errors
///
/// Returns a serialization failure or a payload larger than 16 KiB.
pub fn validate_payload<T: Serialize>(value: &T) -> Result<(), String> {
    encode_payload(value).map(|_| ())
}

fn encode_payload<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_NATIVE_PAYLOAD_BYTES {
        return Err("native payload exceeds 16 KiB".into());
    }
    Ok(bytes)
}
impl NativeIntent {
    /// Validate payload bounds and the required identity or prompt text.
    ///
    /// # Errors
    ///
    /// Returns an oversized payload, empty prompt, or absent turn/question id.
    pub fn validate(&self) -> Result<(), String> {
        validate_payload(self)?;
        match self {
            Self::Prompt { text } if text.trim().is_empty() => Err("prompt is empty".into()),
            Self::Interrupt { turn_id } if turn_id.is_empty() => Err("turn id is empty".into()),
            Self::Answer { question_id, .. } if question_id.is_empty() => {
                Err("question id is empty".into())
            }
            _ => Ok(()),
        }
    }
}
fn valid_ask_answers(question: &NativeQuestion, response: &serde_json::Value) -> bool {
    let Some(answers) = response
        .get("answers")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let mut ids = std::collections::BTreeSet::new();
    answers.len() == question.questions.len()
        && answers
            .iter()
            .all(|answer| valid_ask_answer(question, answer, &mut ids))
}
fn valid_ask_answer<'a>(
    question: &NativeQuestion,
    answer: &'a serde_json::Value,
    ids: &mut std::collections::BTreeSet<&'a str>,
) -> bool {
    let Some(id) = answer.get("id").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !ids.insert(id) {
        return false;
    }
    let Some(item) = question.questions.iter().find(|item| item.id == id) else {
        return false;
    };
    valid_selected_options(item, answer) && valid_custom_answer(question, answer)
}
fn valid_selected_options(item: &NativeQuestionItem, answer: &serde_json::Value) -> bool {
    let Some(selected) = answer
        .get("selected_options")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let mut options = std::collections::BTreeSet::new();
    (item.multi || selected.len() <= 1)
        && selected.iter().all(|value| {
            value.as_str().is_some_and(|option| {
                options.insert(option) && item.options.iter().any(|entry| entry.id == option)
            })
        })
}
fn valid_custom_answer(question: &NativeQuestion, answer: &serde_json::Value) -> bool {
    answer.get("custom_input").is_none_or(|value| {
        value.as_str().is_some_and(|text| {
            text.is_empty()
                || question
                    .fields
                    .get("allow_custom_input")
                    .and_then(serde_json::Value::as_bool)
                    != Some(false)
        })
    })
}

/// Validate normalized answers and explicit provider constraints before reservation.
/// Provider-specific form schemas remain the adapter's responsibility.
///
/// # Errors
///
/// Returns a response that does not match the exact current request's shape
/// or permitted option identities.
pub fn validate_answer(
    question: &NativeQuestion,
    response: &serde_json::Value,
) -> Result<(), &'static str> {
    if response
        .get("cancelled")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && response.as_object().is_some_and(|object| object.len() == 1)
    {
        return Ok(());
    }
    let valid = match question.kind.as_str() {
        "confirm" => response
            .get("confirmed")
            .is_some_and(serde_json::Value::is_boolean),
        "select" | "permission" => response
            .get("option_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| question.options.iter().any(|option| option.id == id)),
        "input" | "editor" => response
            .get("text")
            .is_some_and(serde_json::Value::is_string),
        "form" => response
            .get("values")
            .is_some_and(serde_json::Value::is_object),
        "ask" => valid_ask_answers(question, response),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("response does not match the current provider question")
    }
}
