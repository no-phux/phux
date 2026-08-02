use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, parse_selector, report_no_server, resolve_target};

/// `phux send-keys TARGET KEYS...` — send input to a pane via the
/// side-effect-free `ROUTE_INPUT` route.
///
/// `TARGET` is the full selector grammar (`docs/consumers/tui.md` §3):
/// `session`, `session:window`, `session:window.pane`, `@id`, `.`. It
/// is resolved client-side to a single pane (the selected one — the focused
/// pane when the selector spans several), then the events route to exactly
/// that pane by id, so this neither attaches nor resizes the live pane
/// (phux-n95; see [`phux_client::send_keys::send_to`]).
pub(crate) fn run_send_keys(target: &str, keys: &[String], socket: Option<PathBuf>) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "send-keys", false).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match phux_client::send_keys::send_to(&socket_path, pane, keys).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err @ AttachError::Io(_)) => report_no_server(&err, &socket_path, "send-keys"),
            Err(AttachError::Refused(msg)) => {
                eprintln!("phux: cannot send to '{target}': {msg} (try `phux ls`)");
                ExitCode::FAILURE
            }
            Err(err) => {
                eprintln!("phux: send-keys failed: {err}");
                ExitCode::FAILURE
            }
        }
    })
}
