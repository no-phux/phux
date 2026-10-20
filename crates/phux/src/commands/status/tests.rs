use std::path::{Path, PathBuf};

use phux_client::attach::AttachError;
use phux_client::state::Degradation;
use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};
use phux_protocol::{ResourceId, SessionId, WindowId};

use super::{
    StatusReport, build_report, format_uptime, not_running_document, render_human, status_document,
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
        client_protocol: (1, 2, 3),
        features: vec!["acknowledged_input", "report_agent_state"],
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
            "logs: server /state/phux/server.log; clients /state/phux/client-<pid>.log".to_owned(),
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
    assert_eq!(doc["client_protocol"]["minor"], 2);
    assert_eq!(
        doc["features"],
        serde_json::json!(["acknowledged_input", "report_agent_state"]),
        "advertised features are named, in bit order"
    );
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
/// A server on another minor or patch prints its own version, with this
/// client's beside it, in both formats.
#[test]
fn a_skewed_server_reports_its_version_and_the_clients() {
    let mut report = fabricated();
    report.protocol = (1, 0, 0);
    report.client_protocol = (1, 4, 2);
    let lines = render_human(&report, 1_754_000_000);
    assert_eq!(lines[2], "protocol: 1.0.0 (this client: 1.4.2)");
    let doc = status_document(&report);
    assert_eq!(doc["protocol"]["minor"], 0, "the server's, not ours");
    assert_eq!(doc["client_protocol"]["minor"], 4);
    assert_eq!(doc["client_protocol"]["patch"], 2);
}

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
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![session("beta", 1, 1), session("alpha", 2, 0)])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
            ResourceInfo::new(
                ResourceId::satellite("build-box", 1),
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
        ServerFeatureSet::with(&[ServerFeature::Shutdown]),
        &snapshot,
        &degradation,
    );
    let names: Vec<_> = report.sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert_eq!(report.features, ["shutdown"]);
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
