//! JSON adapter for the connected server's typed public operations.
use std::{io::Read, path::PathBuf, process::ExitCode};

use phux_client::{
    attach::connection::Connection,
    resource::show,
    terminal::{self, CreateError},
};
use phux_protocol::{
    api::{ErrorCode, ErrorDocument, OperationError, OperationOutcome, PublicOperation},
    ids::ResourceKey,
};
use serde_json::{Value, json};

use super::{ApiAction, ApiArgs, server_target::ServerSpec};

mod questions;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;

pub(crate) fn run(args: ApiArgs, socket: Option<PathBuf>) -> ExitCode {
    let action = match Action::prepare(args.action) {
        Ok(action) => action,
        Err((code, message)) => return fail(code, message, 2),
    };
    let spec = ServerSpec {
        socket,
        remote: args.remote,
    };
    let runtime = match super::build_cli_runtime() {
        Ok(runtime) => runtime,
        Err(error) => return action.target_error(ErrorCode::Unknown, error, 1),
    };
    let server = match spec.resolve_quiet(&runtime, "api") {
        Ok(server) => server,
        Err(error) => return action.resolution_error(error),
    };
    runtime.block_on(async {
        match action {
            Action::Questions(request) => request.run(&server.dial()).await,
            Action::Execution(request) => {
                super::resource::execution_result(&server.dial(), &request).await
            }
            Action::Invoke(request) => match server.connect().await {
                Ok(mut conn) => invoke(&mut conn, request).await,
                Err(error) => request.target_error(ErrorCode::Transport, error, 1),
            },
            Action::Catalog(operation) => match server.connect().await {
                Ok(mut conn) => catalog(&mut conn, operation.as_deref()).await,
                Err(error) => fail(ErrorCode::Transport, error, 1),
            },
        }
    })
}

enum Action {
    Catalog(Option<String>),
    Invoke(Request),
    Questions(questions::Request),
    Execution(phux_protocol::api::ExecutionResultRequest),
}

impl Action {
    fn prepare(action: ApiAction) -> Result<Self, (ErrorCode, String)> {
        match action {
            ApiAction::List => Ok(Self::Catalog(None)),
            ApiAction::Describe { operation } => Ok(Self::Catalog(Some(operation))),
            ApiAction::Call {
                operation,
                request_json,
            } => {
                let method = PublicOperation::parse(&operation).ok_or_else(|| {
                    (
                        ErrorCode::Unsupported,
                        format!("unknown public operation {operation:?}"),
                    )
                })?;
                let input = request_input(&request_json)
                    .map_err(|error| (ErrorCode::InvalidArgument, error))?;
                let parsed = match method {
                    PublicOperation::QuestionList
                    | PublicOperation::QuestionRespond
                    | PublicOperation::QuestionReceipt => {
                        questions::Request::parse(method, &input).map(Self::Questions)
                    }
                    PublicOperation::ExecutionResult => serde_json::from_str(&input)
                        .map(Self::Execution)
                        .map_err(|error| error.to_string()),
                    _ => parse_request(method, &input).map(Self::Invoke),
                };
                parsed.map_err(|error| (ErrorCode::InvalidArgument, error))
            }
        }
    }

    fn target_error(self, code: ErrorCode, error: impl std::fmt::Display, status: u8) -> ExitCode {
        match self {
            Self::Invoke(request) => request.target_error(code, error, status),
            Self::Questions(questions::Request::Receipt(request)) => {
                questions::receipt_unavailable(request.operation_id, error, status)
            }
            _ => fail(code, error, status),
        }
    }

    fn resolution_error(self, error: super::json_err::Refusal) -> ExitCode {
        let code = if error.exit == 2 {
            ErrorCode::InvalidArgument
        } else {
            ErrorCode::Transport
        };
        let mut message = error.err.message;
        if !error.err.remedy.is_empty() {
            message.push('\n');
            message.push_str(&error.err.remedy);
        }
        self.target_error(code, message, error.exit)
    }
}

async fn catalog(conn: &mut Connection, name: Option<&str>) -> ExitCode {
    let document = match terminal::describe(conn).await {
        Ok(document) => document,
        Err(error) => return fail(error.api_code(), error, 1),
    };
    let Some(name) = name else {
        return crate::output::json(&document);
    };
    let Some(description) = describe(&document, name) else {
        return fail(
            ErrorCode::Unsupported,
            format!("server does not describe {name:?}"),
            1,
        );
    };
    crate::output::json(&description)
}

enum Request {
    Create(phux_protocol::api::TerminalCreate),
    Operation(phux_protocol::api::TerminalOperation),
    Show(ResourceKey),
}

impl Request {
    fn validate(&self) -> Result<(), (String, &str)> {
        let (command, key) = match self {
            Self::Create(request) => (request.command(), request.idempotency_key.as_str()),
            Self::Operation(request) => (request.command(), request.idempotency_key.as_str()),
            Self::Show(_) => return Ok(()),
        };
        command.map(|_| ()).map_err(|error| (error, key))
    }

