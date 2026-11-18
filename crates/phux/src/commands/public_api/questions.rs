//! The public JSON adapter for routed question operations.

use std::process::ExitCode;

use phux_client::{
    attach::connection::Dial,
    native_agent::{ClientError, questions},
};
use phux_protocol::{
    api::PublicOperation,
    native_agent::{
        NativeDisposition,
        questions::{QuestionReceipt, QuestionReceiptRequest, QuestionTarget, RespondQuestion},
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
                Err(error) => super::fail("question_read_failed", error, 1),
            },
            Self::Respond(answer) => match questions::respond_on(dial, answer).await {
                Ok(receipt) => report(&receipt),
                Err(error) => {
                    let outcome = if matches!(error, ClientError::Refused { .. }) {
                        "refused"
                    } else {
                        "not_sent"
                    };
                    eprintln!(
                        "{}",
                        serde_json::json!({"schema_version":1,"error":{
                            "code":"question_response_failed", "outcome":outcome,
                            "message":error.to_string()
                        }})
                    );
                    ExitCode::FAILURE
                }
            },
            Self::Receipt(request) => {
                match questions::receipt_on(dial, request.question, request.operation_id).await {
                    Ok(receipt) => report(&receipt),
                    Err(error) => {
                        eprintln!(
                            "{}",
                            serde_json::json!({"schema_version":1,"error":{
                                "code":"question_receipt_unavailable", "outcome":"unknown",
                                "operation_id":request.operation_id, "message":error.to_string()
                            }})
                        );
                        ExitCode::from(crate::exit_codes::EXIT_PARTIAL_VIEW)
                    }
                }
            }
        }
    }
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
