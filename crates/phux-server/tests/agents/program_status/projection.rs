//! How records project into the summary, badges, and agent state, and who
//! may write them.

use super::*;

#[test]
fn clients_cannot_mutate_the_summary_or_record_namespace() {
    run_local(async {
        let mut pane =
            Pane::start(&[fenced(report("state=blocked:app=terraform"), 0)], false).await;
        pane.stage(0).await;
        for key in [
            RESOURCE_PROGRAM_STATUS_KEY.to_owned(),
            record_key(""),
            record_key("forged"),
        ] {
            let before = pane.get(&key).await;
            let request_id = pane.next_request();
            send_frame(
                &mut pane.stream,
                &FrameKind::SetMetadata {
                    request_id,
                    scope: Scope::Resource(pane.terminal.clone()),
                    key: key.clone(),
                    value: br#"{"id":"forged","state":"done"}"#.to_vec(),
                },
            )
            .await;
            assert_eq!(pane.get(&key).await, before, "SET must not land: {key}");
            let request_id = pane.next_request();
            send_frame(
                &mut pane.stream,
                &FrameKind::DeleteMetadata {
                    request_id,
                    scope: Scope::Resource(pane.terminal.clone()),
                    key: key.clone(),
                },
            )
            .await;
            assert_eq!(pane.get(&key).await, before, "DELETE must not land: {key}");
        }
        pane.assert_ids(&[""]).await;
        pane.stop().await;
    });
}

#[test]
fn generic_programs_project_priority_and_attention_into_existing_badges() {
    run_local(async {
        let mut pane = Pane::start(
            &[
                fenced(report("state=working:app=terraform:progress=10"), 0),
                fenced(report("id=result:state=done"), 1),
                fenced(report("id=failure:state=error"), 2),
                fenced(report("id=approval:state=blocked:kind=permission"), 3),
                fenced(report("id=question:state=blocked:kind=question"), 4),
                fenced(report("id=approval:state=blocked:kind=auth"), 5),
                fenced(
                    reports(&[
                        "id=approval:state=clear",
                        "id=question:state=clear",
                        "id=failure:state=clear",
                        "id=result:state=clear",
                        "state=idle:app=terraform",
                    ]),
                    6,
                ),
            ],
            false,
        )
        .await;
        for (stage, id, state, attention) in [
            (0, "", "working", "normal"),
            (1, "result", "done", "low"),
            (2, "failure", "done", "high"),
            (3, "approval", "blocked", "high"),
            (4, "question", "blocked", "high"),
            (5, "approval", "blocked", "high"),
        ] {
            pane.stage(stage).await;
            assert_eq!(pane.summary().await["active"]["id"], id);
            let badge = pane.wait_badge(state, attention).await;
            assert_eq!(
                badge["name"], "terraform",
                "generic apps need no agent manifest"
            );
        }
        pane.stage(6).await;
        pane.wait_badge("idle", "none").await;
        pane.stop().await;
    });
}

