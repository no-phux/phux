//! The stdio JSON-RPC loop: one writer, abortable `tools/call` tasks.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::{AbortHandle, JoinSet};

use super::{Dispatcher, MCP_PROTOCOL_VERSION, default_dispatcher};
use crate::contracts::tool_content;
use crate::jsonrpc::{
    INTERNAL_ERROR, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, REQUEST_CANCELLED, Request,
};
use crate::tools;

/// Drive the protocol with each tool call an abortable task. Only this loop
/// writes, so output stays framed while replies complete out of order.
pub(super) async fn serve_io<R, W>(
    mut reader: R,
    mut writer: W,
    dispatcher: Dispatcher,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    // Tools such as `phux_send_keys` connect through `phux-client`, which
    // would otherwise announce the headless `phux-cli` token.
    phux_client::attach::connection::install_client_name(Some(
        phux_client::attach::connection::product_client_name(
            phux_client::attach::connection::MCP_CLIENT_PRODUCT,
        ),
    ));
    let mut line = String::new();
    let mut in_flight = InFlight {
        tasks: JoinSet::new(),
        pending: std::collections::HashMap::new(),
    };

    loop {
        tokio::select! {
            read = reader.read_line(&mut line) => {
                match read {
                    Ok(0) => {
                        in_flight.abort_all().await;
                        return Ok(());
                    }
                    Err(err) => {
                        in_flight.abort_all().await;
                        return Err(err);
                    }
                    Ok(_) => {}
                }
                handle_message(&line, &mut writer, &dispatcher, &mut in_flight).await?;
                line.clear();
            }
            joined = in_flight.tasks.join_next(), if !in_flight.tasks.is_empty() => {
                if let Some(Ok((key, response))) = joined
                    && in_flight.pending.remove(&key).is_some()
                {
                    write_response(&mut writer, &response).await?;
                }
            }
        }
    }
}

/// In-flight tool calls, keyed by the JSON-RPC request id each answers.
struct InFlight {
    tasks: JoinSet<(String, Value)>,
    pending: std::collections::HashMap<String, (Value, AbortHandle)>,
}

impl InFlight {
    /// Abort and drain every tracked call, so none outlives the loop.
    async fn abort_all(&mut self) {
        for (_, task) in self.pending.drain().map(|(_, value)| value) {
            task.abort();
        }
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}

/// Parse one line and route it, writing whatever reply it owes. A blank
/// line is not a message.
async fn handle_message(
    line: &str,
    writer: &mut (impl AsyncWrite + Unpin),
    dispatcher: &Dispatcher,
    in_flight: &mut InFlight,
) -> std::io::Result<()> {
    let message = line.trim();
    if message.is_empty() {
        return Ok(());
    }
    let request = match parse_request(message) {
        Ok(request) => request,
        Err(response) => return write_response(writer, &response).await,
    };

    if request.method == "notifications/cancelled" {
        return cancel_request(&request, writer, in_flight).await;
    }
    if request.method == "tools/call" && !request.is_notification() {
        return spawn_tools_call(request, writer, dispatcher, in_flight).await;
    }
    let Some(response) = handle_request(request).await else {
        return Ok(());
    };
    write_response(writer, &response).await
}

/// Abort the cancelled request's task and answer it with a cancellation
/// error; an untracked id (already finished) is ignored.
async fn cancel_request(
    request: &Request,
    writer: &mut (impl AsyncWrite + Unpin),
    in_flight: &mut InFlight,
) -> std::io::Result<()> {
    let Some(cancelled_id) = request
        .params
        .as_ref()
        .and_then(|params| params.get("requestId"))
    else {
        return Ok(());
    };
    let Some((id, task)) = in_flight.pending.remove(&request_key(cancelled_id)) else {
        return Ok(());
    };
    task.abort();
    let response = crate::jsonrpc::error(id, REQUEST_CANCELLED, "request cancelled");
    write_response(writer, &response).await
}

/// Start a `tools/call` as an abortable task keyed by request id; reusing
/// an in-flight id is an invalid request.
async fn spawn_tools_call(
    request: Request,
    writer: &mut (impl AsyncWrite + Unpin),
    dispatcher: &Dispatcher,
    in_flight: &mut InFlight,
) -> std::io::Result<()> {
    let id = request.id.clone().unwrap_or(Value::Null);
    let key = request_key(&id);
    let std::collections::hash_map::Entry::Vacant(entry) = in_flight.pending.entry(key.clone())
    else {
        let response = crate::jsonrpc::error(id, INVALID_REQUEST, "duplicate in-flight request id");
        return write_response(writer, &response).await;
    };
    let params = request.params;
    let dispatcher = Arc::clone(dispatcher);
    let task_id = id.clone();
    let abort = in_flight.tasks.spawn(async move {
        let response = handle_tools_call_with(task_id, params.as_ref(), dispatcher.as_ref()).await;
        (key, response)
    });
    entry.insert((id, abort));
    Ok(())
}

fn request_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "null".to_owned())
}

