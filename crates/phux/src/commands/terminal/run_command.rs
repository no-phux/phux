//! Direct launch-and-wait CLI composition. Nothing is typed into a pane.
use std::{path::PathBuf, process::ExitCode, time::Duration};

use phux_client::{
    attach::connection::Connection,
    org::{self, OrgError},
    resource::execution,
    terminal::{self, CreateError},
};
use phux_protocol::{
    api::{
        ExecutionOutcome, ExecutionResult, ExecutionResultRequest, ExecutionResultStatus,
        ExecutionState, MAX_EXECUTION_WAIT_SECS, TerminalCreate, TerminalCreated,
        TerminalOperation, hex16,
    },
    ids::{AuthorityId, SessionKey},
    wire::frame::OrgFilter,
};
use serde_json::{Value, json};

use crate::commands::{
    TerminalRunArgs,
    server_target::{ServerSpec, ServerTarget},
};
use crate::exit_codes::{EXIT_FAILURE, EXIT_RUN_TIMEOUT, EXIT_USAGE};

const LAUNCH_BUDGET: Duration = Duration::from_secs(30);
const RECOVERY_BUDGET: Duration = Duration::from_secs(5);
const EXIT_LAUNCH_FAILED: u8 = 126;

pub(super) fn run(
    runtime: &tokio::runtime::Runtime,
    socket: Option<PathBuf>,
    args: &TerminalRunArgs,
) -> ExitCode {
    let key = match args.idempotency_key.as_deref() {
        Some(raw) => match phux_client::spawn::parse_idempotency_key(raw) {
            Ok(key) => hex16(key.as_bytes()),
            Err(error) => {
                return invocation_error("invalid_argument", &error.to_string(), EXIT_USAGE);
            }
        },
        None => uuid::Uuid::new_v4().simple().to_string(),
    };
    if !(1..=MAX_EXECUTION_WAIT_SECS).contains(&args.timeout) || args.argv.is_empty() {
        return invocation_error(
            "invalid_argument",
            "supply argv and a timeout from 1 through 3600 seconds",
            EXIT_USAGE,
        );
    }
    let spec = ServerSpec {
        socket,
        remote: args.remote.clone(),
    };
    let target = match spec.resolve(runtime, "terminal run", true) {
        Ok(target) => target,
        Err(code) => return code,
    };
    let (document, code) = match runtime.block_on(execute(&target, args, key)) {
        Ok(outcome) => outcome,
        Err(error) => return invocation_error(error.code(), &error.to_string(), EXIT_FAILURE),
    };
    let output = crate::output::json(&document);
    if output == ExitCode::SUCCESS {
        ExitCode::from(code)
    } else {
        output
    }
}

async fn execute(
    target: &ServerTarget,
    args: &TerminalRunArgs,
    key: String,
) -> Result<(Value, u8), OrgError> {
    let (mut conn, authority, request) =
        tokio::time::timeout(RECOVERY_BUDGET, prepare(target, args, key))
            .await
            .map_err(|_| {
                OrgError::Unexpected(
                    "could not resolve destination within 5 seconds; nothing launched".to_owned(),
                )
            })??;
    let mut document = json!({
        "schema_version": 1,
        "authority": authority.to_string(),
        "session": request.session,
        "idempotency_key": request.idempotency_key,
        "receipt": null,
        "result": null,
    });
    let created = launch(target, &mut conn, authority, &request).await;
    drop(conn);
    let receipt = match created {
        Ok(receipt) => receipt,
        Err(failure) => {
            document["status"] = json!(failure.status);
            document["error"] = failure.error;
            return Ok((document, failure.exit));
        }
    };
    document["receipt"] = json!(receipt);
    let Some(pin) = receipt.execution_ref else {
        document["status"] = json!("execution_unknown");
        return Ok((document, EXIT_FAILURE));
    };
    let query = ExecutionResultRequest {
        execution: pin,
        timeout_secs: Some(args.timeout),
    };
    let exact = match execution::result_on(&target.dial(), &query).await {
        Ok(exact) => exact,
        Err(error) => {
            document["status"] = json!("execution_unknown");
            document["observation_error"] = json!(error.document().error);
            return Ok((document, EXIT_FAILURE));
        }
    };
    let (status, exit) = disposition(&exact);
    document["status"] = json!(status);
    document["result"] = json!(exact);
    Ok((document, exit))
}

