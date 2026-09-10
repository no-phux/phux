//! `phux-mcp` — a minimal Model Context Protocol stdio adapter over the
//! phux agent surface (phux-93b, ADR-0022 §5 "MCP as a thin adapter").
//!
//! Speaks JSON-RPC 2.0 over the MCP stdio transport: newline-delimited
//! JSON, one message per line on stdin/stdout. The JSON-RPC is hand-rolled
//! over `serde_json` (no framework dep); every tool is a thin wrapper over
//! `phux-client`'s agent surface (`snapshot`, `send_keys`, `run`, `wait`)
//! or a direct `GET_STATE` control command — the same structured surface
//! the CLI uses, never a separate core.
//!
//! Methods:
//! - `initialize` → capabilities + serverInfo.
//! - `notifications/initialized` → no reply (notification).
//! - `tools/list` → the tool catalog.
//! - `tools/call` → dispatch by tool name; tool failures become a result
//!   with `isError: true`, never a process crash.
//!
//! Robustness: malformed JSON or an unknown method yields a JSON-RPC error
//! response; the loop continues until stdin EOF.

#![forbid(unsafe_code)]
// The MCP transport speaks on stdout; writing responses there is the whole
// point of this binary (the workspace lints deny stdout/stderr by default).
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
mod ask_tool;
mod cli_adapter;
mod cli_tools;
mod diagnostic_tools;
mod jsonrpc;
mod plugin_action;
mod plugin_workspace;
mod socket;
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

/// The MCP protocol version this adapter implements.
///
/// TODO(phux-93b): pinned to the 2024-11-05 revision the task specifies.
/// Newer MCP revisions are additive; bump when we adopt one.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

const SKILL: &str = include_str!("../../../skills/phux-mcp/SKILL.md");
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

    // Current-thread runtime: the phux client surface is async and its
    // client-side libghostty Terminal is !Send (ADR-0003).
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

/// Read newline-delimited JSON-RPC messages from stdin until EOF, handling
/// each and writing any response to stdout.
///
/// # Errors
///
/// Returns an [`std::io::Error`] only on an stdin read failure (not EOF) —
/// per-message parse/dispatch failures are turned into JSON-RPC error
/// responses, not propagated.
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

/// Drive the stdio protocol while tool calls remain cancellable.
///
/// Tool futures run as independently abortable runtime tasks. Only this loop
/// writes replies, keeping output framing serialized while allowing replies to
/// complete out of request order with the original JSON-RPC id intact.
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

/// The tool calls running as independently abortable tasks, indexed by the
/// JSON-RPC request id each one will answer.
struct InFlight {
    tasks: JoinSet<(String, Value)>,
    pending: HashMap<String, (Value, AbortHandle)>,
}

impl InFlight {
    /// Drop every tracked call: abort the handles, abort the set, and drain
    /// it so no task outlives the loop that was going to answer for it.
    async fn abort_all(&mut self) {
        for (_, task) in self.pending.drain().map(|(_, value)| value) {
            task.abort();
        }
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}

/// Parse one newline-delimited message and route it, writing whatever reply
/// it owes. A blank line is not a message; a malformed one is a JSON-RPC
/// parse error with a null id (the request id is unrecoverable from
/// unparseable JSON) and the loop carries on.
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
    let request: Request = match serde_json::from_str(message) {
        Ok(request) => request,
        Err(err) => {
            let response = jsonrpc::error(Value::Null, PARSE_ERROR, format!("parse error: {err}"));
            return write_response(writer, &response).await;
        }
    };

    if request.method == "notifications/cancelled" {
        return cancel_request(&request, writer, in_flight).await;
    }
    if request.method == "tools/call" && !request.is_notification() {
        return spawn_tools_call(request, writer, dispatcher, in_flight).await;
    }
    // Everything else is a plain request/response method; a notification
    // yields no reply.
    let Some(response) = handle_request(request).await else {
        return Ok(());
    };
    write_response(writer, &response).await
}

/// Abort the task tracked for the cancelled request id and close that request
/// out with a cancellation error carrying its original id. An id we are not
/// tracking is silently ignored: the call already finished or never existed.
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

/// Start a `tools/call` as its own abortable task, keyed by request id so a
/// later `notifications/cancelled` can find it. Reusing an id that is still
/// in flight is an invalid request, not a second task.
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

/// Handle one line of input, returning the JSON-RPC response to emit, or
/// `None` for a notification (which gets no reply).
#[cfg(test)]
async fn handle_line(line: &str) -> Option<Value> {
    // Parse the envelope. A malformed line is a JSON-RPC parse error with a
    // null id (we cannot recover the request id from unparseable JSON).
    let request: Request = match serde_json::from_str(line) {
        Ok(req) => req,
        Err(err) => {
            return Some(jsonrpc::error(
                Value::Null,
                PARSE_ERROR,
                format!("parse error: {err}"),
            ));
        }
    };
    handle_request(request).await
}