#[test]
fn program_status_beats_a_live_screen_heuristic_but_not_an_explicit_state() {
    run_local(async {
        // The same permission dialog used by agent_detect.rs, kept on screen
        // while OSC reports change. First prove the real detector recognizes it.
        let rule = "─".repeat(20);
        let dialog = format!(
            "\x1b[2J\x1b[Htranscript\n\n{rule}\n Bash command\n\n\
             touch /tmp/probe.txt\n\n Do you want to proceed?\n\
             ❯ 1. Yes\n 2. Yes, and always allow access\n 3. No\n\n Esc to cancel\n"
        );
        let mut pane = Pane::start_named(
            &[
                dialog.into_bytes(),
                fenced(report("state=done:app=claude:msg=UmVhZHk="), 1),
                fenced(report("state=working:app=claude:progress=75"), 2),
            ],
            false,
            "claude",
        )
        .await;
        pane.release(0).await;
        pane.wait_badge("blocked", "high").await;
        pane.stage(1).await;
        pane.wait_badge("done", "low").await;
        assert_eq!(pane.record("").await["state"], "done");

        let request_id = pane.next_request();
        send_frame(&mut pane.stream, &FrameKind::SetMetadata {
            request_id,
            scope: Scope::Resource(pane.terminal.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: br#"{"name":"pinned","kind":"claude","state":"blocked","attention":"high","session":"manual"}"#.to_vec(),
        }).await;
        pane.wait_value(RESOURCE_AGENT_KEY, |value| {
            value
                .as_ref()
                .is_some_and(|value| value["name"] == "pinned" && value["state"] == "blocked")
        })
        .await;
        pane.stage(2).await;
        assert_eq!(
            pane.record("").await["state"],
            "working",
            "raw reports remain independent"
        );
        let declared = pane.get(RESOURCE_AGENT_KEY).await.unwrap();
        assert_eq!(declared["name"], "pinned");
        assert_eq!(declared["kind"], "claude");
        assert_eq!(declared["state"], "blocked");
        assert_eq!(declared["attention"], "high");
        assert_eq!(declared["session"], "manual");
        pane.stop().await;
    });
}

#[test]
fn bidi_formatting_stays_in_raw_metadata_but_is_disarmed_in_badge_names() {
    run_local(async {
        let title = "deploy \u{202e}prod\u{202c}";
        let body = format!(
            "state=blocked:app=terraform:title={}",
            STANDARD.encode(title)
        );
        let blank_title = " \u{202e}\u{202c} ";
        let blank_body = format!(
            "state=blocked:app=terraform:title={}",
            STANDARD.encode(blank_title)
        );
        let mut pane = Pane::start(
            &[fenced(report(&body), 0), fenced(report(&blank_body), 1)],
            false,
        )
        .await;
        pane.stage(0).await;
        assert_eq!(pane.record("").await["title"], title);
        assert_eq!(pane.summary().await["active"]["title"], title);
        let badge = pane.wait_badge("blocked", "high").await;
        let name = badge["name"].as_str().expect("badge has a display name");
        assert_eq!(
            name, "deploy prod",
            "disarming controls must preserve label content"
        );
        assert!(
            !name.contains('\u{202e}'),
            "untrusted direction override must not reach chrome"
        );
        assert!(
            !name.contains('\u{202c}'),
            "untrusted direction control must not reach chrome"
        );
        pane.stage(1).await;
        assert_eq!(pane.record("").await["title"], blank_title);
        let badge = pane.wait_badge("blocked", "high").await;
        assert_eq!(
            badge["name"], "terraform",
            "a blank label must not erase the status badge"
        );
        pane.stop().await;
    });
}

#[test]
fn live_agent_stream_keeps_precedence_and_program_status_resumes_without_a_report() {
    run_local(async {
        for finish in [true, false] {
            let mut pane = Pane::start_named(
                &[fenced(report("state=error:app=cargo:msg=RmFpbGVk"), 0)],
                false,
                "claude",
            )
            .await;
            pane.stage(0).await;
            pane.wait_badge("done", "high").await;
            let raw = pane.summary().await;
            let request_id = pane.next_request();
            let spawn = phux_server_testkit::Spawn {
                resource: Some(Box::new(
                    phux_protocol::wire::frame::SpawnResource::agent_session(
                        pane.terminal.clone(),
                        "claude",
                    ),
                )),
                ..phux_server_testkit::Spawn::default()
            };
            let phux_protocol::wire::frame::SpawnResult::Ok(session) =
                phux_server_testkit::spawn_resource(&mut pane.stream, request_id, spawn).await
            else {
                panic!("agent session spawn failed");
            };
            let request_id = pane.next_request();
            let result = command(
                &mut pane.stream,
                request_id,
                Command::AppendResourceOutput {
                    terminal_id: session.clone(),
                    bytes: b"{\"type\":\"prompt\"}\n".to_vec(),
                },
            )
            .await;
            assert!(matches!(result, CommandResult::OkWith(_)), "{result:?}");
            pane.wait_badge("working", "normal").await;
            assert_eq!(
                pane.summary().await,
                raw,
                "the stream does not replace OSC records"
            );
            let request_id = pane.next_request();
            let ending = if finish {
                Command::AppendResourceOutput {
                    terminal_id: session,
                    bytes: b"{\"type\":\"session_end\"}\n".to_vec(),
                }
            } else {
                Command::KillResource {
                    terminal_id: session,
                    operation_id: None,
                }
            };
            let result = command(&mut pane.stream, request_id, ending).await;
            assert!(
                matches!(result, CommandResult::Ok | CommandResult::OkWith(_)),
                "{result:?}"
            );
            pane.wait_badge("done", "high").await;
            assert_eq!(pane.summary().await, raw);
            pane.stop().await;
        }
    });
}
