//! The native Window menu and its palette destination.

use super::*;

#[gpui::test]
fn native_window_menu_and_palette_share_the_overview_command(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let fixture = fixture(cx);
    let menus = cx.update(|cx| cx.get_menus()).expect("published menus");
    let window = menus
        .iter()
        .find(|menu| menu.name.as_ref() == "Window")
        .expect("a Window menu AppKit appends open windows to");
    let actions: Vec<(String, String)> = window
        .items
        .iter()
        .filter_map(|item| match item {
            gpui::OwnedMenuItem::Action { name, action, .. } => Some((
                name.clone(),
                action
                    .as_any()
                    .downcast_ref::<crate::app::actions::Run>()?
                    .id
                    .clone(),
            )),
            _ => None,
        })
        .collect();
    assert!(actions.contains(&("Show All Windows…".into(), "windows".into())));
    assert!(actions.contains(&("Minimize".into(), "minimize".into())));
    let (first_handle, first) = fixture.first.clone();
    let cx = &mut VisualTestContext::from_window(first_handle, cx);
    cx.dispatch_action(crate::app::actions::Run::new("windows"));
    render(cx);
    assert_eq!(
        first.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::Windows
    );
    assert!(cx.debug_bounds("palette-windows-title").is_some());
    // The same command closes it again.
    cx.update(|window, cx| first.update(cx, |desk, cx| desk.execute("windows", window, cx)));
    render(cx);
    assert_eq!(
        first.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::None
    );
}
