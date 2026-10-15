//! Dedicated SDK-owned agent control and structured publication contract.
//!
//! Queued means transport/SDK submission only, never provider consumption.
#![allow(missing_docs)]

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use crate::ids::ResourceId;

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
    Prompt { text: String },
    Interrupt { turn_id: String },
    Answer { question_id: String, response: serde_json::Value },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommand { pub operation_id: u64, pub activation: String, pub intent: NativeIntent }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDisposition { Admitted, Queued, Completed, Refused, Unknown }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCommandResult { pub operation_id: u64, pub disposition: NativeDisposition, pub detail: Option<String> }
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCapabilities { pub prompt: bool, pub interrupt: bool, pub questions: bool, pub follow_up: bool }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOption { pub id: String, pub label: String, pub description: Option<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeQuestionItem { pub id: String, pub question: String, pub header: String, pub options: Vec<NativeOption>, pub multi: bool, pub recommended: Option<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeQuestion { pub id: String, pub kind: String, pub title: String, pub message: String, pub options: Vec<NativeOption>, pub questions: Vec<NativeQuestionItem>, pub fields: serde_json::Value }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeView { pub activation: String, pub backend: String, pub capabilities: NativeCapabilities, pub connected: bool, pub turn_id: Option<String>, pub provider_session_id: Option<String>, pub questions: Vec<NativeQuestion> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeState { Working, Blocked, Done }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeUpdate {
    View { view: NativeView },
    QuestionOpened { question: NativeQuestion },
    QuestionClosed { id: String, reason: String },
    State { state: NativeState, #[serde(default)] turn_id: Option<String> },
    Transcript { entry: serde_json::Value },
    Notice { level: String, text: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePublicationReceipt { pub publication_id: u64, pub first_seq: u64, pub last_seq: u64 }
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
pub struct SpawnedNativeAgent { pub parent: ResourceId, pub agent: ResourceId, pub activation: String }
/// Separate host operations retain their own connection role and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeRequest {
    Spawn { spec: SpawnNativeAgent },
    Command { agent_session_id: ResourceId, command: NativeCommand },
    Register { agent_session_id: ResourceId, activation: String, binding_proof: String },
    Receive { agent_session_id: ResourceId, activation: String, binding_proof: String, generation: u64 },
    Complete { agent_session_id: ResourceId, activation: String, binding_proof: String, generation: u64, result: NativeCommandResult },
    Publish { agent_session_id: ResourceId, activation: String, binding_proof: String, generation: u64, publication_id: u64, update: NativeUpdate },
    Close { agent_session_id: ResourceId, activation: String, binding_proof: String, generation: u64 },
}
/// Validate before allocating a queue slot or reserving a question responder.
pub fn validate_payload<T: Serialize>(value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_NATIVE_PAYLOAD_BYTES { return Err("native payload exceeds 16 KiB".into()); }
    Ok(())
}
impl NativeIntent {
    pub fn validate(&self) -> Result<(), String> {
        validate_payload(self)?;
        match self {
            Self::Prompt { text } if text.trim().is_empty() => Err("prompt is empty".into()),
            Self::Interrupt { turn_id } if turn_id.is_empty() => Err("turn id is empty".into()),
            Self::Answer { question_id, .. } if question_id.is_empty() => Err("question id is empty".into()),
            _ => Ok(()),
        }
    }
}
