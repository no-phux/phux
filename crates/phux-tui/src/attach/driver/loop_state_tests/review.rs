fn wide_sidebar() -> SidebarReservation {
    SidebarReservation {
        edge: crate::attach::paint::SidebarEdge::Left,
        width: 32,
    }
}

fn done_reviewer() -> AgentRecord {
    AgentRecord {
        name: "reviewer".into(),
        state: phux_client::agent_meta::AgentMetaState::Done,
        ..AgentRecord::default()
    }
}

fn seed_local_agent(state: &mut SessionLoop, id: &ResourceId, record: &AgentRecord) {
    if !state.mirror.workspace.groups.iter().any(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| crate::layout::leaves(tree).contains(id))
    }) {
        state
            .mirror
            .workspace
            .add_group("agent".to_owned(), id.clone());
    }
    state
        .mirror
        .panes
        .entry(id.clone())
        .or_insert_with(|| PaneSlot::new_with_size(80, 24).unwrap());
    state
        .mirror
        .agent_meta
        .records
        .insert(id.clone(), record.clone());
}

fn agent_line(bytes: &[u8], name: &str) -> String {
    painted_sidebar_text(bytes)
        .lines()
        .find(|line| line.contains(name))
        .unwrap_or("")
        .to_owned()
}

/// A reviewed done needs no one: nothing on the strip keeps the unread
/// diamond, and any row still naming `name` (its tab) quiets to a ring.
fn assert_reviewed_row(bytes: &[u8], name: &str) {
    let screen = painted_sidebar_text(bytes);
    assert!(
        !screen.is_empty() && !screen.contains('◆'),
        "reviewed {name} must not keep the unread diamond (bytes={}):\n{screen}",
        bytes.len()
    );
    for line in screen.lines().filter(|line| line.contains(name)) {
        assert!(
            line.contains('○'),
            "reviewed {name} must quiet to a ring: {line}\n{screen}"
        );
    }
}

fn assert_unread_done_row(bytes: &[u8], name: &str) {
    let line = agent_line(bytes, name);
    assert!(line.contains(name), "missing {name}: {line}");
    assert!(
        line.contains('◆'),
        "unreviewed done {name} must keep the diamond: {line}"
    );
}

async fn loop_wide() -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let (mut state, client, server, out) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state.viewport_dims = (100, 24);
    (state, client, server, out)
}

async fn intercept_agent_get(
    state: &mut SessionLoop,
    client: &mut Connection,
    request_id: u32,
    id: &ResourceId,
    record: &AgentRecord,
) -> RepaintAccumulator {
    let mut repaint = RepaintAccumulator::default();
    state
        .peers
        .foreign_agent_pending
        .insert(request_id, id.clone());
    state
        .intercept_peer_reply(
            client,
            FrameKind::MetadataValue {
                request_id,
                value: Some(record.encode()),
            },
            &mut repaint,
        )
        .await
        .unwrap();
    repaint
}

fn agent_broadcast(id: &ResourceId, record: &AgentRecord) -> FrameKind {
    FrameKind::MetadataChanged {
        scope: Scope::Resource(id.clone()),
        key: phux_client::agent_meta::RESOURCE_AGENT_KEY.to_owned(),
        value: Some(record.encode()),
        actor: None,
    }
}

/// Push the current projection into the painter and emit a chrome paint.
/// Change detection is not the point: the tests need the glyphs the closed
/// fleet would show, even when an identical GET correctly declined to dirty.
fn snapshot_sidebar(state: &mut SessionLoop, out: &mut Vec<u8>, sidebar: SidebarReservation) {
    out.clear();
    let _ = state.refresh_chrome();
    state.sidebar_painter.invalidate();
    let mut repaint = RepaintAccumulator::default();
    repaint.raise_chrome();
    state.drain_repaint(out, Some(sidebar), &mut repaint);
}

/// A→B→A keeps an unchanged reviewed Done in the local queue,
/// the peer queue, and the roster, with the fleet closed.
#[tokio::test(flavor = "current_thread")]
async fn a_reviewed_done_survives_rebuild_in_peer_and_local_chrome() {
    let done_id = ResourceId::local(10);
    let done = done_reviewer();
    let sidebar = wide_sidebar();

    let (mut session_a, _, _, _) = loop_wide().await;
    seed_local_agent(&mut session_a, &done_id, &done);
    session_a.mirror.focused_resource = Some(done_id.clone());
    assert!(
        session_a
            .review
            .observe_record(&done_id, Some(&done), Some(&done_id))
    );
    assert!(session_a.review.is_seen(&done_id));

    let (mut session_b, mut client, _server, mut out) = loop_wide().await;
    session_b.set_review(std::mem::take(&mut session_a.review));
    seed_cached_peer(&mut session_b, &done_id);
    assert_eq!(
        session_b.mirror.focused_resource.as_ref(),
        Some(&ResourceId::local(1))
    );

    let _ = intercept_agent_get(&mut session_b, &mut client, 900, &done_id, &done).await;
    assert!(session_b.peers.foreign_agents.contains_key(&done_id));
    assert!(session_b.peers.chrome_dirty);
    assert!(session_b.review.is_seen(&done_id));
    snapshot_sidebar(&mut session_b, &mut out, sidebar);
    assert_reviewed_row(&out, "reviewer");
    assert_eq!(session_b.sidebar_painter.click_targets().counts.roster, 2);

    let mut repaint = intercept_agent_get(&mut session_b, &mut client, 901, &done_id, &done).await;
    assert!(!session_b.peers.chrome_dirty);
    out.clear();
    session_b.drain_repaint(&mut out, Some(sidebar), &mut repaint);
    assert!(out.is_empty(), "an identical GET must emit no bytes");
    assert!(!session_b.refresh_chrome());
    assert!(session_b.review.is_seen(&done_id));

    let (mut session_a2, mut client, _server_a2, mut out) = loop_wide().await;
    session_a2.set_review(std::mem::take(&mut session_b.review));
    seed_local_agent(&mut session_a2, &done_id, &done);
    snapshot_sidebar(&mut session_a2, &mut out, sidebar);
    assert_reviewed_row(&out, "reviewer");

    session_a2
        .mirror
        .agent_meta
        .pending
        .insert(42, done_id.clone());
    let mut repaint = RepaintAccumulator::default();
    session_a2
        .apply_server_frame(
            &mut client,
            &mut out,
            Some(sidebar),
            FrameKind::MetadataValue {
                request_id: 42,
                value: Some(done.encode()),
            },
            true,
            &mut repaint,
        )
        .await
        .unwrap();
    out.clear();
    session_a2.drain_repaint(&mut out, Some(sidebar), &mut repaint);
    assert!(out.is_empty(), "an identical local GET must emit no bytes");
    assert!(session_a2.review.is_seen(&done_id));
}

