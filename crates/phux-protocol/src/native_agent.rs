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

/// A human operation against one exact SDK activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeIntent {
    /// Submit text through the provider's SDK prompt operation.
    Prompt {
        /// The complete prompt, without terminal framing.
        text: String,
    },
    /// Interrupt the observed provider turn.
    Interrupt {
        /// The provider's exact current turn identity.
        turn_id: String,
    },
    /// Respond explicitly to a currently open provider request.
    Answer {
        /// The request's exact source identity.
        question_id: String,
        /// The human-selected response in the request's normalized shape.
        response: serde_json::Value,
    },
}
/// An idempotent intent whose identity must not be reused with another payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommand {
    /// Nonzero, exact unsigned 64-bit operation identity.
    pub operation_id: u64,
    /// The SDK owner's reserved activation.
    pub activation: String,
    /// The typed operation captured by this identity.
    pub intent: NativeIntent,
}
/// A factual operation stage; admission is distinct from model success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum NativeDisposition {
    /// The provider accepted the operation.
    Admitted = 0x00,
    /// Submission succeeded, but provider consumption is not acknowledged.
    Queued = 0x01,
    /// The provider reported completion of this operation.
    Completed = 0x02,
    /// This operation was refused.
    Refused = 0x03,
    /// The outcome is ambiguous and must not trigger automatic resend.
    Unknown = 0x04,
}

impl NativeDisposition {
    pub(crate) const fn to_u8(self) -> u8 {
        self as u8
    }

    pub(crate) const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::Admitted),
            0x01 => Some(Self::Queued),
            0x02 => Some(Self::Completed),
            0x03 => Some(Self::Refused),
            0x04 => Some(Self::Unknown),
            _ => None,
        }
    }
}
/// A correlated native operation receipt, never a PTY input receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCommandResult {
    /// Original operation identity.
    pub operation_id: u64,
    /// The observed submission or provider stage.
    pub disposition: NativeDisposition,
    /// Optional factual explanation; never an inferred success.
    pub detail: Option<String>,
}
/// Whether the provider advertises one independent native operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "bool", into = "bool")]
pub enum NativeSupport {
    /// The provider does not offer this operation.
    #[default]
    Unsupported,
    /// The provider offers this operation through its typed SDK.
    Supported,
}

