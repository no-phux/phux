//! A replaced connection must not replace window-local Settings or phone state.

use super::*;
use crate::launch::Resolved;

#[test]
fn reconnect_keeps_settings_navigation_search_return_context_and_phone_form() {
    let mut desk = Desk::new(&Resolved::offline("settings reconnect")).with_phone(true);
    desk.settings_open = true;
    desk.settings_category(crate::desk::SettingsCategory::Developer);
    desk.settings_ui.query = rune::TextEdit::new("logs");
    desk.settings_ui.query.focused = true;
    desk.settings_ui.return_from_themes = true;
    desk.renew();
    assert!(desk.phone.is_some());
    assert!(desk.settings_open);
    assert_eq!(
        desk.settings_ui.category,
        Some(crate::desk::SettingsCategory::Developer)
    );
    assert_eq!(desk.settings_ui.query.text, "logs");
    assert!(desk.settings_search_focused());
    assert!(desk.settings_ui.return_from_themes);
}

#[test]
fn replacing_a_machine_keeps_phone_preferences_not_old_panes_or_latches() {
    // Embedded launch never locates/spawns an installed CLI on the redial.
    let mut desk = Desk::new(&Resolved::embedded(None)).with_phone(true);
    desk.machines.current = Some("previous".into());
    let phone = desk.phone.as_mut().unwrap();
    phone.keyboard = false;
    phone.symbols = true;
    phone.ctrl = true;
    phone.picker = true;
    phone.tree = Some(phux_client_core::layout::LayoutNode::Leaf(
        phux_protocol::ids::ResourceId::local(1),
    ));
    desk.switch_machine(None);
    assert!(desk.machines.current.is_none());
    let phone = desk
        .phone
        .as_ref()
        .expect("machine replacement stays a phone");
    assert!(!phone.keyboard);
    assert!(phone.symbols);
    assert!(!phone.ctrl);
    assert!(!phone.picker);
    assert!(phone.tree.is_none());
    assert!(!phone.navigating());
}
