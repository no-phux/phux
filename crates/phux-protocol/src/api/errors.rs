//! Error vocabulary used by the real client projection and discovery schema.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! codes {
    ($($variant:ident => $code:literal),* $(,)?) => {
        /// Stable public workflow error code. Outcome, not code alone, decides recovery.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[allow(missing_docs, reason = "codes are paired with the structured outcome and message")]
        pub enum ErrorCode {
            $(#[serde(rename = $code)] $variant),*
        }
        impl ErrorCode {
            /// Stable JSON spelling.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $code),* }
            }
        }
    }
}
codes! {
    InvalidArgument => "invalid_argument", IncompleteReceipt => "incomplete_receipt",
    ReceiptNotRetained => "receipt_not_retained", OutcomeUnknown => "outcome_unknown",
    Unsupported => "unsupported", Transport => "transport", ServerError => "server_error",
    UnexpectedReply => "unexpected_reply", NoSuchTarget => "no_such_target",
    SelectorNotSingle => "selector_not_single", Unauthorized => "unauthorized",
    NotFound => "not_found", Conflict => "conflict", Stale => "stale", NotEmpty => "not_empty",
    NameTaken => "name_taken", ExecutionUncertain => "execution_uncertain",
    LeaseHeld => "lease_held", DuplicateOperation => "duplicate_operation",
    StoreFailure => "store_failure", Unknown => "unknown",
}
impl ErrorCode {
    /// Refusal mapping also owns `Refusal::as_str`; no separate discovery registry.
    #[must_use]
    pub const fn from_refusal(refusal: &crate::wire::frame::Refusal) -> Self {
        use crate::wire::frame::Refusal as R;
        match refusal {
            R::Unauthorized { .. } => Self::Unauthorized,
            R::NotFound => Self::NotFound,
            R::Conflict { .. } => Self::Conflict,
            R::Stale { .. } => Self::Stale,
            R::NotEmpty { .. } => Self::NotEmpty,
            R::Unsupported { .. } => Self::Unsupported,
            R::InvalidArgument { .. } => Self::InvalidArgument,
            R::NameTaken => Self::NameTaken,
            R::ExecutionUncertain => Self::ExecutionUncertain,
            R::LeaseHeld { .. } => Self::LeaseHeld,
            R::DuplicateOperation => Self::DuplicateOperation,
            R::StoreFailure { .. } => Self::StoreFailure,
            R::Unknown { .. } => Self::Unknown,
        }
    }
}

/// Meaning of failure of a public workflow invocation, not process completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    /// Validation/feature/dial failure before mutation send; no effect from this call.
    NotSent,
    /// Server refused this mutation before committing it.
    Refused,
    /// Intent committed, but client cannot interpret the complete receipt. Reconcile.
    Committed,
    /// Original mutation may have committed. Read receipts/records; never blindly replay.
    Unknown,
}

/// Shared structured error envelope emitted by CLI and inside MCP tool errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorDocument {
    /// Error document revision, 1.
    pub schema_version: u32,
    /// Failure details and recovery classification.
    pub error: OperationError,
}
/// One public workflow failure. Messages are diagnostic, not parseable codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationError {
    /// Stable code.
    pub code: ErrorCode,
    /// Human explanation.
    pub message: String,
    /// What is known about the original intention.
    pub outcome: OperationOutcome,
    /// Caller-supplied retry identity, if recoverable from the input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}
