#[cfg(test)]
mod tests {
    use super::*;
    use phux_client::state::Degradation;
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::frame::{Command as WireCommand, ErrorCode, FrameKind};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn bounded_watch_propagates_an_error_completed_before_timeout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing_socket = dir.path().join("missing.sock");

        let result =
            collect_watch_items(&missing_socket, None, None, Some(Duration::from_secs(1))).await;

        assert!(
            matches!(result, Err(AttachError::Io(_))),
            "the MCP collector must not turn a transport error into an empty success: {result:?}"
        );
    }

    /// The fixture as a server that reached everything it knows about.
    fn whole_fleet() -> StateView {
        StateView::new(fixture(), Degradation::default())
    }

    /// The fixture as a hub that could not reach one satellite: the panes it
    /// would have contributed are simply not in the merge.
    fn partial_fleet() -> StateView {
        StateView::new(
            fixture(),
            Degradation::from_interleaved(&[FrameKind::Error {
                request_id: None,
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite build-box is unreachable: link is down".to_owned(),
            }]),
        )
    }

    #[tokio::test]
    async fn added_tool_dispatch_routes_to_strict_validation() {
        // `Some(message)` pins the refusal text exactly; `None` only requires
        // a refusal. Each case is refused before the CLI is discovered.
        let cases: [(&str, Value, Option<&str>); 8] = [
            // No `target`: detach addresses a session, not a pane.
            (
                "phux_detach",
                json!({ "confirm": true, "session": "work", "target": "@1" }),
                None,
            ),
            (
                "phux_paste",
                json!({ "target": "@1" }),
                Some("missing required argument `text`"),
            ),
            (
                "phux_paste",
                json!({ "target": "@1", "text": "x", "keys": ["a"] }),
                None,
            ),
            (
                "phux_signal",
                json!({ "target": "@1", "signal": "kill" }),
                None,
            ),
            ("phux_insert_pane", json!({ "target": "@1" }), None),
            (
                "phux_agent_set",
                json!({ "target": "@1" }),
                Some("missing required argument `name`"),
            ),
            (
                "phux_agent_wait",
                json!({ "until": ["unknown"] }),
                Some(
                    "`until` must contain only: idle, working, blocked, done (got \"unknown\"; \
                     'unknown' is a departure, not a waitable state)",
                ),
            ),
            (
                "phux_snapshot",
                json!({ "target": "@1", "unwrap": true, "cells": true }),
                Some(
                    "`unwrap` cannot be combined with `cells`: cell coordinates are grid \
                     coordinates and do not survive the join",
                ),
            ),
        ];
        for (tool, args, expected) in cases {
            let error = dispatch(tool, &args)
                .await
                .expect_err(&format!("{tool} {args} must be refused"));
            if let Some(expected) = expected {
                assert_eq!(error.0, expected, "{tool} {args}");
            }
        }
    }

    /// `agent_state` items carry the CLI's `phux watch --json` line shape.
    #[test]
    fn agent_state_json_matches_the_cli_line_shape() {
        use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
        use phux_client::watch::AgentStateUpdate;

        let record = |state: AgentMetaState| AgentRecord {
            name: "bot".to_owned(),
            kind: Some("codex".to_owned()),
            state,
            attention: None,
            session: Some("fleet".to_owned()),
        };

        let moved = AgentStateUpdate {
            terminal: Some(ResourceId::local(7)),
            record: Some(record(AgentMetaState::Blocked)),
            previous: Some(record(AgentMetaState::Working)),
        };
        let value = agent_state_json(&moved);
        assert_eq!(value["event"], json!("agent_state"));
        assert_eq!(value["terminal"], json!("@7"));
        assert_eq!(value["name"], json!("bot"));
        assert_eq!(value["kind"], json!("codex"));
        assert_eq!(value["session"], json!("fleet"));
        assert_eq!(value["state"], json!("blocked"));
        assert_eq!(value["from"], json!("working"));
        assert_eq!(
            value["attention"],
            json!(AgentAttention::High.as_str()),
            "attention is the effective level derived from `blocked`, not the \
             (absent) declared one",
        );

        // The tombstone: `state` present-and-null, identity kept.
        let cleared = AgentStateUpdate {
            terminal: Some(ResourceId::local(7)),
            record: None,
            previous: Some(record(AgentMetaState::Working)),
        };
        let value = agent_state_json(&cleared);
        assert_eq!(value["event"], json!("agent_state"));
        assert!(
            value.get("state").is_some_and(Value::is_null),
            "the tombstone's `state` must be present-and-null, got {value}",
        );
        assert_eq!(value["name"], json!("bot"));
        assert_eq!(value["from"], json!("working"));
        assert!(value.get("attention").is_none());

        let event = WatchItem::Event(phux_client::watch::WatchEvent {
            terminal: None,
            event: phux_protocol::wire::frame::AgentEvent::Bell,
            stamp: None,
        });
        assert_eq!(watch_item_json(&event)["event"], json!("bell"));
        assert_eq!(
            watch_item_json(&WatchItem::AgentState(moved))["event"],
            json!("agent_state"),
        );
    }

    #[test]
    fn snapshot_format_arg_validates_the_closed_vocabulary() {
        assert_eq!(snapshot_format_arg(&json!({})).unwrap(), None);
        assert_eq!(
            snapshot_format_arg(&json!({ "format": "html" })).unwrap(),
            Some("html"),
        );
        assert_eq!(
            snapshot_format_arg(&json!({ "format": "vt" })).unwrap(),
            Some("vt"),
        );
        let err = snapshot_format_arg(&json!({ "format": "png" })).unwrap_err();
        assert!(err.0.contains("png"), "{}", err.0);
    }

    /// Absent ⇒ `Current`; headless `=` ⇒ error.
    #[test]
    fn parse_target_defaults_and_accepts_grammar() {
        assert_eq!(parse_target(&json!({})).unwrap(), Selector::Current);
        let err = parse_target(&json!({ "target": "=" })).unwrap_err();
        assert!(err.0.contains("attached-TUI focus history"), "{err:?}");
    }

    /// A miss against a hub that could not reach a satellite must not be
    /// reported as "no such target": the pane may be alive behind that link.
    #[tokio::test]
    async fn a_miss_against_a_partial_fleet_does_not_claim_the_target_is_absent() {
        let socket = std::path::Path::new("unused-for-non-tag-selectors");
        let selector = selector::parse("@999").unwrap();

        let complete = resolve_one(socket, &selector, &whole_fleet())
            .await
            .expect_err("@999 is in neither fixture");
        assert_eq!(complete.0, "no such target");

        let degraded = resolve_one(socket, &selector, &partial_fleet())
            .await
            .expect_err("@999 is in neither fixture");
        assert!(
            !degraded.0.contains("no such target"),
            "a partial view cannot assert absence, got {:?}",
            degraded.0
        );
        assert!(
            degraded.0.contains("satellite build-box is unreachable"),
            "the message must name what could not be seen, got {:?}",
            degraded.0
        );
    }

    /// One scripted connection on a fresh socket; the handle yields the frames
    /// the client sent.
    fn serve(
        spec: ScriptSpec,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        tokio::task::JoinHandle<Vec<FrameKind>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        (dir, socket, server)
    }

    /// Detach through the MCP tool against a server acking `count` clients.
    async fn detach(count: u64, mut args: Value) -> (Value, Vec<FrameKind>) {
        let (_dir, socket, server) = serve(ScriptSpec::new().detach_result(count));
        args["confirm"] = json!(true);
        args["socket"] = json!(socket.to_string_lossy());
        let result = dispatch("phux_detach", &args).await.unwrap();
        (result, server.await.unwrap())
    }

    /// `phux_detach` sends `DetachClients { session }` and reports the count
    /// the server acked, not an echo of the input.
    #[tokio::test]
    async fn phux_detach_reports_the_server_acked_count() {
        let (result, seen) = detach(2, json!({ "session": "work" })).await;
        assert_eq!(result["schema_version"], json!(1));
        assert_eq!(result["detached"], json!(2));
        assert_eq!(result["session"], json!("work"));
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "the client must negotiate before DETACH_CLIENTS; got {seen:?}"
        );
        assert!(
            matches!(
                seen.get(1),
                Some(FrameKind::Command {
                    command: WireCommand::DetachClients { session },
                    ..
                }) if session.as_deref() == Some("work")
            ),
            "the named session must ride the wire command verbatim; got {seen:?}"
        );
    }

    /// Omitting `session` detaches server-wide: `None` on the wire, and a
    /// present-and-null `session` in the result.
    #[tokio::test]
    async fn phux_detach_omits_session_for_a_server_wide_call() {
        let (result, seen) = detach(0, json!({})).await;
        assert_eq!(
            result["detached"],
            json!(0),
            "0 detached is success, not a miss"
        );
        assert!(result["session"].is_null());
        assert!(
            matches!(
                seen.get(1),
                Some(FrameKind::Command {
                    command: WireCommand::DetachClients { session: None },
                    ..
                })
            ),
            "no session named must ride as None, not an empty string; got {seen:?}"
        );
    }

    /// Build a snapshot: session "work" (id 1, focused, pane 100 focused)
    /// with two windows, plus a second session "play" (pane 200).
    fn fixture() -> SessionSnapshot {
        let work = SessionId::new(1);
        let play = SessionId::new(2);
        let w0 = WindowId::new(10);
        let w1 = WindowId::new(11);
        let p0 = WindowId::new(20);
        let sessions = vec![
            SessionInfo::new(work, "work"),
            SessionInfo::new(play, "play"),
        ];
        let windows = vec![
            WindowInfo::new(w0, work, "shell").with_index(0),
            WindowInfo::new(w1, work, "editor").with_index(1),
            WindowInfo::new(p0, play, "shell").with_index(0),
        ];
        let panes = vec![
            ResourceInfo::new(ResourceId::local(100), w0, 80, 24),
            ResourceInfo::new(ResourceId::local(101), w1, 80, 24),
            ResourceInfo::new(ResourceId::local(102), w1, 80, 24),
            ResourceInfo::new(ResourceId::local(200), p0, 80, 24),
            ResourceInfo::new(
                ResourceId::satellite("devbox", 7),
                WindowId::new(999),
                80,
                24,
            ),
        ];
        SessionSnapshot::new(work, w0, ResourceId::local(100))
            .with_sessions(sessions)
            .with_windows(windows)
            .with_resources(panes)
    }
}
