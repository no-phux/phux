//! `phux-mcp` — a minimal Model Context Protocol stdio adapter over the
//! phux agent surface (ADR-0022 §5).
//!
//! Newline-delimited JSON-RPC 2.0 on stdin/stdout, hand-rolled over
//! `serde_json`. Methods: `initialize`, `tools/list`, `tools/call` (tool
//! failures are `isError: true` results), `ping`, and the `initialized` /
//! `cancelled` notifications. Malformed input gets a JSON-RPC error and the
//! loop continues until stdin EOF.

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stdout,
    reason = "stdout is the MCP transport for JSON-RPC responses"
)]
#![allow(
    clippy::print_stderr,
    reason = "stderr is the adapter's out-of-band diagnostic channel"
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "bin-internal modules expose items via `pub`; `pub(crate)` would trip unreachable_pub in a binary with no external API (same pattern crates/phux/src/lib.rs uses for its own internal modules)"
)]

mod agent_tools;
mod annotations;
mod approval_tools;
mod ask_tool;
mod cli_adapter;
mod cli_tools;
mod diagnostic_tools;
#[cfg(test)]
mod goldens;
mod jsonrpc;
mod pane_tools;
mod plugin_tools;
mod resource_tools;
mod tool_table;
mod tools;

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::task::{AbortHandle, JoinSet};

use jsonrpc::{
    INTERNAL_ERROR, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, REQUEST_CANCELLED, Request,
};

type DispatchFuture =
    Pin<Box<dyn Future<Output = Result<Value, tools::ToolError>> + Send + 'static>>;
type Dispatcher = Arc<dyn Fn(String, Value) -> DispatchFuture + Send + Sync>;

/// The MCP protocol revision this adapter implements.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

const SKILL: &str = include_str!("../../../.agents/skills/using-phux-mcp/SKILL.md");
const HELP: &str = "phux-mcp - MCP stdio adapter for phux\n\n\
Usage: phux-mcp [OPTION]\n\n\
Options:\n  \
  --skill   Print the agent operating guide compiled into this binary\n  \
  --schema  Print the same MCP tool catalog returned by tools/list\n  \
  -h, --help     Print help\n  \
  -V, --version  Print version\n\n\
With no arguments, serve MCP over newline-delimited JSON-RPC on stdin/stdout.\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Serve,
    Skill,
    Schema,
    Help,
    Version,
}

fn parse_mode<I, S>(args: I) -> Result<Mode, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(Mode::Serve),
        [arg] if arg.as_ref() == "--skill" => Ok(Mode::Skill),
        [arg] if arg.as_ref() == "--schema" => Ok(Mode::Schema),
        [arg] if arg.as_ref() == "-h" || arg.as_ref() == "--help" => Ok(Mode::Help),
        [arg] if arg.as_ref() == "-V" || arg.as_ref() == "--version" => Ok(Mode::Version),
        _ => Err(
            "expected no arguments or exactly one of --skill, --schema, --help, --version"
                .to_owned(),
        ),
    }
}

