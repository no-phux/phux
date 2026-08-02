//! `phux status` — one glance at the server behind the socket.
//!
//! Collect-then-render: [`collect`] gathers every fact into a
//! [`StatusReport`], and the two pure renderers ([`render_human`],
//! [`status_document`]) turn it into the human report or the stable JSON
//! shape — so both formats are pinned by unit tests on a fabricated report,
//! without a server.
//!
//! Every fact is sourced without a wire change:
//!
//! - **pid** — the UDS peer credentials, read at connect time
//!   ([`Connection::peer_pid`]). An OS fact about the socket; the server
//!   does not participate.
//! - **since** — the socket file's mtime, i.e. the moment the listener
//!   bound it. Honest across a graceful upgrade, where the listener (and
//!   the socket inode) is inherited rather than re-bound.
//! - **protocol** — a real `HELLO`/`HELLO_OK` exchange
//!   ([`phux_client::state::probe_hello`]); the one-shot verbs otherwise
//!   skip the handshake, so the negotiated version is invisible to them.
//! - **clients / sessions / satellite split / degradation** — `GET_STATE`,
//!   the same snapshot `phux ls` renders.
//! - **log paths** — the canonical `phux_server::telemetry` helpers, so
//!   status and `phux logs` can never disagree about where the logs live.
//!
//! With no server running: the human path prints the same multi-line
//! no-server diagnostic every other verb prints (exit 1); `--json` answers
//! with `{"running": false, ...}` **on stdout** (exit 1) — a status question
//! about a stopped server has an answer, not an error — embedding the same
//! `code` / `message` / `remedy` vocabulary as the shared JSON error
//! contract. Failures after the connect (the server hangs up mid-probe) are
//! errors and go through the shared contract emitter.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_client::state::Degradation;
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, ls};

/// Version of the `phux status --json` document. Additive fields do not
/// bump it.
const STATUS_SCHEMA_VERSION: u8 = 1;

/// Everything one `phux status` reports, collected before any rendering.
#[derive(Debug)]
pub(crate) struct StatusReport {
    /// The socket the report describes.
    socket: PathBuf,
    /// Server pid from the UDS peer credentials; `None` when the platform
    /// exposes no pid on the socket.
    pid: Option<i32>,
    /// Socket bind time (the socket file's mtime), seconds since the epoch;
    /// `None` when the file cannot be statted.
    since_unix_secs: Option<i64>,
    /// The protocol version triple the server selected in `HELLO_OK`.
    protocol: (u16, u16, u16),
    /// Hub-local sessions, name-sorted (the `phux ls` order).
    sessions: Vec<SessionInfo>,
    /// Formatted ids of satellite Terminals — panes that exist on federated
    /// satellites and cannot be joined to hub-local sessions.
    satellite_terminals: Vec<String>,
    /// Per-satellite degradation notices; empty means the view is complete.
    unreachable: Vec<String>,
    /// The canonical server log path.
    server_log: PathBuf,
    /// The directory holding the per-pid `client-<pid>.log` files.
    client_log_dir: PathBuf,
}

/// `phux status` — report the running server: pid, up-since, protocol,
/// clients, sessions, logs. Does not auto-start a server.
pub(crate) fn run_status(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(collect(&socket_path)) {
        Ok(report) => {
            if json {
                print_json(&report)
            } else {
                for line in render_human(&report, now_unix_secs()) {
                    outln!("{line}");
                }
                ExitCode::SUCCESS
            }
        }
        Err(err) => report_failure(json, &err, &socket_path),
    }
}

/// Gather every fact of the report over one connection: peer pid at connect,
/// then the `HELLO` probe, then `GET_STATE` — the attach path's own frame
/// order, so any server that can attach a client can answer this.
async fn collect(socket_path: &Path) -> Result<StatusReport, AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    let pid = conn.peer_pid();
    let protocol = phux_client::state::probe_hello(&mut conn).await?;
    let view = phux_client::state::get_state_on(&mut conn).await?;
    let (snapshot, degradation) = view.into_parts();
    Ok(build_report(
        socket_path,
        pid,
        socket_mtime_unix_secs(socket_path),
        protocol,
        &snapshot,
        &degradation,
    ))
}

