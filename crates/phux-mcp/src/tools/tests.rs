#[cfg(test)]
mod tests {
    use super::*;
    use phux_client::state::Degradation;
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::frame::{
        Command as WireCommand, ErrorCode, FrameKind, RESOURCE_TAGS_KEY, Scope,
    };
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

    /// An old-protocol server stays on `transport`. The remedy names `kill`
    /// when the socket-owner pid is known; otherwise it asks to inspect that socket.
    #[test]
    fn an_incompatible_transport_error_names_the_stop_recovery() {
        let socket = Path::new("/tmp/old.sock");
        let err = AttachError::IncompatibleProtocol(
            "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
             update the phux server so protocol major.minor match"
                .to_owned(),
        );
        let with_pid = render_transport_error(&err, socket, "spawn", Some(4242));
        let doc = with_pid.document();
        assert_eq!(doc["error"]["code"], "transport");
        assert_eq!(doc["exit_code"], 1);
        let remedy = doc["remedy"].as_str().unwrap_or_default();
        assert!(remedy.contains("kill 4242"), "{remedy}");
        assert!(
            remedy.contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
            "{remedy}"
        );
        assert!(
            remedy.contains("phux server --ensure --socket '/tmp/old.sock'`"),
            "{remedy}"
        );
        for forbidden in [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(!remedy.contains(forbidden), "{forbidden} in {remedy}");
        }

        let unknown = render_transport_error(&err, socket, "spawn", None);
        let unknown_doc = unknown.document();
        let remedy = unknown_doc["remedy"].as_str().unwrap_or_default();
        assert!(remedy.contains("`lsof -nP -- '/tmp/old.sock'`"), "{remedy}");
        assert!(!remedy.contains("`kill "), "{remedy}");
        for forbidden in [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(!remedy.contains(forbidden), "{forbidden} in {remedy}");
        }
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

    #[test]
    fn compiled_skill_teaches_discovery_and_load_bearing_rules() {
        let skill = crate::SKILL;
        for rule in [
            "tools/list",
            "phux mcp --schema",
            "finite timeout",
            "level read",
            "observed transition",
            "delivery_unknown",
            "confirm: true",
            "human's client-local focus",
        ] {
            assert!(skill.contains(rule), "compiled MCP skill omits {rule:?}");
        }
        assert!(!skill.contains("ADR-"));
        assert!(!skill.contains("docs/"));
    }

    #[tokio::test]
    async fn added_tool_dispatch_routes_to_strict_validation() {
        let kill_error = dispatch("phux_kill", &json!({ "target": "@1" }))
            .await
            .unwrap_err();
        assert_eq!(
            kill_error.0, "phux_kill is destructive; pass `confirm: true`",
            "kill must reject before discovering or starting the CLI",
        );
        let detach_error = dispatch("phux_detach", &json!({})).await.unwrap_err();
        assert_eq!(
            detach_error.0,
            "phux_detach is destructive; pass `confirm: true`"
        );
        assert!(
            dispatch("phux_detach", &json!({ "confirm": false }))
                .await
                .unwrap_err()
                .0
                .contains("confirm: true"),
        );
        assert!(
            dispatch(
                "phux_detach",
                &json!({ "confirm": true, "session": "work", "target": "@1" })
            )
            .await
            .is_err(),
            "phux_detach has no `target` — it addresses a session, not a pane",
        );
        assert!(dispatch("phux_launch", &json!({})).await.is_err());
        let paste_error = dispatch("phux_paste", &json!({ "target": "@1" }))
            .await
            .unwrap_err();
        assert_eq!(paste_error.0, "missing required argument `text`");
        assert!(
            dispatch(
                "phux_paste",
                &json!({ "target": "@1", "text": "x", "keys": ["a"] })
            )
            .await
            .is_err()
        );
        assert!(
            dispatch("phux_signal", &json!({ "target": "@1", "signal": "kill" }))
                .await
                .is_err()
        );
        assert!(
            dispatch("phux_insert_pane", &json!({ "target": "@1" }))
                .await
                .is_err()
        );
        assert!(
            dispatch("phux_workspace", &json!({ "action": "delete" }))
                .await
                .is_err()
        );
        assert_eq!(
            dispatch("phux_agent", &json!({ "action": "list" }))
                .await
                .unwrap_err()
                .0,
            "unknown tool: phux_agent",
        );
        assert_eq!(
            dispatch("phux_agent_set", &json!({ "target": "@1" }))
                .await
                .unwrap_err()
                .0,
            "missing required argument `name`",
        );
        assert_eq!(
            dispatch("phux_agent_wait", &json!({ "until": ["unknown"] }))
                .await
                .unwrap_err()
                .0,
            "`until` must contain only: idle, working, blocked, done (got \"unknown\"; \
             'unknown' is a departure, not a waitable state)",
        );
        assert_eq!(
            dispatch(
                "phux_snapshot",
                &json!({ "target": "@1", "unwrap": true, "cells": true })
            )
            .await
            .unwrap_err()
            .0,
            "`unwrap` cannot be combined with `cells`: cell coordinates are grid \
             coordinates and do not survive the join",
        );
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

    /// Absent ⇒ `Current`; malformed and headless `=` ⇒ error.
    #[test]
    fn parse_target_defaults_and_accepts_grammar() {
        assert_eq!(parse_target(&json!({})).unwrap(), Selector::Current);
        assert_eq!(
            parse_target(&json!({ "target": "." })).unwrap(),
            Selector::Current,
        );
        assert_eq!(
            parse_target(&json!({ "target": "work:1.2" })).unwrap(),
            Selector::Pane("work".to_owned(), selector::GroupRef::Index(1), 2),
        );
        assert_eq!(
            parse_target(&json!({ "target": "@100" })).unwrap(),
            Selector::ResourceId(100),
        );
        assert!(parse_target(&json!({ "target": "@nope" })).is_err());
        let err = parse_target(&json!({ "target": "=" })).unwrap_err();
        assert!(err.0.contains("attached-TUI focus history"), "{err:?}");
    }

    #[tokio::test]
    async fn resolve_one_maps_every_selector_form() {
        let whole = whole_fleet();
        let socket = std::path::Path::new("unused-for-non-tag-selectors");

        assert_eq!(
            resolve_one(socket, &selector::parse("work").unwrap(), &whole)
                .await
                .unwrap(),
            ResourceId::local(100),
        );
        for (raw, expected) in [
            ("work:1", ResourceId::local(101)),
            ("work:editor", ResourceId::local(101)),
            ("work:1.1", ResourceId::local(102)),
            ("@200", ResourceId::local(200)),
            ("devbox/@7", ResourceId::satellite("devbox", 7)),
        ] {
            assert_eq!(
                resolve_one(socket, &selector::parse(raw).unwrap(), &whole)
                    .await
                    .unwrap(),
                expected,
            );
        }
        assert_eq!(
            resolve_one(socket, &Selector::Current, &whole)
                .await
                .unwrap(),
            ResourceId::local(100),
        );
        // Misses error.
        assert!(
            resolve_one(socket, &selector::parse("ghost").unwrap(), &whole)
                .await
                .is_err()
        );
        assert!(
            resolve_one(socket, &selector::parse("@999").unwrap(), &whole)
                .await
                .is_err()
        );
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

    /// `#tag` resolution fetches the tag index, then prefers the focused pane.
    #[tokio::test]
    async fn resolve_one_fetches_tag_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp-tag.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let spec = ScriptSpec::new().metadata(|scope, key| {
            assert_eq!(key, RESOURCE_TAGS_KEY);
            let Scope::Resource(terminal_id) = scope else {
                panic!("tag lookup must be terminal-scoped, got {scope:?}");
            };
            matches!(terminal_id.local_id(), Some(100 | 200))
                .then(|| serde_json::to_vec(&vec!["build"]).unwrap())
        });
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let pane = resolve_one(&socket, &selector::parse("#build").unwrap(), &whole_fleet())
            .await
            .unwrap();
        assert_eq!(pane, ResourceId::local(100));
        let seen = server.await.unwrap();
        // One lookup per pane, satellite panes included.
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "the client must negotiate before metadata lookup; got {seen:?}"
        );
        let metadata = &seen[1..];
        assert_eq!(
            metadata.len(),
            fixture().resources.len(),
            "one GET_METADATA per pane in the snapshot, pipelined; got {seen:?}"
        );
        assert!(
            metadata
                .iter()
                .all(|frame| matches!(frame, FrameKind::GetMetadata { .. })),
            "HELLO must be followed only by GET_METADATA; got {seen:?}"
        );
    }

    /// `phux_detach` sends `DetachClients { session }` and reports the count
    /// the server acked, not an echo of the input.
    #[tokio::test]
    async fn phux_detach_reports_the_server_acked_count() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp-detach.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let spec = ScriptSpec::new().detach_result(2);
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let result = dispatch(
            "phux_detach",
            &json!({ "session": "work", "confirm": true, "socket": socket.to_string_lossy() }),
        )
        .await
        .unwrap();
        assert_eq!(result["schema_version"], json!(1));
        assert_eq!(result["detached"], json!(2));
        assert_eq!(result["session"], json!("work"));

        let seen = server.await.unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp-detach-all.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let spec = ScriptSpec::new().detach_result(0);
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let result = dispatch(
            "phux_detach",
            &json!({ "confirm": true, "socket": socket.to_string_lossy() }),
        )
        .await
        .unwrap();
        assert_eq!(
            result["detached"],
            json!(0),
            "0 detached is success, not a miss"
        );
        assert!(result["session"].is_null());

        let seen = server.await.unwrap();
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
