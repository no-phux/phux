//! Dedicated SDK-owned agent control and structured publication contract.
//!
//! Queued means transport/SDK submission only, never provider consumption.
#![allow(missing_docs)]

use crate::ids::ResourceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NATIVE_SCHEMA: &str = "phux.native-agent/v1";
pub const MAX_NATIVE_PAYLOAD_BYTES: usize = 16 * 1024;
/// Reserved explicit public launch recipe; never a binary/argv heuristic.
pub const NATIVE_HOST_SPEC_ENV: &str = "PHUX_NATIVE_HOST_SPEC";
pub const MAX_NATIVE_OPERATIONS: usize = 1024;
pub const MAX_NATIVE_PENDING: usize = 32;
pub const MAX_NATIVE_QUESTIONS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeIntent {
    Prompt {
        text: String,
    },
    Interrupt {
        turn_id: String,
    },
    Answer {
        question_id: String,
        response: serde_json::Value,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommand {
    pub operation_id: u64,
    pub activation: String,
    pub intent: NativeIntent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDisposition {
    Admitted,
    Queued,
    Completed,
    Refused,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCommandResult {
    pub operation_id: u64,
    pub disposition: NativeDisposition,
    pub detail: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCapabilities {
    pub prompt: bool,
    pub interrupt: bool,
    pub questions: bool,
    pub follow_up: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeQuestionItem {
    pub id: String,
    pub question: String,
    pub header: String,
    pub options: Vec<NativeOption>,
    pub multi: bool,
    pub recommended: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeQuestion {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub message: String,
    pub options: Vec<NativeOption>,
    pub questions: Vec<NativeQuestionItem>,
    pub fields: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeView {
    pub activation: String,
    pub backend: String,
    pub capabilities: NativeCapabilities,
    pub connected: bool,
    pub turn_id: Option<String>,
    pub provider_session_id: Option<String>,
    pub questions: Vec<NativeQuestion>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeState {
    Working,
    Blocked,
    Done,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeUpdate {
    View {
        view: NativeView,
    },
    QuestionOpened {
        question: NativeQuestion,
    },
    QuestionClosed {
        id: String,
        reason: String,
    },
    State {
        state: NativeState,
        #[serde(default)]
        turn_id: Option<String>,
    },
    Transcript {
        entry: serde_json::Value,
    },
    Notice {
        level: String,
        text: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePublicationReceipt {
    pub publication_id: u64,
    pub first_seq: u64,
    pub last_seq: u64,
}
/// Explicit native launch. `command` is the real provider executable/argv,
/// never the guardian executable or a shell expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnNativeAgent {
    pub session: String,
    pub backend: String,
    pub provider: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub sdk_runtime: Option<String>,
    pub node_bridge: Option<String>,
    #[serde(default)]
    pub native_id: Option<String>,
    /// An existing local dormant terminal's wire id, when already interned.
    #[serde(default)]
    pub guardian: Option<ResourceId>,
    /// Stable organization identity for imported dormant records with no
    /// live wire handle. Mutually exclusive with `guardian`.
    #[serde(default)]
    pub guardian_key: Option<crate::ids::ResourceKey>,
    /// Optional observed revision when targeting a dormant guardian.
    #[serde(default)]
    pub guardian_expected: Option<crate::ids::Revision>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnedNativeAgent {
    pub parent: ResourceId,
    pub agent: ResourceId,
    pub activation: String,
}
/// Separate host operations retain their own connection role and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeRequest {
    Spawn {
        spec: SpawnNativeAgent,
    },
    Command {
        agent_session_id: ResourceId,
        command: NativeCommand,
    },
    Register {
        agent_session_id: ResourceId,
        activation: String,
        binding_proof: String,
    },
    Receive {
        agent_session_id: ResourceId,
        activation: String,
        binding_proof: String,
        generation: u64,
    },
    Complete {
        agent_session_id: ResourceId,
        activation: String,
        binding_proof: String,
        generation: u64,
        result: NativeCommandResult,
    },
    Publish {
        agent_session_id: ResourceId,
        activation: String,
        binding_proof: String,
        generation: u64,
        publication_id: u64,
        update: NativeUpdate,
    },
    Close {
        agent_session_id: ResourceId,
        activation: String,
        binding_proof: String,
        generation: u64,
    },
}
/// Validate before allocating a queue slot or reserving a question responder.
pub fn validate_payload<T: Serialize>(value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_NATIVE_PAYLOAD_BYTES {
        return Err("native payload exceeds 16 KiB".into());
    }
    Ok(())
}
impl NativeIntent {
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
/// Validate normalized answers and explicit provider constraints before reservation.
/// Provider-specific form schemas remain the adapter's responsibility.
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
        "ask" => response
            .get("answers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|answers| {
                let mut ids = std::collections::BTreeSet::new();
                answers.len() == question.questions.len()
                    && answers.iter().all(|answer| {
                        let Some(id) = answer.get("id").and_then(serde_json::Value::as_str) else {
                            return false;
                        };
                        if !ids.insert(id) {
                            return false;
                        }
                        let Some(item) = question.questions.iter().find(|item| item.id == id)
                        else {
                            return false;
                        };
                        answer
                            .get("selected_options")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|selected| {
                                let mut options = std::collections::BTreeSet::new();
                                (item.multi || selected.len() <= 1)
                                    && selected.iter().all(|value| {
                                        value.as_str().is_some_and(|option| {
                                            options.insert(option)
                                                && item
                                                    .options
                                                    .iter()
                                                    .any(|entry| entry.id == option)
                                        })
                                    })
                            })
                            && answer.get("custom_input").is_none_or(|value| {
                                value.as_str().is_some_and(|text| {
                                    text.is_empty()
                                        || question
                                            .fields
                                            .get("allow_custom_input")
                                            .and_then(serde_json::Value::as_bool)
                                            != Some(false)
                                })
                            })
                    })
            }),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("response does not match the current provider question")
    }
}