/// Assemble the [`StatusReport`] from its collected parts. Split from
/// [`collect`] so tests can fabricate reports without a socket.
fn build_report(
    socket_path: &Path,
    pid: Option<i32>,
    since_unix_secs: Option<i64>,
    protocol: (u16, u16, u16),
    snapshot: &SessionSnapshot,
    degradation: &Degradation,
) -> StatusReport {
    let mut sessions = snapshot.sessions.clone();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let satellite_terminals = snapshot
        .panes
        .iter()
        .filter(|pane| pane.id.host().is_some())
        .map(|pane| crate::selector::format_terminal_id(&pane.id))
        .collect();
    StatusReport {
        socket: socket_path.to_path_buf(),
        pid,
        since_unix_secs,
        protocol,
        sessions,
        satellite_terminals,
        unreachable: degradation.notices().to_vec(),
        server_log: phux_server::telemetry::server_log_path(),
        client_log_dir: phux_server::telemetry::state_dir(),
    }
}

/// The socket file's mtime as Unix seconds — the moment the listener bound
/// it, which is the server's up-since. `None` when the stat fails.
fn socket_mtime_unix_secs(socket_path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(socket_path)
        .and_then(|m| m.modified())
        .ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

/// Wall-clock now as Unix seconds, for the uptime arithmetic.
fn now_unix_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Clients attached across all sessions — the summed real counts the wire
/// carries, not a boolean.
fn total_clients(sessions: &[SessionInfo]) -> u32 {
    sessions
        .iter()
        .map(|s| u32::from(s.attached_client_count))
        .sum()
}

/// The human report: six labeled lines (server, since, protocol, clients,
/// sessions, logs), with one indented line per session and per satellite
/// Terminal under `sessions:`, and — when the fleet view is partial — one
/// indented `partial view:` line per unreachable satellite. Pure, so tests
/// pin the exact render on a fabricated report.
fn render_human(report: &StatusReport, now_unix_secs: i64) -> Vec<String> {
    let mut lines = Vec::new();
    let pid = report
        .pid
        .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string());
    lines.push(format!(
        "server: running (pid {pid}) at {}",
        report.socket.display()
    ));
    lines.push(report.since_unix_secs.map_or_else(
        || "since: unknown".to_owned(),
        |secs| {
            format!(
                "since: {} (up {})",
                format_utc(secs),
                format_uptime(now_unix_secs.saturating_sub(secs))
            )
        },
    ));
    let (major, minor, patch) = report.protocol;
    lines.push(format!("protocol: {major}.{minor}.{patch}"));
    lines.push(format!(
        "clients: {} attached",
        total_clients(&report.sessions)
    ));
    let sessions_label = if report.satellite_terminals.is_empty() {
        format!("sessions: {}", report.sessions.len())
    } else {
        let n = report.satellite_terminals.len();
        let plural = if n == 1 { "" } else { "s" };
        format!(
            "sessions: {} (+ {n} satellite terminal{plural})",
            report.sessions.len()
        )
    };
    lines.push(sessions_label);
    for session in &report.sessions {
        lines.push(format!("  {}", ls::format_session_line(session)));
    }
    for terminal in &report.satellite_terminals {
        lines.push(format!("  {terminal}: satellite terminal"));
    }
    for notice in &report.unreachable {
        lines.push(format!("  partial view: {notice}"));
    }
    lines.push(format!(
        "logs: server {}; clients {}",
        report.server_log.display(),
        report.client_log_dir.join("client-<pid>.log").display()
    ));
    lines
}