/// A genuine change while the pane is unfocused re-arms unread, including
/// after a session-loop rebuild.
#[tokio::test(flavor = "current_thread")]
async fn a_change_while_away_rearms_unread_done() {
    let done_id = ResourceId::local(10);
    let done = done_reviewer();
    let sidebar = wide_sidebar();
    let (mut state, mut client, _server, mut out) = loop_wide().await;
    state
        .review
        .observe_record(&done_id, Some(&done), Some(&done_id));
    seed_cached_peer(&mut state, &done_id);
    let mut repaint = intercept_agent_get(&mut state, &mut client, 900, &done_id, &done).await;
    assert!(state.review.is_seen(&done_id));

    let working = AgentRecord {
        state: phux_client::agent_meta::AgentMetaState::Working,
        ..done.clone()
    };
    out.clear();
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            Some(sidebar),
            agent_broadcast(&done_id, &working),
            true,
            &mut repaint,
        )
        .await
        .unwrap();
    assert!(!state.review.is_seen(&done_id));

    state
        .apply_server_frame(
            &mut client,
            &mut out,
            Some(sidebar),
            agent_broadcast(&done_id, &done_reviewer()),
            true,
            &mut repaint,
        )
        .await
        .unwrap();
    out.clear();
    state.drain_repaint(&mut out, Some(sidebar), &mut repaint);
    assert_unread_done_row(&out, "reviewer");
    assert!(!state.review.is_seen(&done_id));
}

/// Confirmed death forgets the identity; dropping a peer from the live set
/// (a locality change) must not.
#[tokio::test(flavor = "current_thread")]
async fn confirmed_death_clears_review_and_locality_does_not() {
    let done_id = ResourceId::local(10);
    let done = done_reviewer();
    let (mut state, mut client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state
        .review
        .observe_record(&done_id, Some(&done), Some(&done_id));
    seed_cached_peer(&mut state, &done_id);
    state.peers.foreign_agents.insert(done_id.clone(), done);
    state.peers.prune_agents(&std::collections::HashSet::new());
    assert!(
        state.review.is_seen(&done_id),
        "pruning a peer cache is a locality change, not death"
    );

    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::ResourceClosed {
                terminal_id: done_id.clone(),
                exit_status: None,
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            },
            true,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(
        !state.review.is_seen(&done_id),
        "ResourceClosed must forget the identity"
    );
}

/// Kernel stream absence after a loop rebuild is not retraction; a later
/// stream-derived completion still re-arms.
#[tokio::test(flavor = "current_thread")]
async fn stream_completion_invalidates_after_rebuild_without_retracting_on_gap() {
    let pane = ResourceId::local(10);
    let sid = ResourceId::local(99);
    let sidebar = wide_sidebar();
    let (mut session_a, _, _, _) = loop_wide().await;
    seed_local_agent(&mut session_a, &pane, &done_reviewer());
    session_a.review.observe_stream(
        &pane,
        &[(
            sid.clone(),
            phux_client::agent_meta::AgentMetaState::Working,
        )],
        Some(&pane),
    );
    assert!(session_a.review.is_seen(&pane));

    let (mut session_b, _, _, mut out) = loop_wide().await;
    session_b.set_review(std::mem::take(&mut session_a.review));
    seed_local_agent(&mut session_b, &pane, &done_reviewer());
    let _ = session_b.refresh_chrome();
    assert!(
        session_b.review.is_seen(&pane),
        "stream absence after rebuild is not retraction"
    );

    assert!(session_b.review.observe_stream(
        &pane,
        &[(sid, phux_client::agent_meta::AgentMetaState::Done)],
        None
    ));
    assert!(!session_b.review.is_seen(&pane));
    snapshot_sidebar(&mut session_b, &mut out, sidebar);
    // The local agent shows once, as the badge on its `agent` tab.
    assert_unread_done_row(&out, "agent");
}

// ---- the per-frame fold's raise matrix (phux-jx39.5) ------------------------
//
// `fold_frame_outcome` never paints: it raises what the burst drain owes.
// Peer-side changes defer the chrome refresh to the drain (`peers.chrome_dirty`,
// which `drain_repaint` turns into an in-place chrome paint) and raise the
// fleet; local changes raise chrome only when a painter input moved.