impl NativeSupport {
    /// Whether the operation is available.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

impl From<bool> for NativeSupport {
    fn from(supported: bool) -> Self {
        if supported {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }
}

impl From<NativeSupport> for bool {
    fn from(support: NativeSupport) -> Self {
        support.is_supported()
    }
}

/// Independent provider capabilities, serialized as explicit JSON booleans.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCapabilities {
    /// Typed prompt submission.
    pub prompt: NativeSupport,
    /// Interruption of an observed turn.
    pub interrupt: NativeSupport,
    /// Structured human provider responses.
    pub questions: NativeSupport,
    /// Additional prompts while a provider turn is active.
    pub follow_up: NativeSupport,
}
/// A provider-supplied selectable option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOption {
    /// Opaque option identity, preserved in the response.
    pub id: String,
    /// Human-visible option label.
    pub label: String,
    /// Optional provider explanation.
    pub description: Option<String>,
}
/// One item in a structured multi-question request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeQuestionItem {
    /// Exact item identity.
    pub id: String,
    /// Provider question text.
    pub question: String,
    /// Short provider grouping label.
    pub header: String,
    /// The item's available choices.
    pub options: Vec<NativeOption>,
    /// Whether more than one option may be selected.
    pub multi: bool,
    /// Optional suggested option identity; never an automatic selection.
    pub recommended: Option<String>,
}
/// One live provider request requiring an explicit human response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeQuestion {
    /// Exact request identity within its activation.
    pub id: String,
    /// Normalized dialog kind, such as `confirm`, `permission`, or `form`.
    pub kind: String,
    /// Provider title.
    pub title: String,
    /// Provider explanation, treated as untrusted content.
    pub message: String,
    /// Choices for a selection or permission request.
    pub options: Vec<NativeOption>,
    /// Items for a multi-question request.
    pub questions: Vec<NativeQuestionItem>,
    /// Provider constraints, initial editor value, or scalar form schema.
    pub fields: serde_json::Value,
}
/// The current projection of one server-owned SDK session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeView {
    /// Reserved SDK activation; not a daemon incarnation.
    pub activation: String,
    /// The reserved controller backend identity.
    pub backend: String,
    /// Operations actually advertised by the provider adapter.
    pub capabilities: NativeCapabilities,
    /// Whether the controller remains available.
    pub connected: bool,
    /// Actual current provider turn, when known.
    pub turn_id: Option<String>,
    /// Actual provider session identity, never a fabricated local alias.
    pub provider_session_id: Option<String>,
    /// Independently live provider requests.
    pub questions: Vec<NativeQuestion>,
}
/// Observed provider turn state; finishing a turn does not close questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeState {
    /// The provider is processing a turn.
    Working,
    /// The provider is waiting for human action.
    Blocked,
    /// The turn ended; its session may remain available.
    Done,
}
/// A bounded, structured publication from the exclusive SDK controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeUpdate {
    /// Replace the current controller projection.
    View {
        /// The exact reserved identity and current capabilities.
        view: NativeView,
    },
    /// Publish a newly opened provider request.
    QuestionOpened {
        /// The provider's request and response constraints.
        question: NativeQuestion,
    },
    /// Withdraw one explicitly closed request.
    QuestionClosed {
        /// Exact request identity.
        id: String,
        /// Factual source-closure reason.
        reason: String,
    },
    /// Report turn progress independently of question lifetime.
    State {
        /// Observed turn state.
        state: NativeState,
        /// Actual provider turn identity, when available.
        #[serde(default)]
        turn_id: Option<String>,
    },
    /// Publish a privacy-filtered transcript envelope.
    Transcript {
        /// The normalized `phux.transcript/v1` envelope.
        entry: serde_json::Value,
    },
    /// Publish a bounded provider or host notice.
    Notice {
        /// Notice severity.
        level: String,
        /// Human-visible explanation, not an approval instruction.
        text: String,
    },
}
/// The committed stream range for an idempotent publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePublicationReceipt {
    /// Original controller publication identity.
    pub publication_id: u64,
    /// First committed stream sequence in this publication.
    pub first_seq: u64,
    /// Last committed stream sequence in this publication.
    pub last_seq: u64,
}
/// Explicit native launch. `command` is the real provider executable/argv,
/// never the guardian executable or a shell expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnNativeAgent {
    /// Exact organization session name.
    pub session: String,
    /// Explicit configured native backend.
    pub backend: String,
    /// Provider identity.
    pub provider: String,
    /// Provider executable and arguments, never an inferred shell command.
    pub command: Vec<String>,
    /// Executing host's working directory.
    pub cwd: Option<String>,
    /// Explicit host environment; portable recipes strip private bootstrap state.
    pub env: BTreeMap<String, String>,
    /// An actual provider session to resume.
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
/// The exact resources and activation created by a native launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnedNativeAgent {
    /// Durable guardian terminal.
    pub parent: ResourceId,
    /// Precreated native `AgentSession`.
    pub agent: ResourceId,
    /// Exclusive SDK activation.
    pub activation: String,
}
/// Separate host operations retain their own connection role and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeRequest {
    /// Create an explicitly configured native owner.
    Spawn {
        /// The executing host's launch specification.
        spec: SpawnNativeAgent,
    },
    /// Submit a human intent against an existing native owner.
    Command {
        /// Exact native `AgentSession`.
        agent_session_id: ResourceId,
        /// Original operation identity, activation, and intent.
        command: NativeCommand,
    },
    /// Register the exclusive guardian controller.
    Register {
        /// Precreated native `AgentSession`.
        agent_session_id: ResourceId,
        /// Reserved SDK activation.
        activation: String,
        /// Private bootstrap proof; never a portable recipe field.
        binding_proof: String,
    },
    /// Receive one admitted intent on the controller's receive connection.
    Receive {
        /// Controller-owned `AgentSession`.
        agent_session_id: ResourceId,
        /// Reserved SDK activation.
        activation: String,
        /// Private controller proof.
        binding_proof: String,
        /// Registration generation fencing stale receivers.
        generation: u64,
    },
    /// Report the factual outcome of an admitted operation.
    Complete {
        /// Controller-owned `AgentSession`.
        agent_session_id: ResourceId,
        /// Reserved SDK activation.
        activation: String,
        /// Private controller proof.
        binding_proof: String,
        /// Current registration generation.
        generation: u64,
        /// Exact operation identity and observed stage.
        result: NativeCommandResult,
    },
    /// Commit one idempotent bounded provider publication.
    Publish {
        /// Controller-owned `AgentSession`.
        agent_session_id: ResourceId,
        /// Reserved SDK activation.
        activation: String,
        /// Private controller proof.
        binding_proof: String,
        /// Current registration generation.
        generation: u64,
        /// Stable publication identity retained across transport loss.
        publication_id: u64,
        /// Structured update captured by this publication identity.
        update: NativeUpdate,
    },
    /// Explicitly close the controller source.
    Close {
        /// Controller-owned `AgentSession`.
        agent_session_id: ResourceId,
        /// Reserved SDK activation.
        activation: String,
        /// Private controller proof.
        binding_proof: String,
        /// Current registration generation.
        generation: u64,
    },
}

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
        let request = serde_json::from_slice(encoded).map_err(|error| error.to_string())?;
        // Canonicalize the parsed DTO: ignored future fields cannot survive as
        // wire data that differs from the request inspected by policy.
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
