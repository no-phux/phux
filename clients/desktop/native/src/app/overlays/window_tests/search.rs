//! Cross-host work search and live metadata invalidation.

use super::*;

fn search_targets(desk: &Entity<Desk>, query: &str, cx: &mut TestAppContext) -> Vec<String> {
    desk.update(cx, |desk, _| {
        prepare_goto(desk, query);
        desk.goto_search
            .ranked()
            .iter()
            .flat_map(|section| &section.rows)
            .map(|row| match &row.target {
                Target::Work(destination) => {
                    format!("work:{}:{}", destination.host_label(), destination.name)
                }
                Target::Pane(id) => format!("pane:{id}"),
                _ => panic!("unexpected search target"),
            })
            .collect()
    })
}

#[gpui::test]
fn goto_combines_host_with_pane_metadata_in_either_token_order(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let desk = look_alike_desk(cx);
    desk.update(cx, |desk, _| {
        desk.remote = Some("mini".into());
        desk.link
            .as_mut()
            .unwrap()
            .connection
            .agents
            .get_mut(&ResourceId::local(2))
            .unwrap()
            .kind = Some("codex".into());
    });
    for query in [
        "mini claude",
        "claude mini",
        "mini working",
        "working mini",
        "mini codex",
        "codex mini",
    ] {
        let targets = search_targets(&desk, query, cx);
        assert!(
            targets.contains(&"work:mini:api".into()),
            "{query}: {targets:?}"
        );
        assert!(targets.contains(&"pane:@2".into()), "{query}: {targets:?}");
        assert_eq!(targets.len(), 2, "{query}: no unrelated session or pane");
    }
    for query in ["mini /repo", "/repo mini"] {
        let targets = search_targets(&desk, query, cx);
        assert_eq!(targets.len(), 5, "{query}: work and its four panes");
        assert!(targets.contains(&"work:mini:api".into()));
        assert!(targets.contains(&"pane:@4".into()));
    }
    assert_eq!(
        search_targets(&desk, "mini zsh", cx).len(),
        5,
        "pane labels reach the work row too"
    );
}

#[gpui::test]
fn goto_rebuilds_live_work_metadata_without_changing_the_query(cx: &mut TestAppContext) {
    use phux_protocol::wire::frame::FrameKind;

    let _dial = DeterministicDial::arm();
    let desk = look_alike_desk(cx);
    desk.update(cx, |desk, _| desk.remote = Some("mini".into()));
    assert!(search_targets(&desk, "mini fresh-folder", cx).is_empty());
    desk.update(cx, |desk, _| {
        let connection = &mut desk.link.as_mut().unwrap().connection;
        let pane = &mut connection.topology.as_mut().unwrap().panes[0];
        pane.cwd = Some("/fresh-folder".into());
        // An actual connection drain is the cache's production invalidator.
        connection
            .client
            .feed(FrameKind::Ping { nonce: 1 })
            .unwrap();
        connection.drain();
        // This fixture has synthetic inventory, not an attached runtime driver.
        connection.status = Status::Attached;
    });
    let targets = search_targets(&desk, "mini fresh-folder", cx);
    assert!(targets.contains(&"work:mini:api".into()), "{targets:?}");
    assert!(targets.contains(&"pane:@1".into()), "{targets:?}");
    assert!(search_targets(&desk, "mini fresh-agent", cx).is_empty());
    desk.update(cx, |desk, _| {
        let connection = &mut desk.link.as_mut().unwrap().connection;
        let agent = connection.agents.get_mut(&ResourceId::local(2)).unwrap();
        agent.name = "fresh-agent".into();
        agent.state = AgentState::Blocked;
        connection.drain();
        connection.status = Status::Attached;
    });
    let targets = search_targets(&desk, "mini fresh-agent", cx);
    assert!(targets.contains(&"work:mini:api".into()), "{targets:?}");
    assert!(targets.contains(&"pane:@2".into()), "{targets:?}");
    assert!(search_targets(&desk, "mini working", cx).is_empty());
    assert_eq!(search_targets(&desk, "mini blocked", cx).len(), 2);
}

#[gpui::test]
fn work_pane_search_metadata_is_bounded_before_indexing(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let desk = look_alike_desk(cx);
    desk.update(cx, |desk, _| {
        desk.link
            .as_mut()
            .unwrap()
            .connection
            .topology
            .as_mut()
            .unwrap()
            .panes[0]
            .cwd = Some("界".repeat(100_000));
        let rows = crate::app::overlays::rows::work_rows(desk);
        let row = rows.iter().find(|row| row.title == "api").unwrap();
        assert!(
            row.keywords.chars().count() <= 512 + row.detail.as_ref().unwrap().chars().count() + 1
        );
    });
}
