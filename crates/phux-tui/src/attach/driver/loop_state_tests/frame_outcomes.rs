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

/// (case, outcome, repaint level, drain chrome when asserted, folded-state check).
type FoldCase<'a> = (&'a str, FrameOutcome, RepaintLevel, Option<bool>, &'a dyn Fn(&SessionLoop) -> bool);

/// One outcome on a fresh loop raises the fleet and the paint its row names.
/// Foreign state waits for the drain (which then owes the strip a refresh);
/// unchanged painter inputs owe no paint; a replaced layout repaints fully.
/// `kept` checks the folded state kept what the frame carried.
#[tokio::test(flavor = "current_thread")]
async fn fold_raises_the_fleet_and_the_paint_each_outcome_owes() {
    let peer = ResourceId::local(9);
    let peer_session = SessionId::new(2);
    let layout = ArrangementDoc::single(peer.clone()).encode_cbor().unwrap();
    let agent = FrameOutcome {
        foreign_agent: Some((peer.clone(), Some(working("peer").encode()))),
        ..FrameOutcome::default()
    };
    let foreign_layout = FrameOutcome {
        foreign_layout: Some((peer_session, Some(layout))),
        ..FrameOutcome::default()
    };
    let chrome = FrameOutcome {
        chrome_dirty: true,
        ..FrameOutcome::default()
    };
    let replaced = FrameOutcome {
        layout_replaced: true,
        ..FrameOutcome::default()
    };
    let agent_kept = |s: &SessionLoop| s.peers.foreign_agents[&ResourceId::local(9)].name == "peer";
    let layout_kept = |s: &SessionLoop| s.peers.foreign_layouts.contains_key(&SessionId::new(2));
    let anything = |_: &SessionLoop| true;
    #[rustfmt::skip] // a case table, one row per case
    let cases: [FoldCase<'_>; 4] = [
        ("foreign agent record", agent, RepaintLevel::None, Some(true), &agent_kept),
        ("foreign layout", foreign_layout, RepaintLevel::None, Some(true), &layout_kept),
        ("chrome dirty, painters unchanged", chrome, RepaintLevel::None, Some(false), &anything),
        ("replaced layout", replaced, RepaintLevel::Full, None, &anything),
    ];
    for (case, outcome, level, drain_chrome, kept) in cases {
        let (repaint, state) = fold(outcome).await;
        assert!(repaint.fleet_dirty, "{case}");
        assert_eq!(repaint.level, level, "{case}");
        if let Some(drain_chrome) = drain_chrome {
            assert_eq!(state.peers.chrome_dirty, drain_chrome, "{case}: drain chrome");
        }
        assert!(kept(&state), "{case}: folded state");
    }
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

