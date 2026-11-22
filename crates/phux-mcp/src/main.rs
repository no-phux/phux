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
#[cfg(test)]
mod contract_tests;
mod contracts;
mod coord_tools;
mod diagnostic_tools;
mod document_tools;
mod git_tool;
#[cfg(test)]
mod goldens;
mod jsonrpc;
#[cfg(test)]
mod operation_tests;
mod operations;
mod org_tools;
mod pane_tools;
mod perf_tools;
mod plugin_tools;
mod resource_tools;
mod stdio;
mod terminal_tools;
mod tool_table;
mod tools;

use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{BufReader, BufWriter};

#[cfg(test)]
use contracts::tool_content;
use stdio::serve_io;
#[cfg(test)]
use stdio::{handle_request, initialize_result, parse_request};

type DispatchFuture =
    Pin<Box<dyn Future<Output = Result<Value, tools::ToolError>> + Send + 'static>>;
type Dispatcher = Arc<dyn Fn(String, Value) -> DispatchFuture + Send + Sync>;

/// The MCP protocol revision this adapter implements.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

const SKILL: &str = include_str!("../../../.agents/skills/using-phux-mcp/SKILL.md");
const HELP: &str = "phux-mcp - MCP stdio adapter for phux\n\n\
Usage: phux-mcp [OPTION]\n\n\
Options:\n\
\x20     --skill    Print the agent operating guide compiled into this binary\n\
\x20     --schema   Print the same MCP tool catalog returned by tools/list\n\
\x20 -h, --help     Print help\n\
\x20 -V, --version  Print version\n\n\
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

// MCP stderr is an out-of-band channel, not a human terminal renderer. Keep
// these diagnostics plain even under forced color; stdout stays JSON-RPC.
fn diagnostic(level: &str, message: std::fmt::Arguments<'_>) {
    eprintln!("phux: {level}: mcp: {message}");
}

fn write_stdout(bytes: &[u8]) -> std::process::ExitCode {
    match std::io::stdout().lock().write_all(bytes) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => std::process::ExitCode::SUCCESS,
        Err(err) => {
            diagnostic("error", format_args!("stdout write failed: {err}"));
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
            diagnostic(
                "error",
                format_args!("could not render the tool catalog: {err}"),
            );
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
            diagnostic("error", format_args!("{message}"));
            eprintln!("  hint: Try `phux-mcp --help` for usage.");
            return std::process::ExitCode::from(2);
        }
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            diagnostic("error", format_args!("failed to build runtime: {err}"));
            return std::process::ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(serve());
    finish_serving(rt, result)
}

/// Cancelled blocking local readers may outlive serving; never wait for them at shutdown.
fn finish_serving(
    rt: tokio::runtime::Runtime,
    result: std::io::Result<()>,
) -> std::process::ExitCode {
    rt.shutdown_background();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => std::process::ExitCode::SUCCESS,
        Err(err) => {
            diagnostic("error", format_args!("fatal: {err}"));
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
    operations::dispatcher()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn finish_serving_preserves_exit_status_mapping() {
        for (result, expected) in [
            (Ok(()), std::process::ExitCode::SUCCESS),
            (
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
                std::process::ExitCode::SUCCESS,
            ),
            (
                Err(std::io::Error::from(std::io::ErrorKind::Other)),
                std::process::ExitCode::FAILURE,
            ),
        ] {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            assert_eq!(super::finish_serving(rt, result), expected);
        }
    }

    #[test]
    fn finish_serving_does_not_wait_for_a_running_blocking_reader() {
        use std::sync::mpsc::sync_channel;

        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let (finished_tx, finished_rx) = sync_channel(1);
        let (status_tx, status_rx) = sync_channel(1);
        let worker = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.spawn_blocking(move || {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
                let _ = finished_tx.send(());
            });
            // Ensure the blocking reader is running before the real main shutdown path.
            let entered = entered_rx.recv_timeout(Duration::from_secs(5));
            let status = super::finish_serving(rt, Ok(()));
            let _ = status_tx.send((entered, status));
        });
        let status = status_rx.recv_timeout(Duration::from_secs(5));
        // Always release and join before asserting, even if shutdown regresses to waiting.
        let released = release_tx.send(());
        let finished = finished_rx.recv_timeout(Duration::from_secs(5));
        let joined = worker.join();
        assert!(released.is_ok(), "blocking reader release failed");
        assert!(
            finished.is_ok(),
            "blocking reader did not finish after release"
        );
        assert!(joined.is_ok(), "shutdown worker panicked");
        let (entered, status) = status.expect("shutdown waited for the held blocking reader");
        assert!(entered.is_ok(), "blocking reader never entered");
        assert_eq!(status, std::process::ExitCode::SUCCESS);
    }

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::jsonrpc::{
        INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, REQUEST_CANCELLED, Request,
    };
    use super::*;

    /// Only turns a wedged dispatcher into a bounded failure; no assertion is
    /// on latency.
    const NO_HANG_DEADLINE: Duration = Duration::from_secs(30);

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
