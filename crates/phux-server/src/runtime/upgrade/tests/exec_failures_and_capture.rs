#![allow(clippy::unwrap_used, reason = "tests")]

use super::super::handoff::*;
use super::super::*;
use super::support::*;
use crate::runtime::apps::agent_session::upgrade::request_agent_session_cut;
use phux_resource::agent_session::AgentSessionResource as _;

#[tokio::test(flavor = "current_thread")]
async fn actual_exec_failure_resumes_the_next_append_without_a_barrier() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_, _, append) = state.with_mut(old_image_with_agent_sessions);
            append_records(&append, "{\"type\":\"prompt\"}").await;
            let (_, seal) = capture_blob(&state).await;
            assert!(matches!(
                try_append(&append, "{\"type\":\"stop\"}").await,
                Err(crate::resource::agent_session::AppendRejection::Overflow(_))
            ));
            let (plan, _listener, _) = unexecutable_plan(seal);
            assert_eq!(
                plan.exec().await.kind(),
                std::io::ErrorKind::PermissionDenied
            );
            let next = append_records(&append, "{\"type\":\"stop\"}").await;
            assert_eq!(next.first_seq, 2, "failed appends consumed no sequence");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_prepare_failure_resumes_the_next_append_without_a_barrier() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_, _, append) = state.with_mut(old_image_with_agent_sessions);
            let next = append_records(&append, "{\"type\":\"prompt\"}").await;
            assert_eq!(next.last_seq, 1);
            let listener = tempfile::tempfile().unwrap();
            let dir = tempfile::tempdir().unwrap();
            state.with_mut(|s| {
                s.set_upgrade_context(
                    listener.as_raw_fd(),
                    dir.path().join("phux.sock"),
                    RuntimeFlags {
                        upgrade_source_exe: Some(dir.path().join("missing-replacement")),
                        ..RuntimeFlags::default()
                    },
                );
            });
            let Err(UpgradeError::Io(error)) = prepare_upgrade(&state).await else {
                panic!("the replacement does not exist");
            };
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            let next = append_records(&append, "{\"type\":\"stop\"}").await;
            assert_eq!(next.first_seq, 2);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn upgrade_carries_agent_sessions_with_their_id_and_log_tail() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(Box::pin(async {
            let state = SharedState::new();
            let (pane_wire, agent_wire, append) = state.with_mut(old_image_with_agent_sessions);
            append_records(
                &append,
                "{\"type\":\"session_start\"}\n\
                 {\"type\":\"prompt\",\"data\":{\"marker\":\"before-upgrade\"}}",
            )
            .await;
            let (blob, _seal) = capture_blob(&state).await;

            assert_eq!(
                blob.agent_sessions.len(),
                1,
                "the retained pane's is dropped"
            );
            let carried = &blob.agent_sessions[0];
            assert_eq!(Some(carried.wire_id), agent_wire.local_id());
            assert_eq!(Some(carried.parent_wire_id), pane_wire.local_id());
            assert_eq!(carried.provider, "claude");
            assert_eq!(carried.native_id.as_deref(), Some("native-1"));
            assert_eq!(carried.state.as_deref(), Some("working"));
            assert_eq!((carried.base_seq, carried.dropped), (2, 0));
            assert!(!carried.ended);
            assert_eq!(carried.records.len(), 2);
            assert!(carried.records[1].contains("before-upgrade"));

            // What crosses the exec is the serialized blob.
            let blob = StateBlob::from_bytes(&blob.to_bytes().unwrap()).unwrap();
            let resumed = SharedState::new();
            let watchers = resumed
                .with_mut(|s| {
                    s.rebuild_from_blob(
                        &blob,
                        crate::runtime::apps::terminal::upgrade::unwired,
                        crate::runtime::apps::rebuild_session,
                    )
                })
                .expect("rebuild")
                .exit_watchers;
            assert_eq!(
                watchers.len(),
                blob.panes.len() + 1,
                "every pane and the carried session get an exit watcher"
            );

            let (session, handle, parent) = resumed.with(|s| {
                let session = s.resource_from_wire(&agent_wire).expect("same `@N`");
                let handle = s.resource_handle(session).expect("engine").clone();
                let parent = s.resource_from_wire(&pane_wire).expect("pane");
                (session, handle, parent)
            });
            assert_eq!(handle.parent, Some(parent), "rebound to its rebuilt pane");
            let facet = resumed
                .with(|s| s.registry().resource(session).and_then(|r| r.agent.clone()))
                .expect("registered as an agent session");
            assert_eq!(
                (
                    facet.provider.as_str(),
                    facet.native_id.as_deref(),
                    facet.state.as_deref()
                ),
                ("claude", Some("native-1"), Some("working"))
            );

            let engine = handle.agent_session().unwrap();
            let cut = request_agent_session_cut(&engine.bootstrap, false)
                .await
                .expect("the resumed engine answers");
            assert_eq!(cut.base_seq, 2);
            assert_eq!(
                cut.records
                    .iter()
                    .map(|r| String::from_utf8_lossy(r).into_owned())
                    .collect::<Vec<_>>(),
                carried.records,
                "the log tail survived byte for byte"
            );
            let next = append_records(&engine.append, "{\"type\":\"stop\"}").await;
            assert_eq!(next.first_seq, 3, "the stream continues where it left off");
        }))
        .await;
}

#[tokio::test(start_paused = true)]
async fn pane_handoff_collection_aborts_at_its_aggregate_deadline() {
    let (upgrade, _receiver) = mpsc::channel(1);

    let result = collect_pane_handoffs(
        vec![(phux_core::ids::ResourceId::default(), upgrade)],
        Duration::from_secs(2),
    )
    .await;

    assert!(matches!(result, Err(UpgradeError::HandoffDeadline)));
}

#[tokio::test(start_paused = true)]
async fn pane_handoffs_are_collected_concurrently() {
    let mut handles = Vec::new();
    for _ in 0..2 {
        let (sender, mut receiver) = mpsc::channel::<UpgradeHandleRequest>(1);
        tokio::spawn(async move {
            let request = receiver.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            let _ = request.reply.send(no_pty_handoff());
        });
        handles.push((phux_core::ids::ResourceId::default(), sender));
    }

    let handoffs = collect_pane_handoffs(handles, Duration::from_secs(2))
        .await
        .expect("two 1.5s actors fit in one 2s window only when concurrent");
    assert_eq!(handoffs.len(), 1, "the duplicate fixture pane id coalesces");
}

#[tokio::test]
async fn pane_handoff_rejects_a_half_present_pty_pair() {
    let pane = phux_core::ids::ResourceId::default();
    let (sender, mut receiver) = mpsc::channel::<UpgradeHandleRequest>(1);
    tokio::spawn(async move {
        let request = receiver.recv().await.unwrap();
        let mut handoff = no_pty_handoff();
        handoff.child_pid = Some(42);
        let _ = request.reply.send(handoff);
    });

    let result = collect_pane_handoffs(vec![(pane, sender)], Duration::from_secs(1)).await;
    assert!(matches!(
        result,
        Err(UpgradeError::InvalidPaneHandoff { pane: found }) if found == pane
    ));
}
