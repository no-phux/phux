//! A passive host's inventory invalidates an already-open palette's search.

use super::*;
use gpui::VisualTestContext;

fn update_passive_metadata(desk: &mut Desk, host: &Option<String>, name: &str, cwd: &str) {
    let connection = desk
        .fleet
        .observers
        .get_mut(host)
        .unwrap()
        .connection
        .as_mut()
        .unwrap();
    connection
        .agents
        .get_mut(&ResourceId::local(42))
        .unwrap()
        .name = name.into();
    connection.topology.as_mut().unwrap().panes[0].cwd = Some(cwd.into());
    let revision = desk.fleet.revision;
    assert!(
        desk.fleet.drain(host),
        "changed inventory must be published"
    );
    assert_ne!(desk.fleet.revision, revision);
}

#[gpui::test]
fn passive_metadata_drain_refreshes_fixed_host_agent_and_path_queries(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    cx.update(|cx| cx.set_reduce_motion(true));
    let (handle, desk, _) = launch(cx);
    let cx = &mut VisualTestContext::from_window(*handle, cx);
    desk.update(cx, |desk, _| {
        install(desk, connection(1, vec![session(7, "local")]));
        observe(
            &mut desk.fleet,
            Some("mini"),
            with_agent(
                connection(2, vec![session(8, "remote")]),
                42,
                AgentState::Working,
            ),
        );
    });
    desk.update_in(cx, |desk, window, cx| desk.execute("goto", window, cx));
    draw(cx);
    let host = Some("mini".to_owned());
    for (query, before, after) in [
        ("mini old-agent", 1, 0),
        ("mini /old-directory", 1, 0),
        ("mini new-agent", 0, 1),
        ("mini /new-directory", 0, 1),
    ] {
        desk.update_in(cx, |desk, window, cx| {
            update_passive_metadata(desk, &host, "old-agent", "/old-directory");
            let palette = desk.palette_state.clone().unwrap();
            palette.update(cx, |state, cx| state.set_query(query, window, cx));
            cx.notify();
        });
        draw(cx);
        let palette = desk.read_with(cx, |desk, _| desk.palette_state.clone().unwrap());
        assert_eq!(
            palette.read_with(cx, |state, _| state.matched_count()),
            before,
            "{query}"
        );
        let active_revision = desk.read_with(cx, |desk, _| {
            desk.link.as_ref().unwrap().connection.revision()
        });
        desk.update(cx, |desk, cx| {
            update_passive_metadata(desk, &host, "new-agent", "/new-directory");
            cx.notify();
        });
        draw(cx);
        palette.read_with(cx, |state, cx| {
            assert_eq!(state.query(cx), query, "the query did not change");
            assert_eq!(state.matched_count(), after, "{query}");
        });
        assert_eq!(
            shown(cx, "palette-result-session:mini:remote"),
            after == 1,
            "{query}"
        );
        desk.read_with(cx, |desk, _| {
            assert!(desk.remote.is_none(), "the updated host stayed passive");
            assert_eq!(
                desk.link.as_ref().unwrap().connection.revision(),
                active_revision,
                "only the passive fleet revision can invalidate this search"
            );
        });
    }
}
