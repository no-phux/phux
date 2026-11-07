//! Agent evidence on the parent's record, and resuming from a cursor.

use super::*;

/// Write an executable named `claude` (so the detector identifies the pane
/// by its foreground argv) that just sleeps.
fn write_fake_claude(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{FOREVER}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

/// Poll `phux.agent/v1` on `parent` until it contains `needle` (the arbiter
/// writes it off a side-channel drain task, not inline with the reply).
async fn await_agent_record(stream: &mut UnixStream, base: u32, parent: &ResourceId, needle: &str) {
    let deadline = tokio::time::Instant::now() + phux_server_testkit::WIRE_RECV_TIMEOUT;
    for request_id in base.. {
        let scope = Scope::Resource(parent.clone());
        let key = phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
        if get_metadata(stream, request_id, scope, key)
            .await
            .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "phux.agent/v1 never showed {needle}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Stream evidence projects onto the parent's `phux.agent/v1` record
/// (prompt -> working, stop -> done), and `REPORT_AGENT_STATE` with a live
/// child lands as a `state` record on the child's stream too.
#[test]
fn stream_evidence_and_reported_state_project_onto_the_parents_agent_record() {
    // SAFETY: nextest runs each test in its own process; no thread exists yet.
    unsafe {
        std::env::set_var("PHUX_AGENT_STARTUP_GRACE_MS", "200");
        std::env::set_var("PHUX_AGENT_IDENTIFY_RECHECK_MS", "200");
    }
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let claude = write_fake_claude(server.tmp.path());
        let parent = spawned(&mut owner, 1, Spawn::command(&[&claude])).await;
        let SpawnResult::Ok(session) = spawn_session(&mut owner, 2, parent.clone()).await else {
            panic!("spawn session failed");
        };

        append(&mut owner, 3, &session, "{\"type\":\"prompt\"}\n").await;
        await_agent_record(&mut owner, 100, &parent, "\"state\":\"working\"").await;
        append(&mut owner, 4, &session, "{\"type\":\"stop\"}\n").await;
        await_agent_record(&mut owner, 200, &parent, "\"state\":\"done\"").await;

        let mut watcher = server.connect().await;
        attach_resource(&mut watcher, 100, &session).await;
        let _ = bootstrap(&mut watcher, &session).await;
        let report = Command::ReportAgentState {
            terminal_id: parent.clone(),
            state: ReportedAgentState::Blocked,
        };
        assert_eq!(command(&mut owner, 5, report).await, CommandResult::Ok);
        let (_, record) = next_output(&mut watcher, &session).await;
        let record = String::from_utf8_lossy(&record);
        assert!(
            record.contains("\"type\":\"state\"") && record.contains("\"state\":\"blocked\""),
            "{record}"
        );
        await_agent_record(&mut owner, 300, &parent, "\"state\":\"blocked\"").await;

        drop((owner, watcher));
        server.stop().await;
    });
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one production connection exercises the resume/replace and reattach ordering contract"
)]
fn agent_resume_dispatches_suffix_empty_and_full_fallback_then_one_live_generation() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (_parent, session) = parent_and_session(&mut owner, FOREVER).await;
        append(
            &mut owner,
            10,
            &session,
            "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}",
        )
        .await;
        let mut watcher = UnixStream::connect(&server.socket).await.unwrap();
        send_frame(
            &mut watcher,
            &FrameKind::Hello {
                client_name: "agent-resume-test".to_owned(),
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                client_caps: phux_protocol::caps::ClientCapabilities::new(),
            },
        )
        .await;
        let caps = wait_frame(&mut watcher, "resume capability", |frame| match frame {
            FrameKind::HelloOk { server_caps, .. } => Some(server_caps),
            _ => None,
        })
        .await;
        assert!(
            caps.features_ext
                .contains(phux_protocol::caps::ServerFeatureExt::AgentStreamResume)
        );
        attach_after(&mut watcher, 100, &session, Some(1)).await;
        let (grid, profile, base_seq, suffix) = bootstrap(&mut watcher, &session).await;
        assert_eq!(grid, (0, 0));
        assert_eq!(
            profile,
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 1 }
        );
        assert_eq!(base_seq, 3);
        assert_eq!(record_seqs(&suffix), [2, 3]);

        attach_after(&mut watcher, 101, &session, Some(3)).await;
        let (_, profile, base_seq, empty) = bootstrap(&mut watcher, &session).await;
        assert_eq!(
            profile,
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 3 }
        );
        assert_eq!(base_seq, 3);
        assert!(empty.is_empty());

        attach_after(&mut watcher, 102, &session, Some(4)).await;
        let (_, profile, base_seq, full) = bootstrap(&mut watcher, &session).await;
        assert_eq!(profile, BootstrapStreamProfile::AgentEventsJsonlV1);
        assert_eq!(base_seq, 3);
        assert_eq!(record_seqs(&full), [1, 2, 3]);
        append(
            &mut owner,
            11,
            &session,
            "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}",
        )
        .await;
        let first_live = next_output(&mut watcher, &session).await;
        assert_eq!((first_live.0, record_seqs(&first_live.1)), (5, vec![4, 5]));
        send_frame(
            &mut watcher,
            &FrameKind::Command {
                request_id: 103,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await;
        wait_frame(&mut watcher, "live generation fence", |frame| match frame {
            FrameKind::ResourceOutput { terminal_id, .. } if terminal_id == session => {
                panic!("a replaced agent pump emitted duplicate live output");
            }
            FrameKind::CommandResult {
                request_id: 103,
                result,
            } => {
                assert!(matches!(result, CommandResult::OkWith(_)));
                Some(())
            }
            _ => None,
        })
        .await;
        drop((owner, watcher));
        server.stop().await;
    });
}

