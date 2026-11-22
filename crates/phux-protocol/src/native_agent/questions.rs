//! Current question observations and exact structured responses.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{NativeCommandResult, NativeDisposition, NativeQuestion};
use crate::ids::{AuthorityId, ResourceId, ResourceKey};

/// Lookup by an interactive local handle or an exact durable resource key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuestionTarget {
    /// The current local handle, for discovery in the current connection.
    Handle {
        /// Exact resource handle.
        resource: ResourceId,
    },
    /// Durable resource in the connected authority.
    Key {
        /// Exact organization owning the resource.
        #[schemars(with = "String")]
        authority: AuthorityId,
        /// Durable resource key.
        #[schemars(with = "String")]
        resource: ResourceKey,
    },
}

/// An exact structured question in one conversation activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionRef {
    /// The organization that owns the conversation.
    #[schemars(with = "String")]
    pub authority: AuthorityId,
    /// The durable `AgentSession` resource key, never its guardian pane.
    #[schemars(with = "String")]
    pub conversation: ResourceKey,
    /// The exclusive controller activation, not the connection generation.
    pub activation: String,
    /// The adapter's callback identity within this activation.
    pub id: String,
    /// Opaque version of the callback's offered question and response constraints.
    #[schemars(length(min = 64, max = 64), regex(pattern = "^[0-9a-fA-F]{64}$"))]
    pub revision: String,
}

impl QuestionRef {
    /// Check the reference's local shape; the server verifies the actual identity.
    ///
    /// # Errors
    /// Empty identifiers or an invalid revision are refused.
    pub fn validate(&self) -> Result<(), String> {
        if self.activation.is_empty() || self.id.is_empty() {
            return Err("question and activation identities must be nonempty".into());
        }
        if self.revision.len() != 64 || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("question revision must be 64 hex digits".into());
        }
        Ok(())
    }
}

/// Legacy PTY evidence, not a structured callback or permission grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PtyQuestionSource {
    /// Inferred from the terminal screen.
    Screen,
    /// Reported via a terminal title marker.
    Sentinel,
    /// Reported by a local integration hook.
    Hook,
    /// Reported as lifecycle evidence by an agent stream.
    Stream,
}

/// One level observation. Evidence never implies capability or authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CurrentQuestion {
    /// A provider callback from an exclusive SDK controller.
    Provider {
        /// The exact reference to preserve across a user decision.
        reference: QuestionRef,
        /// Untrusted content and offered response shape.
        question: Box<NativeQuestion>,
        /// Whether the adapter offers structured responses.
        response_supported: bool,
        /// A response already holds this callback's reservation.
        response_reserved: bool,
    },
    /// Evidence of a question in an ordinary pane; no exact answer is available.
    PtyEvidence {
        /// How the question was observed.
        source: PtyQuestionSource,
        /// Producer's advisory id; empty for some terminal markers.
        id: String,
        /// Untrusted producer text.
        question: String,
        /// Untrusted display suggestions, not provider option identities.
        suggestions: Vec<String>,
    },
}

/// An authoritative level read for one resource, not a reconstructed event history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CurrentQuestions {
    /// JSON schema version.
    pub schema_version: u32,
    /// Organization that answered the query.
    #[schemars(with = "String")]
    pub authority: AuthorityId,
    /// Exact queried resource.
    #[schemars(with = "String")]
    pub resource: ResourceKey,
    /// What kind of question owner the server can currently observe.
    pub context: QuestionContext,
    /// All current questions for this resource.
    pub questions: Vec<CurrentQuestion>,
}

/// An empty inventory may mean quiet, not connected, or no answering adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestionContext {
    /// Exclusive SDK controller, whose open callbacks are listed.
    Provider {
        /// Whether the controller has published a live view.
        connected: bool,
    },
    /// Advisory observations are available on this ordinary terminal.
    Pty,
    /// No question owner on this resource (e.g. an SDK guardian pane).
    None,
}

/// Nonzero operation id in the controller ledger. Its decimal string form
/// preserves all 64 bits in JSON clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QuestionOperationId(std::num::NonZeroU64);

impl QuestionOperationId {
    /// Wrap a controller operation id, rejecting the reserved zero value.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match std::num::NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// The exact underlying controller operation id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<String> for QuestionOperationId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("operation_id must be a nonzero unsigned decimal string");
        }
        value
            .parse()
            .map(Self)
            .map_err(|_| "operation_id must be a nonzero unsigned decimal string")
    }
}

impl From<QuestionOperationId> for String {
    fn from(id: QuestionOperationId) -> Self {
        id.0.to_string()
    }
}

/// A human-selected answer, never translated into terminal input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RespondQuestion {
    /// The original operation identity to retain for reconciliation.
    #[schemars(with = "String", regex(pattern = "^[1-9][0-9]{0,19}$"))]
    pub operation_id: QuestionOperationId,
    /// Exact reference returned by the current-question read.
    pub question: QuestionRef,
    /// An explicit response matching the provider's constraints.
    pub response: serde_json::Value,
}

/// Read the retained receipt for one exact response without repeating it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionReceiptRequest {
    /// The original question reference.
    pub question: QuestionRef,
    /// The original operation identity.
    #[schemars(with = "String", regex(pattern = "^[1-9][0-9]{0,19}$"))]
    pub operation_id: QuestionOperationId,
}

/// Receipt for a structured response; an SDK queue is not provider consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionReceipt {
    /// JSON schema version.
    pub schema_version: u32,
    /// The original operation identity.
    #[schemars(with = "String", regex(pattern = "^[1-9][0-9]{0,19}$"))]
    pub operation_id: QuestionOperationId,
    /// Current observed stage.
    pub disposition: NativeDisposition,
    /// Whether a retained reservation awaits handoff. `null` when not known.
    pub awaiting_dispatch: Option<bool>,
    /// Factual explanation, when present.
    pub detail: Option<String>,
}

impl QuestionReceipt {
    /// Convert the correlated result into the lossless public JSON shape.
    #[must_use]
    pub fn new(
        operation_id: QuestionOperationId,
        result: NativeCommandResult,
        awaiting_dispatch: Option<bool>,
    ) -> Self {
        Self {
            schema_version: 1,
            operation_id,
            disposition: result.disposition,
            awaiting_dispatch,
            detail: result.detail,
        }
    }
}

/// Schema of the authoritative current-question document.
#[must_use]
pub fn current_questions_schema() -> serde_json::Value {
    serde_json::Value::from(schemars::schema_for!(CurrentQuestions))
}

/// Schema of the targeted current-question read.
#[must_use]
pub fn question_target_schema() -> serde_json::Value {
    serde_json::Value::from(schemars::schema_for!(QuestionTarget))
}

/// Schema of the response request.
#[must_use]
pub fn respond_question_schema() -> serde_json::Value {
    serde_json::Value::from(schemars::schema_for!(RespondQuestion))
}

/// Schema of the receipt lookup request.
#[must_use]
pub fn question_receipt_request_schema() -> serde_json::Value {
    serde_json::Value::from(schemars::schema_for!(QuestionReceiptRequest))
}

/// Schema of the correlated receipt.
#[must_use]
pub fn question_receipt_schema() -> serde_json::Value {
    serde_json::Value::from(schemars::schema_for!(QuestionReceipt))
}
