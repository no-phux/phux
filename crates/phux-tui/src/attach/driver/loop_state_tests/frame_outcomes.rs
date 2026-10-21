/// Fold `outcome` into `state` with no pre-frame geometry; returns the raised
/// repaint.
async fn fold_into(
    state: &mut SessionLoop,
    client: &mut Connection,
    outcome: FrameOutcome,
) -> RepaintAccumulator {
    state.peers.chrome_dirty = false;
    let mut repaint = RepaintAccumulator::default();
    state
        .fold_frame_outcome(client, &mut Vec::new(), None, outcome, None, &mut repaint)
        .await
        .unwrap();
    repaint
}

/// Fold one synthetic outcome into a fresh bootstrapped loop; returns the
/// raised repaint and the loop, whose `peers.chrome_dirty` says whether the
/// drain owes a peer-chrome refresh.
fn fold(
    outcome: FrameOutcome,
) -> std::pin::Pin<Box<impl Future<Output = (RepaintAccumulator, SessionLoop)>>> {
    Box::pin(async move {
        let (mut state, mut client, _server, _) =
            bootstrapped_loop_with(ServerFeatureSet::new()).await;
        let repaint = fold_into(&mut state, &mut client, outcome).await;
        (repaint, state)
    })
}

fn working(name: &str) -> AgentRecord {
    AgentRecord {
        name: name.to_owned(),
        state: phux_client::agent_meta::AgentMetaState::Working,
        ..AgentRecord::default()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fold_of_an_empty_outcome_raises_nothing() {
    let (repaint, state) = fold(FrameOutcome::default()).await;
    assert_eq!(repaint, RepaintAccumulator::default());
    assert!(!state.peers.chrome_dirty);
}

#[tokio::test(flavor = "current_thread")]
async fn fold_of_a_foreign_agent_record_raises_the_fleet_and_the_drain_chrome() {
    let peer = ResourceId::local(9);
    let (repaint, state) = fold(FrameOutcome {
        foreign_agent: Some((peer.clone(), Some(working("peer").encode()))),
        ..FrameOutcome::default()
    })
    .await;
    assert!(repaint.fleet_dirty);
    assert_eq!(
        repaint.level,
        RepaintLevel::None,
        "chrome waits for the drain"
    );
    assert!(
        state.peers.chrome_dirty,
        "the drain owes the strip a refresh"
    );
    assert_eq!(state.peers.foreign_agents[&peer].name, "peer");
}

#[tokio::test(flavor = "current_thread")]
async fn fold_of_a_foreign_layout_raises_the_fleet_and_the_drain_chrome() {
    let peer_session = SessionId::new(2);
    let layout = ArrangementDoc::single(ResourceId::local(9))
        .encode_cbor()
        .unwrap();
    let (repaint, state) = fold(FrameOutcome {
        foreign_layout: Some((peer_session, Some(layout))),
        ..FrameOutcome::default()
    })
    .await;
    assert!(repaint.fleet_dirty);
    assert_eq!(
        repaint.level,
        RepaintLevel::None,
        "chrome waits for the drain"
    );
    assert!(state.peers.chrome_dirty);
    assert!(state.peers.foreign_layouts.contains_key(&peer_session));
}

#[tokio::test(flavor = "current_thread")]
async fn fold_of_a_new_foreign_ask_raises_but_a_repeat_does_not() {
    let (mut state, mut client, _server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let asked = || FrameOutcome {
        foreign_attention: Some(ResourceId::local(9)),
        ..FrameOutcome::default()
    };
    let first = fold_into(&mut state, &mut client, asked()).await;
    assert!(first.fleet_dirty && state.peers.chrome_dirty);
    assert_eq!(first.level, RepaintLevel::None);

    let repeat = fold_into(&mut state, &mut client, asked()).await;
    assert_eq!(
        repeat,
        RepaintAccumulator::default(),
        "a repeated ask is not news"
    );
    assert!(!state.peers.chrome_dirty);
}

#[tokio::test(flavor = "current_thread")]
async fn fold_of_chrome_dirty_with_unchanged_painters_raises_only_the_fleet() {
    let (repaint, state) = fold(FrameOutcome {
        chrome_dirty: true,
        ..FrameOutcome::default()
    })
    .await;
    assert!(repaint.fleet_dirty);
    assert_eq!(
        repaint.level,
        RepaintLevel::None,
        "unchanged painter inputs owe no paint"
    );
    assert!(!state.peers.chrome_dirty);
}

#[tokio::test(flavor = "current_thread")]
async fn fold_of_a_moved_local_agent_record_raises_chrome_in_place_and_the_fleet() {
    let (mut state, mut client, _server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let pane = ResourceId::local(1);
    state
        .mirror
        .agent_meta
        .records
        .insert(pane.clone(), working("builder"));
    let repaint = fold_into(
        &mut state,
        &mut client,
        FrameOutcome {
            agent_meta_changed: true,
            agent_meta_terminal: Some(pane),
            ..FrameOutcome::default()
        },
    )
    .await;
    assert!(repaint.fleet_dirty);
    assert_eq!(repaint.level, RepaintLevel::Chrome);
}

#[tokio::test(flavor = "current_thread")]
async fn fold_of_a_replaced_layout_raises_a_full_repaint_and_the_fleet() {
    let (repaint, _) = fold(FrameOutcome {
        layout_replaced: true,
        ..FrameOutcome::default()
    })
    .await;
    assert!(repaint.fleet_dirty);
    assert_eq!(repaint.level, RepaintLevel::Full);
}
