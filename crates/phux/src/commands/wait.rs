use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, parse_selector, report_no_server, resolve_target};

/// `phux wait [TARGET]` — poll until a pane meets a condition (ADR-0022 §4).
///
/// `--until TEXT` waits for a visible line to contain `TEXT`; `--idle MS`
/// waits for the screen to settle; with neither, defaults to idle. Exits 0
/// when met, 124 on `--timeout`. The poll floor of the event surface: it
/// reads via the side-effect-free `GET_SCREEN`, so it never disturbs the
/// pane.
pub(crate) fn run_wait(
    session: Option<&str>,
    until: Option<String>,
    idle: Option<u64>,
    timeout: Option<u64>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    use phux_client::wait::{Condition, DEFAULT_IDLE_DWELL, DEFAULT_POLL_INTERVAL, WaitOutcome};

    let selector = match parse_selector(session) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    // `--until` takes precedence; otherwise settle on idle (explicit ms or
    // the default dwell).
    let condition = until.map_or_else(
        || Condition::Idle(idle.map_or(DEFAULT_IDLE_DWELL, Duration::from_millis)),
        Condition::Contains,
    );
    let timeout = timeout.map(Duration::from_secs);
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, "wait").await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let result = match phux_client::wait::poll_until(
            &socket_path,
            terminal_id,
            &condition,
            timeout,
            DEFAULT_POLL_INTERVAL,
        )
        .await
        {
            Ok(result) => result,
            Err(err @ AttachError::Io(_)) => return report_no_server(&err, &socket_path, "wait"),
            Err(err) => {
                eprintln!("phux: wait failed: {err}");
                return ExitCode::FAILURE;
            }
        };
        if json && let Ok(s) = serde_json::to_string_pretty(&result.screen) {
            outln!("{s}");
        }
        match result.outcome {
            WaitOutcome::Met => ExitCode::SUCCESS,
            WaitOutcome::TimedOut => {
                eprintln!("phux: wait timed out after {} polls", result.polls);
                ExitCode::from(124)
            }
        }
    })
}
