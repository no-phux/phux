//! Validating native intents and provider answers before anything is reserved.

use super::{NativeIntent, NativeQuestion, NativeQuestionItem, validate_payload};

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
