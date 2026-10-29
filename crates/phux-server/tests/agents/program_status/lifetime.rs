//! How long records live: shell prompts, resets, alternate screens, and
//! process exit.

use super::*;

#[test]
fn a_shell_prompt_expires_live_work_but_preserves_results() {
    run_local(async {
        let mut prompt = b"\x1b]133;A\x07".to_vec();
        prompt.extend(b"$ ");
        let mut pane = Pane::start(
            &[
                fenced(
                    reports(&[
                        "id=running:state=working:progress=20",
                        "id=waiting:state=blocked:kind=question",
                        "id=success:state=done:msg=UmVhZHk=",
                        "id=failure:state=error:msg=RmFpbGVk",
                    ]),
                    0,
                ),
                fenced(prompt, 1),
            ],
            false,
        )
        .await;
        pane.stage(0).await;
        let success = pane.record("success").await;
        let failure = pane.record("failure").await;
        pane.stage(1).await;
        pane.assert_ids(&["success", "failure"]).await;
        assert_eq!(pane.get(&record_key("running")).await, None);
        assert_eq!(pane.get(&record_key("waiting")).await, None);
        assert_eq!(pane.record("success").await, success);
        assert_eq!(pane.record("failure").await, failure);
        assert_eq!(pane.summary().await["active"], failure);
        pane.stop().await;
    });
}

#[test]
fn soft_reset_and_alternate_screens_preserve_records_but_ris_clears_them() {
    run_local(async {
        let mut pane = Pane::start(
            &[
                fenced(
                    reports(&["state=working:app=generic", "id=child:state=done"]),
                    0,
                ),
                fenced(b"\x1b[?1049h\x1b[!p".to_vec(), 1),
                fenced(b"\x1b[?1049l".to_vec(), 2),
                b"\x1bc".to_vec(),
            ],
            false,
        )
        .await;
        pane.stage(0).await;
        let root = pane.record("").await;
        let child = pane.record("child").await;
        for stage in 1..=2 {
            pane.stage(stage).await;
            pane.assert_ids(&["", "child"]).await;
            assert_eq!(pane.record("").await, root);
            assert_eq!(pane.record("child").await, child);
        }
        let mut watcher = pane.watcher(RESOURCE_PROGRAM_STATUS_KEY).await;
        pane.release(3).await;
        assert_eq!(
            changed(&mut watcher, &pane.terminal, RESOURCE_PROGRAM_STATUS_KEY).await,
            None
        );
        assert_eq!(pane.get(RESOURCE_PROGRAM_STATUS_KEY).await, None);
        assert_eq!(pane.get(&record_key("")).await, None);
        assert_eq!(pane.get(&record_key("child")).await, None);
        drop(watcher);
        pane.stop().await;
    });
}

#[test]
fn a_retained_terminal_expires_work_at_exit_and_keeps_done_and_error_records() {
    run_local(async {
        let mut pane = Pane::start(
            &[fenced(
                reports(&[
                    "id=running:state=working:app=generic",
                    "id=waiting:state=blocked:kind=question",
                    "id=success:state=done:msg=UmVhZHk=",
                    "id=failure:state=error:msg=RmFpbGVk",
                ]),
                0,
            )],
            true,
        )
        .await;
        pane.stage(0).await;
        let success = pane.record("success").await;
        let failure = pane.record("failure").await;
        std::fs::write(pane.tmp.path().join("exit"), b"exit").unwrap();
        timeout(WIRE_RECV_TIMEOUT, async {
            loop {
                let request_id = pane.next_request();
                let result = command(
                    &mut pane.stream,
                    request_id,
                    Command::GetState {
                        scope: StateScope::Server,
                    },
                )
                .await;
                let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                    panic!("GET_STATE failed: {result:?}");
                };
                if snapshot.resources.iter().any(|resource| {
                    resource.id == pane.terminal && resource.lifecycle == ResourceLifecycle::Exited
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pane must remain in inventory after exit");
        pane.wait_value(&record_key("running"), Option::is_none)
            .await;
        assert_eq!(pane.get(&record_key("waiting")).await, None);
        assert_eq!(pane.record("success").await, success);
        assert_eq!(pane.record("failure").await, failure);
        assert_eq!(pane.summary().await["active"], failure);
        pane.stop().await;
    });
}
