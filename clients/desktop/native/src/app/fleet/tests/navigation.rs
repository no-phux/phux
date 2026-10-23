//! Later user navigation must beat a retained fleet choice.
use super::*;

#[gpui::test]
fn explicit_commands_and_tab_selection_cancel_the_fleet_choice(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    handle
        .update(cx, |_, window, cx| {
            desk.update(cx, |desk, cx| {
                install(
                    desk,
                    with_agent(
                        connection(1, vec![session(7, "main")]),
                        9,
                        AgentState::Working,
                    ),
                );
                let target = desk.agent_catalog()[0].clone();
                desk.fleet_view.pending = Some(target.clone());
                desk.execute("sidebar", window, cx);
                assert!(desk.fleet_view.pending.is_none(), "a later command wins");
                desk.fleet_view.pending = Some(target);
                desk.select_tab("another-tab");
                assert!(
                    desk.fleet_view.pending.is_none(),
                    "pointer tab selection wins"
                );
            });
        })
        .unwrap();
}

#[gpui::test]
fn explicit_reveals_supersede_fleet_ownership_even_for_the_same_target(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        install(
            desk,
            with_agent(
                connection(1, vec![session(7, "main")]),
                9,
                AgentState::Working,
            ),
        );
        desk.fleet_view.pending = Some(desk.agent_catalog()[0].clone());
        desk.reveal_terminal(&ResourceId::local(9));
        assert!(
            desk.fleet_view.pending.is_none(),
            "explicit same-target reveal takes ownership"
        );
        desk.fleet_view.pending = Some(desk.agent_catalog()[0].clone());
        desk.reveal_terminal(&ResourceId::local(10));
        assert!(
            desk.fleet_view.pending.is_none(),
            "another target supersedes the choice"
        );
    });
}