/// Dispatch a parsed [`Request`] to its method handler.
async fn handle_request(request: Request) -> Option<Value> {
    let is_notification = request.is_notification();
    // For a request, echo the id; for a notification there is no reply, but
    // we still carry a placeholder so the error path is uniform.
    let id = request.id.clone().unwrap_or(Value::Null);

    match request.method.as_str() {
        "initialize" => Some(jsonrpc::success(id, initialize_result())),
        "notifications/initialized" | "notifications/cancelled" => None,
        "tools/list" => Some(jsonrpc::success(id, json!({ "tools": tools::catalog() }))),
        "tools/call" if is_notification => None,
        "tools/call" => Some(handle_tools_call(id, request.params.as_ref()).await),
        // `ping` is a common MCP keepalive; reply with an empty result.
        "ping" => Some(jsonrpc::success(id, json!({}))),
        other => {
            if is_notification {
                // Unknown notification: ignore silently (no reply for
                // notifications, per JSON-RPC).
                None
            } else {
                Some(jsonrpc::error(
                    id,
                    METHOD_NOT_FOUND,
                    format!("method not found: {other}"),
                ))
            }
        }
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

/// Handle `tools/call`: extract `name`/`arguments`, dispatch, and wrap the
/// outcome in the MCP `content`/`isError` envelope. A tool failure is a
/// *successful* JSON-RPC response carrying `isError: true` — not a
/// JSON-RPC error and never a crash.
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
    // `arguments` is optional; default to an empty object so tools that take
    // no required args (e.g. phux_ls) work without it.
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

/// Build the MCP `tools/call` result envelope: a single text content block
/// carrying the value as pretty JSON (or the raw message), plus `isError`.
fn tool_content(value: &Value, is_error: bool) -> Value {
    // A bare error string is shown verbatim; structured results are
    // pretty-printed JSON so a model reads them cleanly.
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
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::io::{AsyncWriteExt, BufReader};

    use super::*;

    /// Ceiling for "the child/server should already have done this" waits.
    ///
    /// Not load-bearing. Every assertion below is on the JSON-RPC reply or on
    /// whether a pid is still alive — never on latency — so the timeout only
    /// turns a wedged dispatcher into a bounded failure. The 1-2s bounds it
    /// replaces were generous on an idle laptop and a measurement of the
    /// scheduler on a saturated one (phux-br1f); a real hang still fails,
    /// just later, with the same message.
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

    fn sleeping_cli() -> (TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("pid");
        let executable = temp.path().join("phux");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\nexec sleep 60\n",
                pid_file.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        (temp, executable, pid_file)
    }

    fn sleeping_dispatcher(executable: PathBuf) -> Dispatcher {
        Arc::new(move |name, _args| {
            if name == "fast" {
                return Box::pin(async { Ok(json!({ "completed": true })) });
            }
            let adapter = cli_adapter::CliAdapter::new(executable.clone());
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

    #[tokio::test]
    async fn tools_list_is_well_formed() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#).unwrap();
        let resp = handle_request(req).await.expect("tools/list replies");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 39);
        assert!(tools.iter().any(|t| t["name"] == json!("phux_ls")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_paste")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_new")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_kill")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_detach")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_watch")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_ask")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_launch")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_spawn")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_agent_list")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_agent_wait")));
        assert!(
            tools
                .iter()
                .any(|t| t["name"] == json!("phux_agent_prompt"))
        );
        assert!(
            tools
                .iter()
                .any(|t| t["name"] == json!("phux_agent_answer"))
        );
        assert!(tools.iter().any(|t| t["name"] == json!("phux_agent_start")));
        // The AgentSession resource verbs, one strict tool each.
        for name in [
            "phux_agent_session_open",
            "phux_agent_session_close",
            "phux_agent_emit",
            "phux_agent_log",
        ] {
            assert!(
                tools.iter().any(|t| t["name"] == json!(name)),
                "tools/list lost {name}"
            );
        }
        assert!(
            tools
                .iter()
                .any(|t| t["name"] == json!("phux_agent_send_keys"))
        );
        // The multiplexer is gone, not aliased: keeping it alive would
        // freeze the argument union ADR-0071 point 7(b) exists to prevent.
        assert!(
            !tools.iter().any(|t| t["name"] == json!("phux_agent")),
            "the phux_agent action multiplexer must not survive the split",
        );
        assert!(tools.iter().any(|t| t["name"] == json!("phux_insert_pane")));
        assert!(
            tools
                .iter()
                .any(|t| t["name"] == json!("phux_plugin_workspace"))
        );
        // The diagnostics: an agent that can act on the server can now also
        // ask whether it is healthy, without shelling out past MCP.
        assert!(tools.iter().any(|t| t["name"] == json!("phux_status")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_doctor")));
        assert!(tools.iter().any(|t| t["name"] == json!("phux_whoami")));
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
        let resp = handle_line("{ this is not json")
            .await
            .expect("parse error reply");
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
                let (_temp, executable, pid_file) = sleeping_cli();
                let (mut input, server_input) = tokio::io::duplex(4096);
                let (server_output, output) = tokio::io::duplex(4096);
                let server = tokio::task::spawn_local(serve_io(
                    BufReader::new(server_input),
                    server_output,
                    sleeping_dispatcher(executable),
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
                let (_temp, executable, pid_file) = sleeping_cli();
                let (mut input, server_input) = tokio::io::duplex(4096);
                let (server_output, _output) = tokio::io::duplex(4096);
                let server = tokio::task::spawn_local(serve_io(
                    BufReader::new(server_input),
                    server_output,
                    sleeping_dispatcher(executable),
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