async fn prepare(
    target: &ServerTarget,
    args: &TerminalRunArgs,
    idempotency_key: String,
) -> Result<(Connection, AuthorityId, TerminalCreate), OrgError> {
    let mut conn = target.connect().await.map_err(OrgError::Attach)?;
    let key = SessionKey::parse(args.session.strip_prefix('^').unwrap_or(&args.session));
    if key.is_none() && args.session.starts_with('^') {
        return Err(OrgError::NotFound(args.session.clone()));
    }
    let filter = key.map_or(OrgFilter::All, OrgFilter::Session);
    let inventory = org::fetch(&mut conn, filter, 1).await?;
    let session = key
        .map_or_else(
            || inventory.session_named(&args.session),
            |key| inventory.session(&key),
        )
        .ok_or_else(|| OrgError::NotFound(args.session.clone()))?;
    let mut request = TerminalCreate::new(session.key.to_string(), idempotency_key);
    request.argv.clone_from(&args.argv);
    request.cwd = args.cwd.clone().or_else(|| {
        session
            .attrs
            .iter()
            .find_map(|(key, value)| (key == "directory").then(|| value.clone()))
    });
    Ok((conn, inventory.authority, request))
}

struct LaunchFailure {
    status: &'static str,
    error: Value,
    exit: u8,
}

async fn launch(
    target: &ServerTarget,
    conn: &mut Connection,
    authority: AuthorityId,
    request: &TerminalCreate,
) -> Result<TerminalCreated, LaunchFailure> {
    let sent = tokio::time::timeout(LAUNCH_BUDGET, terminal::create(conn, request)).await;
    match sent {
        Ok(Ok(receipt)) if receipt.authority == authority.to_string() => return Ok(receipt),
        Ok(Err(error)) if !unknown_create(&error) => {
            let exit = if matches!(error, CreateError::Invalid(_)) {
                EXIT_USAGE
            } else {
                EXIT_FAILURE
            };
            let error = error.document(&request.idempotency_key)["error"].clone();
            return Err(LaunchFailure {
                status: if error["outcome"] == "not_sent" {
                    "launch_not_sent"
                } else {
                    "launch_refused"
                },
                error,
                exit,
            });
        }
        _ => {}
    }
    if let Some(receipt) = recover(target, authority, request).await {
        return Ok(receipt);
    }
    Err(LaunchFailure {
        status: "launch_unknown",
        error: json!({
            "code":"outcome_unknown",
            "outcome":"unknown",
            "message":"launch could not be confirmed; query terminal operation on the same owner using the returned session and idempotency key, without resending the mutation"
        }),
        exit: EXIT_FAILURE,
    })
}

const fn unknown_create(error: &CreateError) -> bool {
    !matches!(
        error,
        CreateError::Invalid(_)
            | CreateError::Organization(
                OrgError::Unsupported
                    | OrgError::Attach(_)
                    | OrgError::Refused(_)
                    | OrgError::Server { .. }
            )
    )
}

async fn recover(
    target: &ServerTarget,
    authority: AuthorityId,
    request: &TerminalCreate,
) -> Option<TerminalCreated> {
    let lookup = TerminalOperation {
        session: request.session.clone(),
        idempotency_key: request.idempotency_key.clone(),
    };
    let receipt = tokio::time::timeout(RECOVERY_BUDGET, async {
        let mut conn = target.connect().await.ok()?;
        terminal::operation(&mut conn, &lookup).await.ok()
    })
    .await
    .ok()??;
    (receipt.authority == authority.to_string()).then_some(receipt)
}

fn disposition(exact: &ExecutionResult) -> (&'static str, u8) {
    if exact.status == ExecutionResultStatus::TimedOut {
        return ("timed_out", EXIT_RUN_TIMEOUT);
    }
    let Some(observation) = &exact.observation else {
        return ("execution_unknown", EXIT_FAILURE);
    };
    match (observation.state, &observation.outcome) {
        (ExecutionState::Completed, Some(ExecutionOutcome::Exited { code })) => {
            ("exited", u8::try_from(*code).unwrap_or(EXIT_FAILURE))
        }
        (ExecutionState::Completed, Some(ExecutionOutcome::Signaled { signal })) => (
            "signaled",
            u8::try_from(128_i32.saturating_add(*signal)).unwrap_or(EXIT_FAILURE),
        ),
        (ExecutionState::Failed, _)
            if observation
                .diagnostic
                .is_some_and(|diagnostic| diagnostic.stage.before_requested_exec()) =>
        {
            ("launch_failed", EXIT_LAUNCH_FAILED)
        }
        _ => ("execution_unknown", EXIT_FAILURE),
    }
}

fn invocation_error(code: &str, message: &str, exit: u8) -> ExitCode {
    eprintln!(
        "{}",
        json!({"schema_version":1,"error":{"code":code,"message":message,"outcome":"not_sent"}})
    );
    ExitCode::from(exit)
}