/// `secs` since the epoch as a fixed-format UTC timestamp. UTC rather than
/// local time so the render is deterministic (testable) and greppable across
/// machines; the relative `up ...` beside it is what a human actually reads.
fn format_utc(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0).map_or_else(
        || "unknown".to_owned(),
        |dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    )
}

/// A duration in seconds as the two most significant units: `2d 3h`,
/// `3h 24m`, `5m 12s`, `42s`. Negative (clock skew) clamps to `0s`.
fn format_uptime(secs: i64) -> String {
    let secs = u64::try_from(secs.max(0)).unwrap_or(0);
    let (days, hours, minutes) = (secs / 86_400, (secs % 86_400) / 3_600, (secs % 3_600) / 60);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{secs}s"),
        (0, 0, m) => format!("{m}m {}s", secs % 60),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// The `--json` success document. `unreachable` is always present — empty
/// when the fleet view is complete — so a consumer reads completeness
/// positively, mirroring `phux ls --json`.
fn status_document(report: &StatusReport) -> serde_json::Value {
    let (major, minor, patch) = report.protocol;
    serde_json::json!({
        "schema_version": STATUS_SCHEMA_VERSION,
        "running": true,
        "pid": report.pid,
        "socket": report.socket.display().to_string(),
        "since_unix_secs": report.since_unix_secs,
        "protocol": { "major": major, "minor": minor, "patch": patch },
        "clients": total_clients(&report.sessions),
        "sessions": report.sessions.iter().map(|s| serde_json::json!({
            "name": s.name,
            "windows": s.window_count,
            "attached_clients": s.attached_client_count,
        })).collect::<Vec<_>>(),
        "satellite_terminals": report.satellite_terminals,
        "unreachable": report.unreachable,
        "logs": {
            "server": report.server_log.display().to_string(),
            "client_dir": report.client_log_dir.display().to_string(),
        },
    })
}

/// Print the success document on stdout.
fn print_json(report: &StatusReport) -> ExitCode {
    match serde_json::to_string_pretty(&status_document(report)) {
        Ok(s) => {
            outln!("{s}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &json_err::CliError::new(
                json_err::codes::JSON_SERIALIZE,
                format!("failed to serialize status as JSON: {err}"),
                "re-run without --json",
            ),
            1,
        ),
    }
}

/// The `--json` answer for a socket nobody is listening on: `running: false`
/// **on stdout**, exit 1. A status question about a stopped server has an
/// answer, not an error — but the embedded `error` / `remedy` fields use the
/// shared contract vocabulary so a consumer branches on the same
/// `code` strings everywhere. Pure for tests.
fn not_running_document(err: &AttachError, socket_path: &Path) -> serde_json::Value {
    let cli_err = json_err::no_server_error(err, socket_path, "status");
    serde_json::json!({
        "schema_version": STATUS_SCHEMA_VERSION,
        "running": false,
        "socket": socket_path.display().to_string(),
        "error": { "code": cli_err.code, "message": cli_err.message },
        "remedy": cli_err.remedy,
    })
}

/// Whether `err` means "nobody is listening at the socket" — the connect-time
/// signature the no-server family keys on.
fn is_no_server(err: &AttachError) -> bool {
    matches!(
        err,
        AttachError::Io(io_err) if matches!(
            io_err.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
        )
    )
}

/// Route a collection failure to its report:
///
/// - no server + `--json`: the `running: false` answer document on stdout,
///   exit 1;
/// - no server, human: the same multi-line no-server diagnostic every other
///   verb prints (start commands, server log, doctor pointer), exit 1;
/// - anything else (the server hung up mid-probe, a refusal): the shared
///   JSON error contract / prose remedy, exit 1.
fn report_failure(json: bool, err: &AttachError, socket_path: &Path) -> ExitCode {
    if json && is_no_server(err) {
        let doc = not_running_document(err, socket_path);
        match serde_json::to_string_pretty(&doc) {
            Ok(s) => outln!("{s}"),
            // A Value of strings and numbers cannot fail to serialize; the
            // fallback keeps the failure visible rather than silent.
            Err(_) => eprintln!("phux: no server running at {}", socket_path.display()),
        }
        return ExitCode::from(1);
    }
    json_err::report_no_server(json, err, socket_path, "status")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::path::{Path, PathBuf};

    use phux_client::attach::AttachError;
    use phux_client::state::Degradation;
    use phux_protocol::wire::frame::FrameKind;
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot, TerminalInfo};
    use phux_protocol::{SessionId, TerminalId, WindowId};

    use super::{
        StatusReport, build_report, format_uptime, not_running_document, render_human,
        status_document,
    };

    fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
        SessionInfo::new(SessionId::new(1), name)
            .with_window_count(windows)
            .with_attached_client_count(clients)
    }

    /// A fabricated running-server report with two sessions, one satellite
    /// Terminal, and fixed paths — the fixture both render tests pin.
    fn fabricated() -> StatusReport {
        StatusReport {
            socket: PathBuf::from("/run/phux/phux.sock"),
            pid: Some(4242),
            since_unix_secs: Some(1_754_000_000),
            protocol: (1, 2, 3),
            sessions: vec![session("scratch", 1, 0), session("work", 3, 2)],
            satellite_terminals: vec!["build-box/@1".to_owned()],
            unreachable: Vec::new(),
            server_log: PathBuf::from("/state/phux/server.log"),
            client_log_dir: PathBuf::from("/state/phux"),
        }
    }

    /// The six-line human report, pinned exactly: labeled server / since /
    /// protocol / clients / sessions / logs lines, with the per-session
    /// lines (real client counts) and the satellite split indented under
    /// `sessions:`.
    #[test]
    fn human_render_is_pinned() {
        // now = since + 3h24m, so the uptime is deterministic.
        let lines = render_human(&fabricated(), 1_754_000_000 + 3 * 3600 + 24 * 60);
        assert_eq!(
            lines,
            vec![
                "server: running (pid 4242) at /run/phux/phux.sock".to_owned(),
                "since: 2025-07-31 22:13:20 UTC (up 3h 24m)".to_owned(),
                "protocol: 1.2.3".to_owned(),
                "clients: 2 attached".to_owned(),
                "sessions: 2 (+ 1 satellite terminal)".to_owned(),
                "  scratch: 1 window".to_owned(),
                "  work: 3 windows (2 clients attached)".to_owned(),
                "  build-box/@1: satellite terminal".to_owned(),
                "logs: server /state/phux/server.log; clients /state/phux/client-<pid>.log"
                    .to_owned(),
            ]
        );
    }

    /// The JSON success document, pinned: `schema_version` 1, running true,
    /// the protocol triple as an object, summed clients, per-session rows
    /// with real counts, the satellite split, and both log paths.
    #[test]
    fn json_render_is_pinned() {
        let doc = status_document(&fabricated());
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["running"], true);
        assert_eq!(doc["pid"], 4242);
        assert_eq!(doc["socket"], "/run/phux/phux.sock");
        assert_eq!(doc["since_unix_secs"], 1_754_000_000_i64);
        assert_eq!(doc["protocol"]["major"], 1);
        assert_eq!(doc["protocol"]["minor"], 2);
        assert_eq!(doc["protocol"]["patch"], 3);
        assert_eq!(doc["clients"], 2, "clients are summed across sessions");
        assert_eq!(doc["sessions"][0]["name"], "scratch");
        assert_eq!(doc["sessions"][1]["name"], "work");
        assert_eq!(doc["sessions"][1]["windows"], 3);
        assert_eq!(doc["sessions"][1]["attached_clients"], 2);
        assert_eq!(doc["satellite_terminals"][0], "build-box/@1");
        assert_eq!(
            doc["unreachable"].as_array().map(Vec::len),
            Some(0),
            "a complete view still carries the (empty) unreachable list"
        );
        assert_eq!(doc["logs"]["server"], "/state/phux/server.log");
        assert_eq!(doc["logs"]["client_dir"], "/state/phux");
    }

    /// A degraded fleet view surfaces in both formats: an indented
    /// `partial view:` line in the human report, the notice in the JSON
    /// `unreachable` list.
    #[test]
    fn degradation_surfaces_in_both_formats() {
        let mut report = fabricated();
        report.unreachable = vec!["satellite build-box is unreachable: link is down".to_owned()];
        let lines = render_human(&report, 1_754_000_000);
        assert!(
            lines.contains(
                &"  partial view: satellite build-box is unreachable: link is down".to_owned()
            ),
            "human report must name the unreachable satellite: {lines:?}"
        );
        let doc = status_document(&report);
        assert_eq!(
            doc["unreachable"][0],
            "satellite build-box is unreachable: link is down"
        );
    }

    /// Unknown pid and unstattable socket degrade to honest words, not
    /// invented values.
    #[test]
    fn unknown_pid_and_since_render_as_unknown() {
        let mut report = fabricated();
        report.pid = None;
        report.since_unix_secs = None;
        let lines = render_human(&report, 0);
        assert!(lines[0].starts_with("server: running (pid unknown) at "));
        assert_eq!(lines[1], "since: unknown");
        let doc = status_document(&report);
        assert!(doc["pid"].is_null());
        assert!(doc["since_unix_secs"].is_null());
    }

    /// The no-server `--json` answer: `running: false` with the shared
    /// contract's `no_server` code and a remedy naming the start commands —
    /// the documented stdout shape for a stopped server.
    #[test]
    fn not_running_document_is_pinned() {
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let doc = not_running_document(&refused, Path::new("/tmp/phux-test.sock"));
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["running"], false);
        assert_eq!(doc["socket"], "/tmp/phux-test.sock");
        assert_eq!(doc["error"]["code"], "no_server");
        assert_eq!(
            doc["error"]["message"],
            "no server running at /tmp/phux-test.sock"
        );
        let remedy = doc["remedy"].as_str().unwrap();
        assert!(remedy.contains("`phux server`"));
        assert!(remedy.contains("phux doctor"));
        // Exactly the five top-level keys; a consumer may deny-list nothing.
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(5));
    }

    /// `build_report` name-sorts sessions and splits satellite Terminals out
    /// of the pane list, and carries the degradation notices through.
    #[test]
    fn build_report_sorts_and_splits() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::local(1))
                .with_sessions(vec![session("beta", 1, 1), session("alpha", 2, 0)])
                .with_panes(vec![
                    TerminalInfo::new(TerminalId::local(1), WindowId::new(1), 80, 24),
                    TerminalInfo::new(
                        TerminalId::satellite("build-box", 1),
                        WindowId::new(1),
                        80,
                        24,
                    ),
                ]);
        let degradation = Degradation::from_interleaved(&[FrameKind::Error {
            request_id: None,
            code: phux_protocol::wire::frame::ErrorCode::UnsupportedSatelliteRoute,
            message: "satellite build-box is unreachable".to_owned(),
        }]);
        let report = build_report(
            Path::new("/tmp/s.sock"),
            Some(7),
            Some(1),
            (0, 1, 0),
            &snapshot,
            &degradation,
        );
        let names: Vec<_> = report.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"]);
        assert_eq!(report.satellite_terminals.len(), 1);
        assert!(report.satellite_terminals[0].contains("build-box"));
        assert_eq!(
            report.unreachable,
            ["satellite build-box is unreachable".to_owned()]
        );
    }

    #[test]
    fn uptime_renders_two_most_significant_units() {
        assert_eq!(format_uptime(-5), "0s");
        assert_eq!(format_uptime(42), "42s");
        assert_eq!(format_uptime(5 * 60 + 12), "5m 12s");
        assert_eq!(format_uptime(3 * 3600 + 24 * 60), "3h 24m");
        assert_eq!(format_uptime(2 * 86_400 + 3 * 3600 + 60), "2d 3h");
    }
}