    fn target_error(self, code: ErrorCode, error: impl std::fmt::Display, status: u8) -> ExitCode {
        // The connected client validates before sending. If we couldn't connect,
        // distinguish invalid input from the failure to observe a valid receipt.
        if code == ErrorCode::Transport
            && let Err((error, key)) = self.validate()
        {
            return receipt_error(&CreateError::Invalid(error), key);
        }
        let (outcome, key) = match self {
            Self::Create(request) => (OperationOutcome::NotSent, Some(request.idempotency_key)),
            Self::Operation(request) => (OperationOutcome::Unknown, Some(request.idempotency_key)),
            Self::Show(_) => (OperationOutcome::NotSent, None),
        };
        report_error(code, error, outcome, key, status)
    }
}

fn parse_request(operation: PublicOperation, input: &str) -> Result<Request, String> {
    match operation {
        PublicOperation::TerminalCreate => serde_json::from_str(input)
            .map(Request::Create)
            .map_err(|error| error.to_string()),
        PublicOperation::TerminalOperation => serde_json::from_str(input)
            .map(Request::Operation)
            .map_err(|error| error.to_string()),
        PublicOperation::ResourceShow => serde_json::from_str::<String>(input)
            .map_err(|error| error.to_string())
            .and_then(|key| {
                ResourceKey::parse(&key)
                    .map(Request::Show)
                    .ok_or_else(|| "resource.show requires a full durable key".to_owned())
            }),
        _ => Err("not an organization operation".to_owned()),
    }
}

fn request_input(argument: &str) -> Result<std::borrow::Cow<'_, str>, String> {
    if argument == "-" {
        return read_limited(std::io::stdin().lock()).map(std::borrow::Cow::Owned);
    }
    check_request_size(argument)?;
    Ok(std::borrow::Cow::Borrowed(argument))
}

fn read_limited(reader: impl Read) -> Result<String, String> {
    let mut input = String::new();
    reader
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_string(&mut input)
        .map_err(|error| error.to_string())?;
    check_request_size(&input)?;
    Ok(input)
}

fn check_request_size(input: &str) -> Result<(), String> {
    if input.len() > MAX_REQUEST_BYTES {
        return Err("JSON input exceeds 1 MiB".into());
    }
    Ok(())
}

fn describe(document: &Value, name: &str) -> Option<Value> {
    let operation = document
        .get("operations")?
        .as_array()?
        .iter()
        .find(|operation| operation.get("name").and_then(Value::as_str) == Some(name))?;
    Some(json!({
        "schema_version": document["schema_version"],
        "source": document["source"],
        "authority": document["authority"],
        "instance": document["instance"],
        "authorization": document["authorization"],
        "operation": operation,
    }))
}

async fn invoke(conn: &mut Connection, request: Request) -> ExitCode {
    let (result, key) = match request {
        Request::Create(request) => (
            terminal::create(conn, &request).await,
            request.idempotency_key,
        ),
        Request::Operation(request) => (
            terminal::operation(conn, &request).await,
            request.idempotency_key,
        ),
        Request::Show(key) => {
            return match show::show_key_on(conn, key).await {
                Ok(result) => crate::output::json(&result),
                Err(error) => fail(error.api_code(), error, 1),
            };
        }
    };
    match result {
        Ok(receipt) => crate::output::json(&receipt),
        Err(error) => receipt_error(&error, &key),
    }
}

fn receipt_error(error: &CreateError, key: &str) -> ExitCode {
    eprintln!("{}", error.document(key));
    ExitCode::FAILURE
}

fn fail(code: ErrorCode, error: impl std::fmt::Display, status: u8) -> ExitCode {
    report_error(code, error, OperationOutcome::NotSent, None, status)
}

fn report_error(
    code: ErrorCode,
    error: impl std::fmt::Display,
    outcome: OperationOutcome,
    idempotency_key: Option<String>,
    status: u8,
) -> ExitCode {
    let document = ErrorDocument {
        schema_version: 1,
        error: OperationError {
            code,
            message: error.to_string(),
            outcome,
            idempotency_key,
        },
    };
    eprintln!("{}", serde_json::to_value(document).unwrap_or_default());
    ExitCode::from(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_keeps_the_authority_and_unknown_server_operations() {
        let catalog = json!({
            "schema_version": 1, "authority": "origin", "instance": "epoch",
            "operations": [{"name": "future.operation", "retry": "unknown"}]
        });
        let result = describe(&catalog, "future.operation").unwrap();
        assert_eq!(result["authority"], "origin");
        assert_eq!(result["instance"], "epoch");
        assert_eq!(result["operation"]["retry"], "unknown");
        assert!(describe(&catalog, "missing").is_none());
    }

    #[test]
    fn resource_call_refuses_live_handles_and_stdin_has_a_bound() {
        assert!(parse_request(PublicOperation::ResourceShow, "\"@7\"").is_err());
        assert!(read_limited(&vec![b'a'; MAX_REQUEST_BYTES + 1][..]).is_err());
    }

    #[test]
    fn stdin_limit_counts_utf8_bytes_and_rejects_invalid_encoding() {
        let input = "é".repeat(MAX_REQUEST_BYTES / 2);
        assert_eq!(read_limited(input.as_bytes()).unwrap(), input);
        assert!(read_limited([0xff].as_slice()).is_err());
    }
}