async fn write_response(
    writer: &mut (impl AsyncWrite + Unpin),
    response: &Value,
) -> std::io::Result<()> {
    let line = response_line(response);
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

fn response_line(response: &Value) -> String {
    serde_json::to_string(response).unwrap_or_else(|err| {
        serde_json::to_string(&crate::jsonrpc::error(
            Value::Null,
            INTERNAL_ERROR,
            format!("failed to serialize response: {err}"),
        ))
        .unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialization failed"}}"#
                .to_owned()
        })
    })
}

/// Parse one message; a malformed one is a parse error with a null id (the
/// request id is unrecoverable).
pub(super) fn parse_request(message: &str) -> Result<Request, Value> {
    serde_json::from_str(message).map_err(|err| {
        crate::jsonrpc::error(Value::Null, PARSE_ERROR, format!("parse error: {err}"))
    })
}

/// Dispatch a parsed request; `None` for a notification (no reply).
pub(super) async fn handle_request(request: Request) -> Option<Value> {
    let is_notification = request.is_notification();
    let id = request.id.clone().unwrap_or(Value::Null);

    match request.method.as_str() {
        "initialize" => Some(crate::jsonrpc::success(
            id,
            initialize_result(request.params.as_ref()),
        )),
        "notifications/initialized" | "notifications/cancelled" => None,
        "tools/list" => Some(crate::jsonrpc::success(
            id,
            json!({ "tools": tools::catalog() }),
        )),
        "tools/call" if is_notification => None,
        "tools/call" => Some(handle_tools_call(id, request.params.as_ref()).await),
        "ping" => Some(crate::jsonrpc::success(id, json!({}))),
        _ if is_notification => None,
        other => Some(crate::jsonrpc::error(
            id,
            METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        )),
    }
}

/// The `initialize` result: protocol version, advertised capabilities, and
/// server identity. Preserve older hosts' supported revision when requested.
pub(super) fn initialize_result(params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = requested
        .filter(|version| ["2024-11-05", "2025-03-26", MCP_PROTOCOL_VERSION].contains(version))
        .unwrap_or(MCP_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "phux",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// Handle `tools/call`: a tool failure is a successful JSON-RPC response
/// carrying `isError: true`, never a JSON-RPC error or a crash.
async fn handle_tools_call(id: Value, params: Option<&Value>) -> Value {
    let dispatcher = default_dispatcher();
    handle_tools_call_with(id, params, dispatcher.as_ref()).await
}

async fn handle_tools_call_with(
    id: Value,
    params: Option<&Value>,
    dispatcher: &(dyn Fn(String, Value) -> super::DispatchFuture + Send + Sync),
) -> Value {
    let Some(params) = params else {
        return crate::jsonrpc::error(id, INVALID_REQUEST, "tools/call requires params");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return crate::jsonrpc::error(id, INVALID_REQUEST, "tools/call requires a string `name`");
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match dispatcher(name.to_owned(), args).await {
        Ok(value) => crate::jsonrpc::success(id, tool_content(&value, false)),
        Err(error) => crate::jsonrpc::success(id, tool_content(&error.document(), true)),
    }
}