#[test]
fn agent_resume_dispatcher_handles_empty_zero_budget_and_evicted_boundaries() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|cfg| cfg.agent_log_bytes = 0).await;
        let (_parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let mut watcher = server.connect().await;
        attach_after(&mut watcher, 100, &session, Some(0)).await;
        let (_, profile, seq, payload) = bootstrap(&mut watcher, &session).await;
        assert_eq!(
            profile,
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 0 }
        );
        assert_eq!(seq, 0);
        assert!(payload.is_empty());
        attach_resource(&mut watcher, 101, &session).await;
        let (_, profile, _, payload) = bootstrap(&mut watcher, &session).await;
        assert_eq!(profile, BootstrapStreamProfile::AgentEventsJsonlV1);
        assert!(
            payload.is_empty(),
            "empty full replacement still reaches READY"
        );
        append(
            &mut owner,
            10,
            &session,
            "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}",
        )
        .await;
        for (request, cursor, expected) in [
            (102, 1, BootstrapStreamProfile::AgentEventsJsonlV1),
            (
                103,
                2,
                BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 2 },
            ),
        ] {
            attach_after(&mut watcher, request, &session, Some(cursor)).await;
            let (_, profile, seq, payload) = bootstrap(&mut watcher, &session).await;
            assert_eq!(profile, expected);
            assert_eq!(seq, 3);
            assert_eq!(record_seqs(&payload), [3]);
        }
        append(&mut owner, 11, &session, "{\"type\":\"prompt\"}").await;
        let (seq, payload) = next_output(&mut watcher, &session).await;
        assert_eq!((seq, record_seqs(&payload)), (4, vec![4]));
        drop((owner, watcher));
        server.stop().await;
    });
}

#[test]
fn terminal_agent_cursor_is_invalid_before_role_or_content_subscription_changes() {
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::focus::FocusEvent;
    use phux_protocol::wire::frame::RolePolicy;
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let mut watcher = server.connect().await;
        let route = || Command::RouteInput {
            terminal_id: parent.clone(),
            event: InputEvent::Focus(FocusEvent::Gained),
        };
        let before_input = command(&mut watcher, 100, route()).await;
        let before = state(&mut owner, 2).await;
        for (request_id, role_policy) in [(101, RolePolicy::VIEWER), (102, RolePolicy::TAKEOVER)] {
            let result = command(
                &mut watcher,
                request_id,
                Command::attach_agent_session(parent.clone(), Some(role_policy), Some(0)),
            )
            .await;
            assert!(matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    ..
                }
            ));
        }
        assert_eq!(
            command(&mut watcher, 103, route()).await,
            before_input,
            "no viewer mark was installed"
        );
        let after = state(&mut owner, 3).await;
        let holder = |snapshot: &phux_protocol::wire::info::SessionSnapshot| {
            snapshot
                .resources
                .iter()
                .find(|resource| resource.id == parent)
                .unwrap()
                .input_holder
        };
        assert_eq!(holder(&after), holder(&before), "no lease takeover");
        drop((owner, watcher));
        server.stop().await;
    });
}
