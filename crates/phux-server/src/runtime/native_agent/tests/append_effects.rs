use super::*;
use crate::resource::ControlRequest;

fn delayed_parent(fx: &Fixture, capacity: usize) -> mpsc::Receiver<ControlRequest> {
    let (control, receive) = mpsc::channel(capacity);
    fx.state.with_mut(|s| {
        let mut handle = s.resource_handle(fx.parent).unwrap().clone();
        handle.control = control;
        s.register_resource_handle(fx.parent, handle, fx.root.child_token());
    });
    receive
}
fn question_publication(fx: &Fixture, generation: u64) -> tokio::task::JoinHandle<CommandResult> {
    let ctx = fx.context(fx.host);
    let agent = fx.agent.clone();
    tokio::task::spawn_local(async move {
        publish(
            &ctx,
            agent,
            "activation-A".into(),
            "private-test-proof".into(),
            generation,
            2,
            NativeUpdate::QuestionOpened {
                question: question("input"),
            },
        )
        .await
    })
}
fn assert_not_asked(fx: &Fixture) {
    assert!(!fx.state.with(|s| s.agent_is_asked(fx.parent)));
    fx.state.with(|s| {
        let binding = &s.native_agents().bindings[&fx.agent];
        assert!(binding.closed);
        assert!(!binding.view.connected);
        assert!(binding.view.questions.is_empty());
        let core = s.terminal_from_wire(&fx.agent).unwrap();
        assert_eq!(
            s.registry()
                .resource(core)
                .unwrap()
                .agent
                .as_ref()
                .unwrap()
                .state,
            None
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_parent_reply_cannot_reopen_native_ask_inventory_after_closure() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            let mut parent = delayed_parent(&fx, 4);
            let publication = question_publication(&fx, generation);
            let ControlRequest::ReportStreamState {
                state,
                reply: old_reply,
            } = parent.recv().await.unwrap()
            else {
                panic!("expected stream evidence");
            };
            assert_eq!(
                state,
                Some(phux_protocol::wire::frame::ReportedAgentState::Blocked)
            );
            let close_state = fx.state.clone();
            let resource = fx.parent;
            let close = tokio::task::spawn_local(async move {
                source_exited(&close_state, resource).await;
            });
            let ControlRequest::ReportStreamState { state, reply } = parent.recv().await.unwrap()
            else {
                panic!("expected closure evidence");
            };
            assert_eq!(state, None);
            reply.send(Ok(())).unwrap();
            close.await.unwrap();
            assert_not_asked(&fx);
            old_reply.send(Ok(())).unwrap();
            decode::<NativePublicationReceipt>(publication.await.unwrap());
            assert_not_asked(&fx);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn mailbox_capacity_wait_rechecks_native_closure_before_enqueuing_evidence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            let mut parent = delayed_parent(&fx, 1);
            let sender = fx
                .state
                .with(|s| s.resource_handle(fx.parent).unwrap().control.clone());
            let (reply, _held) = oneshot::channel();
            sender
                .try_send(ControlRequest::ReportStreamState { state: None, reply })
                .unwrap();
            let publication = question_publication(&fx, generation);
            // Actor acceptance is the barrier. The parent mailbox is still full,
            // so this continuation cannot yet have enqueued its stream evidence.
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let cut = fx.cut().await;
                    if cut
                        .native_publications
                        .iter()
                        .any(|accepted| accepted.identity.id == 2)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            fx.state.with_mut(|s| begin_closure(s, &fx.agent));
            sync_ask_inventory(&fx.state, &fx.agent);
            drop(parent.recv().await.unwrap());
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), publication)
                .await
                .unwrap()
                .unwrap();
            decode::<NativePublicationReceipt>(result);
            assert!(matches!(
                parent.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            assert_not_asked(&fx);
        })
        .await;
}
