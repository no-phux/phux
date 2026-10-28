//! The session switcher: every machine's sessions, and a new one by name.

use super::*;
use crate::app::Modal;

#[gpui::test]
fn the_switcher_lists_every_machine_and_creates_by_name(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    let _dial = DeterministicDial::arm();
    cx.update(|cx| cx.set_reduce_motion(true));
    let (handle, desk, _) = launch(cx);
    let cx = &mut VisualTestContext::from_window(*handle, cx);
    desk.update(cx, |desk, cx| {
        desk.prefs.sidebar_visible = true;
        install(
            desk,
            connection(1, vec![session(7, "argo"), session(9, "bravo")]),
        );
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "argo")]),
        );
        cx.notify();
    });
    draw(cx);
    desk.update_in(cx, |desk, window, cx| {
        desk.execute("switch-session", window, cx)
    });
    draw(cx);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::Sessions
    );
    assert!(
        shown(cx, "session-switcher"),
        "drops from the session button"
    );
    for row in [
        "palette-result-session:Local:argo",
        "palette-result-session:Local:bravo",
        "palette-result-session:mini:argo",
    ] {
        assert!(shown(cx, row), "{row}");
    }
    let query = |cx: &mut VisualTestContext, text: &'static str| {
        desk.update_in(cx, |desk, window, cx| {
            let state = desk.palette_state.clone().unwrap();
            state.update(cx, |state, cx| state.set_query(text, window, cx));
        });
        draw(cx);
    };
    query(cx, "ARGO");
    assert!(
        !shown(cx, "palette-result-create-session:ARGO"),
        "a name this machine already has is a switch, not a create"
    );
    query(cx, "zeta");
    assert!(shown(cx, "palette-result-create-session:zeta"));
    assert!(!shown(cx, "palette-result-session:Local:bravo"));
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_ne!(
        desk.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::Sessions,
        "enter on the only row creates the session"
    );
}