fn write_stdout(bytes: &[u8]) -> std::process::ExitCode {
    match std::io::stdout().lock().write_all(bytes) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux-mcp: stdout write failed: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn write_schema() -> std::process::ExitCode {
    match serde_json::to_vec_pretty(&tools::catalog()) {
        Ok(mut rendered) => {
            rendered.push(b'\n');
            write_stdout(&rendered)
        }
        Err(err) => {
            eprintln!("phux-mcp: could not render the tool catalog: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn main() -> std::process::ExitCode {
    match parse_mode(std::env::args_os().skip(1)) {
        Ok(Mode::Skill) => return write_stdout(SKILL.as_bytes()),
        Ok(Mode::Schema) => return write_schema(),
        Ok(Mode::Help) => return write_stdout(HELP.as_bytes()),
        Ok(Mode::Version) => {
            return write_stdout(concat!("phux-mcp ", env!("CARGO_PKG_VERSION"), "\n").as_bytes());
        }
        Ok(Mode::Serve) => {}
        Err(message) => {
            eprintln!("phux-mcp: {message}\nTry `phux-mcp --help` for usage.");
            return std::process::ExitCode::from(2);
        }
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("phux-mcp: failed to build runtime: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match rt.block_on(serve()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux-mcp: fatal: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Serve stdin/stdout until EOF.
///
/// # Errors
///
/// Only stdin/stdout I/O failures; per-message failures become JSON-RPC
/// error responses.
async fn serve() -> std::io::Result<()> {
    serve_io(
        BufReader::new(tokio::io::stdin()),
        BufWriter::new(tokio::io::stdout()),
        default_dispatcher(),
    )
    .await
}

fn default_dispatcher() -> Dispatcher {
    Arc::new(|name, args| Box::pin(async move { tools::dispatch(&name, &args).await }))
}

/// Drive the protocol with each tool call an abortable task. Only this loop
/// writes, so output stays framed while replies complete out of order.
async fn serve_io<R, W>(mut reader: R, mut writer: W, dispatcher: Dispatcher) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut line = String::new();
    let mut in_flight = InFlight {
        tasks: JoinSet::new(),
        pending: HashMap::new(),
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
    pending: HashMap<String, (Value, AbortHandle)>,
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
    let response = jsonrpc::error(id, REQUEST_CANCELLED, "request cancelled");
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
        let response = jsonrpc::error(id, INVALID_REQUEST, "duplicate in-flight request id");
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
        serde_json::to_string(&jsonrpc::error(
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
fn parse_request(message: &str) -> Result<Request, Value> {
    serde_json::from_str(message)
        .map_err(|err| jsonrpc::error(Value::Null, PARSE_ERROR, format!("parse error: {err}")))
}

/// Dispatch a parsed request; `None` for a notification (no reply).
async fn handle_request(request: Request) -> Option<Value> {
    let is_notification = request.is_notification();
    let id = request.id.clone().unwrap_or(Value::Null);

    match request.method.as_str() {
        "initialize" => Some(jsonrpc::success(id, initialize_result())),
        "notifications/initialized" | "notifications/cancelled" => None,
        "tools/list" => Some(jsonrpc::success(id, json!({ "tools": tools::catalog() }))),
        "tools/call" if is_notification => None,
        "tools/call" => Some(handle_tools_call(id, request.params.as_ref()).await),
        "ping" => Some(jsonrpc::success(id, json!({}))),
        _ if is_notification => None,
        other => Some(jsonrpc::error(
            id,
            METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        )),
    }
}

/// The `initialize` result: protocol version, advertised capabilities, and
/// server identity. The client's params are tolerated and ignored.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
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
    dispatcher: &(dyn Fn(String, Value) -> DispatchFuture + Send + Sync),
) -> Value {
    let Some(params) = params else {
        return jsonrpc::error(id, INVALID_REQUEST, "tools/call requires params");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return jsonrpc::error(id, INVALID_REQUEST, "tools/call requires a string `name`");
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match dispatcher(name.to_owned(), args).await {
        Ok(value) => jsonrpc::success(id, tool_content(&value, false)),
        Err(tools::ToolError(message)) => {
            jsonrpc::success(id, tool_content(&Value::String(message), true))
        }
    }
}

/// The `tools/call` result envelope: one text block carrying a string
/// verbatim or other values as pretty JSON, plus `isError`.
fn tool_content(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other)
            .unwrap_or_else(|err| format!("<failed to serialize result: {err}>")),
    };
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::io::{AsyncWriteExt, BufReader};

    use super::*;

    /// Only turns a wedged dispatcher into a bounded failure; no assertion is
    /// on latency.
    const NO_HANG_DEADLINE: Duration = Duration::from_secs(30);

    #[test]
    fn standalone_mode_parser_is_exact() {
        assert_eq!(parse_mode([] as [&str; 0]).unwrap(), Mode::Serve);
        assert_eq!(parse_mode(["--skill"]).unwrap(), Mode::Skill);
        assert_eq!(parse_mode(["--schema"]).unwrap(), Mode::Schema);
        assert_eq!(parse_mode(["--help"]).unwrap(), Mode::Help);
        assert_eq!(parse_mode(["--version"]).unwrap(), Mode::Version);
        assert!(parse_mode(["--skill", "--schema"]).is_err());
        assert!(parse_mode(["junk"]).is_err());
    }

    /// A fake CLI that writes its pid to `{dir}/pid` and sleeps.
    fn sleeping_cli() -> (TempDir, Arc<cli_adapter::CliAdapter>, PathBuf) {
        let (temp, adapter, _log) =
            cli_adapter::fake::cli("printf '%s\\n' \"$$\" > '{dir}/pid'\nexec sleep 60\n");
        let pid_file = temp.path().join("pid");
        (temp, Arc::new(adapter), pid_file)
    }

    fn sleeping_dispatcher(adapter: Arc<cli_adapter::CliAdapter>) -> Dispatcher {
        Arc::new(move |name, _args| {
            if name == "fast" {
                return Box::pin(async { Ok(json!({ "completed": true })) });
            }
            let adapter = Arc::clone(&adapter);
            Box::pin(async move {
                adapter
                    .run(std::iter::empty::<&str>(), Duration::from_secs(60))
                    .await
                    .map(|_| json!({ "completed": true }))
            })
        })
    }

    async fn wait_for_pid(path: &Path) -> u32 {
        tokio::time::timeout(NO_HANG_DEADLINE, async {
            loop {
                if let Ok(contents) = fs::read_to_string(path)
                    && let Ok(pid) = contents.trim().parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake CLI wrote its pid")
    }

    fn process_exists(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    async fn wait_for_exit(pid: u32) {
        tokio::time::timeout(NO_HANG_DEADLINE, async {
            while process_exists(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled fake CLI exited");
    }

    #[tokio::test]
    async fn initialize_returns_protocol_and_server_info() {
        let req: Request = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
        )
        .unwrap();
        let resp = handle_request(req).await.expect("initialize replies");
        assert_eq!(resp["jsonrpc"], json!("2.0"));
        assert_eq!(resp["id"], json!(1));
        assert_eq!(
            resp["result"]["protocolVersion"],
            json!(MCP_PROTOCOL_VERSION)
        );
        assert_eq!(resp["result"]["serverInfo"]["name"], json!("phux"));
        assert_eq!(
            resp["result"]["serverInfo"]["version"],
            json!(env!("CARGO_PKG_VERSION"))
        );
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn initialized_notification_gets_no_reply() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(handle_request(req).await.is_none());
    }

    /// Tool names are gated against the table by `tests/parity.rs`; here,
    /// the served shape: object schemas, descriptions, derived hints.
    #[tokio::test]
    async fn tools_list_is_well_formed() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#).unwrap();
        let resp = handle_request(req).await.expect("tools/list replies");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), tool_table::TOOLS.len());
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], json!("object"), "{tool}");
            assert!(tool["description"].is_string(), "{tool}");
            assert!(tool["annotations"]["readOnlyHint"].is_boolean(), "{tool}");
            assert!(
                tool["annotations"]["destructiveHint"].is_boolean(),
                "{tool}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_method_is_a_jsonrpc_error() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":3,"method":"frobnicate"}"#).unwrap();
        let resp = handle_request(req).await.expect("error reply");
        assert_eq!(resp["error"]["code"], json!(METHOD_NOT_FOUND));
        assert_eq!(resp["id"], json!(3));
    }

    #[tokio::test]
    async fn malformed_json_yields_parse_error_with_null_id() {
        let resp = parse_request("{ this is not json").expect_err("parse error reply");
        assert_eq!(resp["error"]["code"], json!(PARSE_ERROR));
        assert_eq!(resp["id"], Value::Null);
    }

    #[tokio::test]
    async fn tools_call_without_params_is_invalid_request() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call"}"#).unwrap();
        let resp = handle_request(req).await.expect("error reply");
        assert_eq!(resp["error"]["code"], json!(INVALID_REQUEST));
    }

    #[tokio::test]
    async fn cancellation_keeps_reading_and_terminates_the_cli_child() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (_temp, adapter, pid_file) = sleeping_cli();
                let (mut input, server_input) = tokio::io::duplex(4096);
                let (server_output, output) = tokio::io::duplex(4096);
                let server = tokio::task::spawn_local(serve_io(
                    BufReader::new(server_input),
                    server_output,
                    sleeping_dispatcher(Arc::clone(&adapter)),
                ));
                input
                    .write_all(
                        br#"{"jsonrpc":"2.0","id":"slow","method":"tools/call","params":{"name":"fake","arguments":{}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fast","arguments":{}}}
"#,
                    )
                    .await
                    .unwrap();

                let mut replies = BufReader::new(output).lines();
                let fast = tokio::time::timeout(NO_HANG_DEADLINE, replies.next_line())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let fast: Value = serde_json::from_str(&fast).unwrap();
                assert_eq!(fast["id"], 2);
                assert_eq!(fast["result"]["isError"], false);

                let pid = wait_for_pid(&pid_file).await;
                assert!(process_exists(pid));
                input
                    .write_all(
                        br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"slow","reason":"test"}}
"#,
                    )
                    .await
                    .unwrap();
                let cancelled = tokio::time::timeout(NO_HANG_DEADLINE, replies.next_line())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let cancelled: Value = serde_json::from_str(&cancelled).unwrap();
                assert_eq!(cancelled["id"], "slow");
                assert_eq!(cancelled["error"]["code"], REQUEST_CANCELLED);
                wait_for_exit(pid).await;

                drop(input);
                server.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test]
    async fn stdin_eof_aborts_pending_tools_and_terminates_the_cli_child() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (_temp, adapter, pid_file) = sleeping_cli();
                let (mut input, server_input) = tokio::io::duplex(4096);
                let (server_output, _output) = tokio::io::duplex(4096);
                let server = tokio::task::spawn_local(serve_io(
                    BufReader::new(server_input),
                    server_output,
                    sleeping_dispatcher(Arc::clone(&adapter)),
                ));
                input
                    .write_all(
                        br#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"fake","arguments":{}}}
"#,
                    )
                    .await
                    .unwrap();
                let pid = wait_for_pid(&pid_file).await;
                assert!(process_exists(pid));

                drop(input);
                tokio::time::timeout(NO_HANG_DEADLINE, server)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                wait_for_exit(pid).await;
            })
            .await;
    }
}
