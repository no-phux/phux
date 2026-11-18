//! The public JSON adapter for routed question operations.

use std::process::ExitCode;

use phux_client::{
    attach::connection::Dial,
    native_agent::{ClientError, questions},
};
use phux_protocol::{
    api::{OperationOutcome, PublicOperation},
    native_agent::{
        NativeDisposition,
        questions::{
            QuestionOperationId, QuestionReceipt, QuestionReceiptRequest, QuestionTarget,
            RespondQuestion,
        },
    },
};

pub(super) enum Request {
    List(QuestionTarget),
    Respond(RespondQuestion),
    Receipt(QuestionReceiptRequest),
}

impl Request {
    pub(super) fn parse(method: PublicOperation, input: &str) -> Result<Self, String> {
        match method {
            PublicOperation::QuestionList => serde_json::from_str(input).map(Self::List),
            PublicOperation::QuestionRespond => serde_json::from_str(input).map(Self::Respond),
            PublicOperation::QuestionReceipt => serde_json::from_str(input).map(Self::Receipt),
            _ => return Err("not a question operation".to_owned()),
        }
        .map_err(|error| error.to_string())
    }

    pub(super) async fn run(self, dial: &Dial) -> ExitCode {
        match self {
            Self::List(target) => match questions::current_on(dial, target).await {
                Ok(document) => crate::output::json(&document),
                Err(error) => fail(
                    "question_read_failed",
                    OperationOutcome::NotSent,
                    error,
                    None,
                    1,
                ),
            },
            Self::Respond(answer) => match questions::respond_on(dial, answer).await {
                Ok(receipt) => report(&receipt),
                Err(error) => {
                    let outcome = if matches!(error, ClientError::Refused { .. }) {
                        OperationOutcome::Refused
                    } else {
                        OperationOutcome::NotSent
                    };
                    fail("question_response_failed", outcome, error, None, 1)
                }
            },
            Self::Receipt(request) => {
                match questions::receipt_on(dial, request.question, request.operation_id).await {
                    Ok(receipt) => report(&receipt),
                    Err(error) => receipt_unavailable(
                        request.operation_id,
                        error,
                        crate::exit_codes::EXIT_PARTIAL_VIEW,
                    ),
                }
            }
        }
    }
}

pub(super) fn receipt_unavailable(
    operation_id: QuestionOperationId,
    error: impl std::fmt::Display,
    status: u8,
) -> ExitCode {
    fail(
        "question_receipt_unavailable",
        OperationOutcome::Unknown,
        error,
        Some(operation_id),
        status,
    )
}

fn fail(
    code: &str,
    outcome: OperationOutcome,
    error: impl std::fmt::Display,
    operation_id: Option<QuestionOperationId>,
    status: u8,
) -> ExitCode {
    let mut details = serde_json::json!({
        "code":code, "outcome":outcome, "message":error.to_string()
    });
    if let Some(id) = operation_id {
        details["operation_id"] = serde_json::json!(id);
    }
    eprintln!(
        "{}",
        serde_json::json!({"schema_version":1,"error":details})
    );
    ExitCode::from(status)
}

fn report(receipt: &QuestionReceipt) -> ExitCode {
    let status = crate::output::json(receipt);
    if status != ExitCode::SUCCESS {
        return status;
    }
    match receipt.disposition {
        NativeDisposition::Unknown => ExitCode::from(crate::exit_codes::EXIT_PARTIAL_VIEW),
        NativeDisposition::Refused => ExitCode::FAILURE,
        _ => status,
    }
}
