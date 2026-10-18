//! Adapter-scoped mutation retries and nonblocking observation. A key owns one
//! invocation for this adapter's lifetime; server-native keys are also forwarded.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::tools::{self, ToolError};
use crate::{DispatchFuture, Dispatcher};

const CAPACITY: usize = 128;
type Outcome = Result<Value, ToolError>;

#[derive(Clone)]
struct Operation {
    tool: String,
    args: Value,
    result: watch::Receiver<Option<Outcome>>,
}

#[derive(Default)]
pub(crate) struct Operations(Mutex<HashMap<String, Operation>>);

impl Operations {
    pub(crate) async fn call(
        self: &Arc<Self>,
        name: String,
        args: Value,
        execute: Dispatcher,
    ) -> Outcome {
        if name == "phux_operation_status" {
            return self.status(&args);
        }
        let mut args = crate::contracts::normalize_target(args)?;
        crate::annotations::require_confirmation(&name, &args)?;
        let background = take_background(&mut args)?;
        let Some(key) = retry_key(&name, &args, background)? else {
            return execute(name, args).await;
        };
        args["idempotency_key"] = json!(key);
        let mut result = self.start(&key, name, args, execute)?;
        if background {
            return Ok(pending(&key));
        }
        wait_result(&mut result).await
    }

    fn start(
        &self,
        key: &str,
        name: String,
        args: Value,
        execute: Dispatcher,
    ) -> Result<watch::Receiver<Option<Outcome>>, ToolError> {
        let mut operations = self
            .0
            .lock()
            .map_err(|_| failure("internal", "operation registry unavailable"))?;
        if let Some(operation) = operations.get(key) {
            if operation.tool != name || operation.args != args {
                return Err(failure(
                    "idempotency_conflict",
                    "this key already belongs to different arguments or a different tool",
                ));
            }
            return Ok(operation.result.clone());
        }
        if operations.len() >= CAPACITY {
            return Err(failure(
                "idempotency_capacity",
                "adapter retry ledger is full; no operation started. Reconnect only after reconciling pending operations",
            ));
        }
        let (sender, receiver) = watch::channel(None);
        operations.insert(
            key.to_owned(),
            Operation {
                tool: name.clone(),
                args: args.clone(),
                result: receiver.clone(),
            },
        );
        drop(operations);
        let mut forwarded = args;
        if !server_keyed(&name) {
            forwarded
                .as_object_mut()
                .map(|object| object.remove("idempotency_key"));
        }
        tokio::spawn(async move {
            // A failed or panicked worker is observable, never silently retried.
            let worker = tokio::spawn(execute(name, forwarded));
            let outcome = worker.await.unwrap_or_else(|_| {
                Err(failure(
                    "operation_lost",
                    "operation worker ended unexpectedly; inspect state before retrying",
                ))
            });
            sender.send_replace(Some(outcome));
        });
        Ok(receiver)
    }

    fn status(&self, args: &Value) -> Outcome {
        tools::strict_object(args, &["idempotency_key"], &["idempotency_key"])?;
        let raw = tools::required_str(args, "idempotency_key")?;
        crate::pane_tools::idempotency_key(raw)?;
        let key = raw.to_ascii_lowercase();
        let operations = self
            .0
            .lock()
            .map_err(|_| failure("internal", "operation registry unavailable"))?;
        let operation = operations.get(&key).ok_or_else(|| failure("operation_not_found", "no operation with this key in this adapter; do not blindly resubmit after reconnecting"))?.clone();
        drop(operations);
        let outcome = operation.result.borrow().clone();
        match outcome {
            Some(Ok(result)) => Ok(
                json!({ "schema_version": 1, "operation_id": key, "state": "completed", "result": result }),
            ),
            Some(Err(error)) => Ok(
                json!({ "schema_version": 1, "operation_id": key, "state": "failed", "error": error.document() }),
            ),
            None => {
                let mut doc = pending(&key);
                // This is discovery, not a claim that an unrelated hold belongs
                // to this operation. The server remains the approval authority.
                let socket = tools::socket_or_default(&operation.args);
                doc["approval_check"] = json!({ "tool": "phux_approvals", "arguments": { "socket": socket }, "requires": "caller with inventory and observe on Global approval records; delegate if refused" });
                doc["request"] = json!({ "tool": operation.tool, "arguments": operation.args });
                Ok(doc)
            }
        }
    }
}

fn retry_key(name: &str, args: &Value, background: bool) -> Result<Option<String>, ToolError> {
    let Some(raw) = args.get("idempotency_key").and_then(Value::as_str) else {
        if background || args.get("idempotency_key").is_some() {
            return Err(failure(
                "invalid_idempotency_key",
                "background calls require a nonzero 32-hex-digit idempotency_key",
            ));
        }
        return Ok(None);
    };
    crate::pane_tools::idempotency_key(raw)?;
    if !supports_mutation(name) {
        return Err(failure(
            "invalid_argument",
            "idempotency_key and background are only available on mutating tools",
        ));
    }
    Ok(Some(raw.to_ascii_lowercase()))
}

async fn wait_result(result: &mut watch::Receiver<Option<Outcome>>) -> Outcome {
    loop {
        let observed = result.borrow().clone();
        if let Some(outcome) = observed {
            return outcome;
        }
        result.changed().await.map_err(|_| {
            failure(
                "operation_lost",
                "operation worker ended without a result; inspect state before retrying",
            )
        })?;
    }
}

fn take_background(args: &mut Value) -> Result<bool, ToolError> {
    let object = args
        .as_object_mut()
        .ok_or_else(|| ToolError::new("tool arguments must be an object"))?;
    match object.remove("background") {
        None | Some(Value::Bool(false)) => Ok(false),
        Some(Value::Bool(true)) => Ok(true),
        Some(_) => Err(ToolError::new("`background` must be a boolean")),
    }
}

fn pending(key: &str) -> Value {
    json!({ "schema_version": 1, "operation_id": key, "state": "pending", "status_tool": "phux_operation_status" })
}

fn failure(code: &str, message: &str) -> ToolError {
    tools::contract_error(
        code,
        message,
        "inspect the operation and server state before retrying",
        2,
    )
}

pub(crate) fn supports_mutation(name: &str) -> bool {
    crate::tool_table::hints_for(name).is_some_and(|hints| !hints.read_only)
}

pub(crate) fn server_keyed(name: &str) -> bool {
    matches!(
        name,
        "phux_new" | "phux_spawn" | "phux_kill" | "phux_signal"
    )
}

pub(crate) fn dispatcher() -> Dispatcher {
    let operations = Arc::new(Operations::default());
    let execute: Dispatcher = Arc::new(|name, args| -> DispatchFuture {
        Box::pin(async move {
            tools::dispatch(&name, &args)
                .await
                .map(crate::contracts::result_document)
        })
    });
    Arc::new(move |name, args| {
        let operations = Arc::clone(&operations);
        let execute = Arc::clone(&execute);
        Box::pin(async move { operations.call(name, args, execute).await })
    })
}
