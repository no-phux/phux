//! Foreign projections of portable workflow acceptance and settlement.
use super::*;

/// Local creation acceptance. Host settlement arrives under these exact ids.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WireWorkflowSubmission {
    NotQueued {
        reason: String,
    },
    Queued {
        request_id: u32,
    },
    Journaled {
        request_id: Option<u32>,
        operation: String,
    },
}

impl From<phux_client_runtime::control::WorkflowSubmission> for WireWorkflowSubmission {
    fn from(value: phux_client_runtime::control::WorkflowSubmission) -> Self {
        use phux_client_runtime::control::WorkflowSubmission;
        match value {
            WorkflowSubmission::NotQueued { reason } => Self::NotQueued { reason },
            WorkflowSubmission::Queued { request_id } => Self::Queued { request_id },
            WorkflowSubmission::Journaled {
                request_id,
                operation,
            } => Self::Journaled {
                request_id,
                operation: topology::group_id_text(operation.as_bytes()),
            },
        }
    }
}

/// Settlement is separate from local queue acceptance; unknown never permits
/// submitting a replacement creation under a new identity.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WireTerminalCreationOutcome {
    LiveTarget { terminal_id: String },
    Refused { message: String },
    Unknown { message: String },
}

impl From<phux_client_runtime::control::TerminalCreationOutcome> for WireTerminalCreationOutcome {
    fn from(value: phux_client_runtime::control::TerminalCreationOutcome) -> Self {
        use phux_client_runtime::control::TerminalCreationOutcome;
        match value {
            TerminalCreationOutcome::LiveTarget { terminal_id } => Self::LiveTarget {
                terminal_id: id::encode(&terminal_id),
            },
            TerminalCreationOutcome::Refused { message } => Self::Refused { message },
            TerminalCreationOutcome::Unknown { message } => Self::Unknown { message },
        }
    }
}
