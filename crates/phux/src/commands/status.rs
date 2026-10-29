//! `phux status` — one glance at the server behind the socket.
//! [`collect`] gathers a [`StatusReport`]; [`render_human`] and
//! [`status_document`] are pure renderers, pinned by unit tests.
//!
//! Sources, with no wire change: pid from the UDS peer credentials; since from
//! the socket file's mtime (survives graceful upgrade); protocol from a real
//! HELLO; clients, sessions, and degradation from `GET_STATE`; log paths from
//! `phux_server::telemetry`.
//!
//! With no server: the human path prints the shared no-server diagnostic;
//! `--json` answers `{"running": false, ...}` on stdout (exit 1) using the
//! error-contract vocabulary. Failures after connect use the shared emitter.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_client::state::Degradation;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, ls};
// Feature bits by their `snake_case` name (the `docs/protocol.md` constant
// lower-cased), from the table `--capabilities --json` shares.

/// Version of the `phux status --json` document. Additive fields do not
/// bump it.
pub(crate) const STATUS_SCHEMA_VERSION: u8 = 1;

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
    /// The protocol version triple the server answered `HELLO_OK` with. On
    /// the same major it may be older or newer than [`Self::client_protocol`].
    protocol: (u16, u16, u16),
    /// This client's own protocol triple, [`phux_protocol::PROTOCOL_VERSION`].
    client_protocol: (u16, u16, u16),
    /// The additive server features `HELLO_OK` advertised, as their
    /// `snake_case` names — the capability probe for callers that cannot speak
    /// the wire themselves.
    features: Vec<&'static str>,
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
    let features = phux_client::state::probe_hello_features(&mut conn)
        .await?
        .unwrap_or_default();
    let view = phux_client::session_list::listing_view(&mut conn).await?;
    drop(conn);
    let (snapshot, degradation) = view.into_parts();
    Ok(build_report(
        socket_path,
        pid,
        socket_mtime_unix_secs(socket_path),
        protocol,
        features,
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
    features: ServerFeatureSet,
    snapshot: &SessionSnapshot,
    degradation: &Degradation,
) -> StatusReport {
    let mut sessions = snapshot.sessions.clone();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let satellite_terminals = snapshot
        .resources
        .iter()
        .filter(|pane| pane.id.host().is_some())
        .map(|pane| phux_client::selector::format_terminal_id(&pane.id))
        .collect();
    StatusReport {
        socket: socket_path.to_path_buf(),
        pid,
        since_unix_secs,
        protocol,
        client_protocol: (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        ),
        features: features.iter().map(ServerFeature::snake_name).collect(),
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

/// The human report: six labeled lines, indented per-session and
/// per-satellite lines under `sessions:`, and one `partial view:` line per
/// unreachable satellite.
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
    lines.push(protocol_line(report.protocol, report.client_protocol));
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

/// `protocol: <server>`, naming this client's own version only when the two
/// differ (same major interoperates, so a skew is informational).
fn protocol_line(server: (u16, u16, u16), client: (u16, u16, u16)) -> String {
    let (major, minor, patch) = server;
    let line = format!("protocol: {major}.{minor}.{patch}");
    if server == client {
        return line;
    }
    let (major, minor, patch) = client;
    format!("{line} (this client: {major}.{minor}.{patch})")
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
    let (client_major, client_minor, client_patch) = report.client_protocol;
    serde_json::json!({
        "schema_version": STATUS_SCHEMA_VERSION,
        "running": true,
        "pid": report.pid,
        "socket": report.socket.display().to_string(),
        "since_unix_secs": report.since_unix_secs,
        "protocol": { "major": major, "minor": minor, "patch": patch },
        "client_protocol": {
            "major": client_major,
            "minor": client_minor,
            "patch": client_patch,
        },
        "features": report.features,
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
    crate::output::json(&status_document(report))
}

/// The `--json` answer when nothing listens: `running: false` on stdout,
/// exit 1, with contract-vocabulary `error` / `remedy` fields.
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

/// Route a collection failure: no server under `--json` is the
/// `running: false` document, no server otherwise is the shared diagnostic, and
/// anything else goes through the error contract. Exit 1 in all cases.
fn report_failure(json: bool, err: &AttachError, socket_path: &Path) -> ExitCode {
    if json && is_no_server(err) {
        let doc = not_running_document(err, socket_path);
        match serde_json::to_string_pretty(&doc) {
            Ok(s) => outln!("{s}"),
            // A Value of strings and numbers cannot fail to serialize; the
            // fallback keeps the failure visible rather than silent.
            Err(_) => error!("no server running at {}", socket_path.display()),
        }
        return ExitCode::from(1);
    }
    json_err::report_no_server(json, err, socket_path, "status")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests;
